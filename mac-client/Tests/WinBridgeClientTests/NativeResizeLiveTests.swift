import AppKit
import Testing
@testable import WinBridgeCore

// Opt-in harness for driving real macOS border gestures through UI automation.
// It has no connection, credentials or remote input; requests are recorded locally.
@MainActor @Test(.enabled(if: ProcessInfo.processInfo.environment["WINBRIDGE_LIVE_RESIZE_REPORT"] != nil))
func nativeResizeLiveGestures() throws {
    let report = try #require(ProcessInfo.processInfo.environment["WINBRIDGE_LIVE_RESIZE_REPORT"])
    let app = NSApplication.shared
    app.setActivationPolicy(.regular)
    let wc = RemoteWindowController(info: WindowInfo(windowId: 100, title: "WinBridge resize test",
        kind: .normal, width: 640, height: 480, isResizable: true), owner: nil)
    defer { wc.forceCloseLocally() }
    let window = try #require(wc.window)
    window.setFrameOrigin(NSPoint(x: 180, y: 180))
    var requests: [ResizeWindow] = []
    wc.onSendControl = { payload in
        if case .resizeWindow(let size) = payload { requests.append(size) }
    }
    wc.present(owner: nil)
    app.activate(ignoringOtherApps: true)
    let deadline = Date().addingTimeInterval(90)
    while Date() < deadline && !FileManager.default.fileExists(atPath: report + ".done") {
        if let event = app.nextEvent(matching: .any, until: Date().addingTimeInterval(0.02), inMode: .default, dequeue: true) {
            app.sendEvent(event)
        }
        RunLoop.current.run(until: Date().addingTimeInterval(0.005))
        let value: [String: Any] = ["width": window.contentLayoutRect.width, "height": window.contentLayoutRect.height,
            "requests": requests.map { ["width": $0.width, "height": $0.height] }]
        try JSONSerialization.data(withJSONObject: value).write(to: URL(fileURLWithPath: report), options: .atomic)
    }
    #expect(!requests.isEmpty, "Option border gesture must emit a host resize")
}
