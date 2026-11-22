import AppKit
import Foundation
import ImageIO

struct ClipboardTarget: Equatable {
    let windowId: UInt64
    let token: UInt64
}

enum ClipboardContentValidation {
    static let maxPNG = 8 * 1024 * 1024
    static let maxText = 1024 * 1024
    static func png(_ data: Data) -> Data? {
        guard data.count <= 64 * 1024 * 1024,
              let source = CGImageSourceCreateWithData(data as CFData, nil),
              let props = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any],
              let w = (props[kCGImagePropertyPixelWidth] as? NSNumber)?.intValue,
              let h = (props[kCGImagePropertyPixelHeight] as? NSNumber)?.intValue,
              w > 0, h > 0, w <= 16384, h <= 16384, w * h <= 16 * 1024 * 1024,
              let image = CGImageSourceCreateImageAtIndex(source, 0, [kCGImageSourceShouldCache: false] as CFDictionary),
              let png = NSBitmapImageRep(cgImage: image).representation(using: .png, properties: [:]),
              png.count <= maxPNG else { return nil }
        return png
    }
    static func text(_ value: String) -> Bool { value.utf8.count <= maxText && !value.contains("\0") }
}

/// Target-scoped transactions; never watches or exports the local system clipboard.
@MainActor
final class ClipboardMonitor {
    var isEnabled = true { didSet { if oldValue != isEnabled { cancel() } } }
    var isSupported: () -> Bool = { false }
    var activeTarget: () -> ClipboardTarget? = { nil }
    var onRequest: ((ClipboardRequest) -> Void)?
    var onStatus: ((String) -> Void)?
    private let board: NSPasteboard
    private var timer: Timer?
    private var nextID: UInt64 = 0
    private var generation: UInt64 = 0
    private var baseline: ClipboardTarget?
    private struct Pending {
        let request: ClipboardRequest
        let changeCount: Int
        let generation: UInt64
        let deadline: Date
    }
    private var pending: Pending?
    private struct FileTicket {
        var changeCount: Int
        let generation: UInt64
        let deadline: Date
    }
    private var files: [UInt64: FileTicket] = [:]

    init(board: NSPasteboard = .general) { self.board = board }

    func start() {
        stop()
        timer = Timer.scheduledTimer(withTimeInterval: 0.5, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.poll() }
        }
    }
    func stop() { timer?.invalidate(); timer = nil; cancel() }
    func cancel() {
        generation &+= 1
        pending = nil
        baseline = nil
        files.removeAll()
    }
    func cancel(windowId: UInt64) {
        if baseline?.windowId == windowId || pending?.request.windowId == windowId {
            generation &+= 1; pending = nil; baseline = nil
        }
    }
    func poll() {
        guard isEnabled, isSupported(), let target = activeTarget(), target.token != 0 else {
            if baseline != nil || pending != nil { cancel() }
            return
        }
        if (pending?.deadline ?? .distantFuture) < Date() { pending = nil; baseline = nil }
        if let pending, pending.request.windowId == target.windowId && pending.request.token == target.token { return }
        if baseline != target { pending = nil }
        let discard = baseline != target
        baseline = target
        send(action: "poll", target: target, discard: discard, content: nil)
    }
    func perform(_ action: String, windowId: UInt64) {
        guard isEnabled else { onStatus?("剪贴板同步已关闭 / Clipboard is disabled"); return }
        guard isSupported() else { onStatus?("请更新 Windows 主机以使用安全剪贴板 / Update the host for scoped clipboard"); return }
        guard ["copy", "cut", "paste"].contains(action), let target = activeTarget(),
              target.windowId == windowId, target.token != 0 else { return }
        var content: ClipboardSync.Content?
        if action == "paste" {
            guard board.types?.contains(.fileURL) != true else {
                onStatus?("不通过剪贴板传输文件，请显式选择文件 / Select files explicitly"); return
            }
            if let data = board.data(forType: .png) ?? board.data(forType: .tiff) {
                guard let normalized = ClipboardContentValidation.png(data) else {
                    onStatus?("图片无效或超过限制 / Invalid or oversized image"); return
                }
                content = .imagePng(normalized)
            } else if let text = board.string(forType: .string), ClipboardContentValidation.text(text) {
                content = .textUtf8(text)
            } else {
                onStatus?("剪贴板没有可用文本或图片 / No supported clipboard content"); return
            }
        }
        baseline = target
        send(action: action, target: target, discard: false, content: content)
    }
    private func send(action: String, target: ClipboardTarget, discard: Bool, content: ClipboardSync.Content?) {
        nextID &+= 1
        let request = ClipboardRequest(requestId: nextID, windowId: target.windowId, token: target.token,
            action: action, data: content.map { ClipboardSync(content: $0) }, discard: discard)
        pending = Pending(request: request, changeCount: board.changeCount, generation: generation,
            deadline: Date().addingTimeInterval(3))
        onRequest?(request)
    }
    func receive(_ result: ClipboardResult) {
        guard let saved = pending, saved.request.requestId == result.requestId else { return }
        pending = nil
        guard isEnabled, isSupported(), saved.generation == generation, saved.deadline >= Date(),
              result.windowId == saved.request.windowId, result.token == saved.request.token,
              result.action == saved.request.action,
              activeTarget() == ClipboardTarget(windowId: result.windowId, token: result.token) else { return }
        guard result.success else {
            baseline = nil
            if result.action != "poll" { onStatus?(result.error) }
            return
        }
        guard !saved.request.discard, result.action != "paste", let content = result.data?.content else { return }
        guard board.changeCount == saved.changeCount else {
            onStatus?("本地剪贴板已变化，保留新内容 / New local clipboard contents preserved"); return
        }
        let item = NSPasteboardItem()
        switch content {
        case .textUtf8(let text):
            guard ClipboardContentValidation.text(text) else { return }
            item.setString(text, forType: .string)
        case .imagePng(let data):
            guard data.count <= ClipboardContentValidation.maxPNG,
                  let png = ClipboardContentValidation.png(data) else { return }
            item.setData(png, forType: .png)
            if let tiff = NSBitmapImageRep(data: png)?.tiffRepresentation { item.setData(tiff, forType: .tiff) }
        }
        board.clearContents()
        board.writeObjects([item])
    }

    // Explicit file-browser transfers are separate from clipboard polling.
    func beginFilePublication(_ requestId: UInt64) {
        files = files.filter { $0.value.deadline >= Date() }
        guard isEnabled, files.count < 64 else { return }
        files[requestId] = FileTicket(changeCount: board.changeCount, generation: generation,
            deadline: Date().addingTimeInterval(600))
    }
    @discardableResult func applyRemoteFiles(_ urls: [URL], requestId: UInt64) -> Bool {
        guard isEnabled, !urls.isEmpty, var ticket = files[requestId], ticket.generation == generation,
              ticket.deadline >= Date(), ticket.changeCount == board.changeCount else { return false }
        board.clearContents()
        let success = board.writeObjects(urls.map { $0 as NSURL })
        ticket.changeCount = board.changeCount
        files[requestId] = ticket
        return success
    }
}
