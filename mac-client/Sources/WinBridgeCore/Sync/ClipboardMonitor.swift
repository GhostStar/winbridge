import AppKit
import Foundation

/// Bidirectional macOS NSPasteboard <-> WinBridge ClipboardSync monitor with echo suppression.
@MainActor
final class ClipboardMonitor {
    private static let maxWireBytes = 15 * 1024 * 1024

    var isEnabled: Bool = true
    var onLocalChange: ((ClipboardSync) -> Void)?

    private var timer: Timer?
    private var lastChangeCount: Int = NSPasteboard.general.changeCount
    private var localSeq: UInt64 = 1
    private var lastRemoteChangeId: UInt64?
    private var lastContentHash: UInt64?

    func start() {
        stop()
        lastChangeCount = NSPasteboard.general.changeCount
        timer = Timer.scheduledTimer(withTimeInterval: 0.35, repeats: true) { [weak self] _ in
            Task { @MainActor in
                self?.poll()
            }
        }
    }

    func stop() {
        timer?.invalidate()
        timer = nil
    }

    func applyRemote(_ sync: ClipboardSync) {
        guard isEnabled, let content = sync.content else { return }
        if sync.changeId != 0 && sync.changeId == lastRemoteChangeId {
            return
        }
        let hash = hashContent(content)
        if hash == lastContentHash {
            lastRemoteChangeId = sync.changeId
            return
        }

        let pb = NSPasteboard.general
        pb.clearContents()
        switch content {
        case .textUtf8(let text):
            pb.setString(text, forType: .string)
        case .imagePng(let pngData):
            pb.setData(pngData, forType: .png)
            if let img = NSImage(data: pngData), let tiff = img.tiffRepresentation {
                pb.setData(tiff, forType: .tiff)
            }
        }

        lastChangeCount = pb.changeCount
        lastRemoteChangeId = sync.changeId
        lastContentHash = hash
    }

    /// Publishes received file URLs onto the macOS clipboard so the user can press ⌘V
    /// directly in Finder or any app, without triggering an outbound clipboard echo.
    func applyRemoteFiles(_ urls: [URL]) {
        guard !urls.isEmpty else { return }
        let pb = NSPasteboard.general
        pb.clearContents()
        pb.writeObjects(urls.map { $0 as NSURL })
        lastChangeCount = pb.changeCount
        lastContentHash = nil
    }

    private func poll() {
        guard isEnabled else { return }
        let pb = NSPasteboard.general
        let current = pb.changeCount
        guard current != lastChangeCount else { return }
        lastChangeCount = current

        guard let content = readLocalContent(from: pb) else { return }
        let hash = hashContent(content)
        if hash == lastContentHash { return }
        lastContentHash = hash

        let id = localSeq
        localSeq &+= 1
        onLocalChange?(ClipboardSync(changeId: id, content: content))
    }

    private func readLocalContent(from pb: NSPasteboard) -> ClipboardSync.Content? {
        // Prefer PNG image if present (e.g. screenshots), otherwise TIFF -> PNG, otherwise UTF-8 text.
        if let png = pb.data(forType: .png), !png.isEmpty, png.count <= Self.maxWireBytes {
            return .imagePng(png)
        }
        if let tiff = pb.data(forType: .tiff),
           let rep = NSBitmapImageRep(data: tiff),
           let png = rep.representation(using: .png, properties: [:]),
           !png.isEmpty,
           png.count <= Self.maxWireBytes {
            return .imagePng(png)
        }
        if let text = pb.string(forType: .string), text.utf8.count <= Self.maxWireBytes {
            return .textUtf8(text)
        }
        return nil
    }

    private func hashContent(_ content: ClipboardSync.Content) -> UInt64 {
        // 64-bit FNV-1a
        var hash: UInt64 = 14695981039346656037
        func mix(_ bytes: UnsafeRawBufferPointer) {
            for b in bytes {
                hash ^= UInt64(b)
                hash &*= 1099511628211
            }
        }
        switch content {
        case .textUtf8(let s):
            var tag: UInt8 = 1
            withUnsafeBytes(of: &tag, mix)
            Data(s.utf8).withUnsafeBytes(mix)
        case .imagePng(let d):
            var tag: UInt8 = 2
            withUnsafeBytes(of: &tag, mix)
            d.withUnsafeBytes(mix)
        }
        return hash
    }
}
