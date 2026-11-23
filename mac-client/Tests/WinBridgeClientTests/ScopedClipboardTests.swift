import AppKit
import Testing
@testable import WinBridgeCore

@MainActor private final class ClipboardFixture {
    let board = NSPasteboard(name: .init("WinBridge-tests-" + UUID().uuidString))
    lazy var monitor = ClipboardMonitor(board: board)
    var target: ClipboardTarget? = ClipboardTarget(windowId: 7, token: 101)
    var supported = true
    var requests: [ClipboardRequest] = []
    var statuses: [String] = []
    init() {
        monitor.activeTarget = { [weak self] in self?.target }
        monitor.isSupported = { [weak self] in self?.supported ?? false }
        monitor.onRequest = { [weak self] in self?.requests.append($0) }
        monitor.onStatus = { [weak self] in self?.statuses.append($0) }
    }
    func clean() { monitor.stop(); board.releaseGlobally() }
    func put(_ text: String) { board.clearContents(); board.setString(text, forType: .string) }
    func response(_ req: ClipboardRequest, text: String) -> ClipboardResult {
        ClipboardResult(requestId: req.requestId, windowId: req.windowId, token: req.token,
            action: req.action, success: true, data: ClipboardSync(content: .textUtf8(text)))
    }
}

@MainActor @Test func clipboardReadsLocalContentOnlyForExplicitPaste() throws {
    let f = ClipboardFixture(); defer { f.clean() }
    f.put("before connection")
    f.monitor.start()
    f.monitor.poll()
    let baseline = try #require(f.requests.last)
    #expect(baseline.action == "poll" && baseline.discard && baseline.data == nil)
    f.monitor.receive(f.response(baseline, text: "old remote contents"))
    #expect(f.board.string(forType: .string) == "before connection")
    f.put("fresh paste")
    f.monitor.perform("paste", windowId: 7)
    let paste = try #require(f.requests.last)
    #expect(paste.action == "paste" && paste.token == 101 && paste.windowId == 7)
    #expect(paste.data?.content == .textUtf8("fresh paste"))
    f.monitor.receive(f.response(paste, text: "ignored paste response"))
    #expect(f.board.string(forType: .string) == "fresh paste")
}

@MainActor @Test func clipboardProtectsNewLocalContentAndRejectsStaleWindowResponses() throws {
    let f = ClipboardFixture(); defer { f.clean() }
    f.put("local")
    f.monitor.perform("copy", windowId: 7)
    let request = try #require(f.requests.last)
    f.put("new local copy")
    f.monitor.receive(f.response(request, text: "late remote copy"))
    #expect(f.board.string(forType: .string) == "new local copy")
    f.monitor.perform("copy", windowId: 7)
    let second = try #require(f.requests.last)
    f.target = ClipboardTarget(windowId: 7, token: 202)
    f.monitor.receive(f.response(second, text: "old generation"))
    #expect(f.board.string(forType: .string) == "new local copy")
    f.monitor.perform("copy", windowId: 7)
    let third = try #require(f.requests.last)
    f.monitor.cancel(windowId: 7)
    f.monitor.receive(f.response(third, text: "after local close"))
    #expect(f.board.string(forType: .string) == "new local copy")
}

@MainActor @Test func clipboardStopsOnFocusLossDisableAndUnsupportedHost() {
    let f = ClipboardFixture(); defer { f.clean() }
    f.put("local")
    f.target = nil
    f.monitor.poll(); f.monitor.perform("paste", windowId: 7)
    #expect(f.requests.isEmpty)
    f.target = ClipboardTarget(windowId: 7, token: 101)
    f.supported = false
    f.monitor.poll(); f.monitor.perform("paste", windowId: 7)
    #expect(f.requests.isEmpty)
    f.supported = true; f.monitor.isEnabled = false
    f.monitor.poll(); f.monitor.perform("paste", windowId: 7)
    #expect(f.requests.isEmpty)
}

@MainActor @Test func explicitFileResultsNeverOverwriteNewContentsOrBypassDisabledSync() {
    let f = ClipboardFixture(); defer { f.clean() }
    let urls = [URL(fileURLWithPath: "/tmp/synthetic-file")]
    f.put("local")
    #expect(!f.monitor.applyRemoteFiles(urls, requestId: 1))
    f.monitor.beginFilePublication(1)
    f.put("new local")
    #expect(!f.monitor.applyRemoteFiles(urls, requestId: 1))
    f.monitor.beginFilePublication(2)
    f.monitor.isEnabled = false
    #expect(!f.monitor.applyRemoteFiles(urls, requestId: 2))
    f.monitor.isEnabled = true
    #expect(!f.monitor.applyRemoteFiles(urls, requestId: 2))
    #expect(f.board.string(forType: .string) == "new local")
    f.monitor.beginFilePublication(3)
    #expect(f.monitor.applyRemoteFiles(urls, requestId: 3))
    #expect(f.monitor.applyRemoteFiles(urls + [URL(fileURLWithPath: "/tmp/synthetic-file-2")], requestId: 3))
}

@MainActor @Test func clipboardValidatesPayloadBeforeClearingBoard() throws {
    let f = ClipboardFixture(); defer { f.clean() }
    f.put("keep")
    f.monitor.perform("copy", windowId: 7)
    let request = try #require(f.requests.last)
    var bad = f.response(request, text: "unused")
    bad.data = ClipboardSync(content: .imagePng(Data([1, 2, 3])))
    f.monitor.receive(bad)
    #expect(f.board.string(forType: .string) == "keep")
    #expect(!ClipboardContentValidation.text("bad\0text"))
    #expect(!ClipboardContentValidation.text(String(repeating: "a", count: 1_048_577)))
    f.board.clearContents(); f.board.writeObjects([URL(fileURLWithPath: "/tmp/synthetic") as NSURL])
    let before = f.requests.count
    f.monitor.perform("paste", windowId: 7)
    #expect(f.requests.count == before)
}

@Test func scopedClipboardWireRoundTrips() throws {
    for payload in [ControlMessage.Payload.clipboardRequest(ClipboardRequest(requestId: 42,
        windowId: 7, token: 99, action: "paste", data: ClipboardSync(content: .textUtf8("example")))),
        .clipboardResult(ClipboardResult(requestId: 42, windowId: 7, token: 99, action: "paste", success: true))] {
        let message = ControlMessage(payload: payload)
        #expect(try ControlMessage.decode(from: message.encode()) == message)
    }
}

@MainActor @Test func scopedClipboardPublishesAndPastesValidatedImages() throws {
    let f = ClipboardFixture(); defer { f.clean() }
    let bitmap = try #require(NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: 2, pixelsHigh: 1,
        bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
        colorSpaceName: .deviceRGB, bytesPerRow: 8, bitsPerPixel: 32))
    bitmap.setColor(.red, atX: 0, y: 0)
    bitmap.setColor(.clear, atX: 1, y: 0)
    let png = try #require(bitmap.representation(using: .png, properties: [:]))
    f.put("previous")
    f.monitor.perform("copy", windowId: 7)
    let request = try #require(f.requests.last)
    var response = f.response(request, text: "unused")
    response.data = ClipboardSync(content: .imagePng(png))
    f.monitor.receive(response)
    let received = try #require(f.board.data(forType: .png))
    let image = try #require(NSBitmapImageRep(data: received))
    #expect(image.pixelsWide == 2 && image.pixelsHigh == 1)
    #expect(image.colorAt(x: 1, y: 0)?.alphaComponent == 0)
    #expect(f.board.data(forType: .tiff) != nil)
    f.monitor.perform("paste", windowId: 7)
    guard case .imagePng(let sent) = f.requests.last?.data?.content else {
        Issue.record("image paste was not scoped"); return
    }
    #expect(NSBitmapImageRep(data: sent)?.pixelsWide == 2)
}

@MainActor @Test func unsupportedHostAndUnavailableTargetExplainClipboardFailure() {
    let f = ClipboardFixture(); defer { f.clean() }
    f.put("preserve local")
    // Observed old host mask is 511: it includes popup alpha but lacks bit 9.
    f.supported = (511 & ProtocolCapabilities.scopedClipboard) != 0
    f.monitor.perform("paste", windowId: 7)
    #expect(f.requests.isEmpty)
    #expect(f.statuses.last?.contains("Restart the updated Windows host") == true)
    #expect(f.board.string(forType: .string) == "preserve local")
    f.supported = true
    f.target = nil
    f.monitor.perform("copy", windowId: 7)
    #expect(f.requests.isEmpty)
    #expect(f.statuses.last?.contains("interactive remote window") == true)
}
