import Foundation

/// Presentation ownership is independent of the host's capture-group root.
enum ViewerVisibility {
    static func parent(of info: WindowInfo) -> UInt64? {
        guard info.kind == .dialog || info.kind == .popup, info.ownerWindowId != 0 else { return nil }
        return info.ownerWindowId
    }

    static func subtree(_ root: UInt64, in windows: [WindowInfo]) -> Set<UInt64> {
        var result: Set<UInt64> = [root]
        for _ in 0..<windows.count {
            let previous = result.count
            for info in windows {
                if let parent = parent(of: info), result.contains(parent) { result.insert(info.windowId) }
            }
            if result.count == previous { break }
        }
        return result
    }

    static func ancestors(_ id: UInt64, in windows: [WindowInfo]) -> Set<UInt64> {
        let byID = Dictionary(windows.map { ($0.windowId, $0) }, uniquingKeysWith: { _, new in new })
        var result: Set<UInt64> = [id]
        var current = id
        while let info = byID[current], let parent = parent(of: info), result.insert(parent).inserted {
            current = parent
        }
        return result
    }
}
