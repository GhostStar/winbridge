import AppKit
import Foundation

struct SavedWindowLayout: Codable, Equatable, Sendable {
    var x: Double
    var y: Double
    var width: Double
    var height: Double
    var isPinnedOnTop: Bool
    var isAudioMuted: Bool

    var rect: NSRect {
        NSRect(x: x, y: y, width: width, height: height)
    }

    init(rect: NSRect, isPinnedOnTop: Bool, isAudioMuted: Bool) {
        self.x = Double(rect.origin.x.rounded())
        self.y = Double(rect.origin.y.rounded())
        self.width = Double(max(200, rect.size.width.rounded()))
        self.height = Double(max(140, rect.size.height.rounded()))
        self.isPinnedOnTop = isPinnedOnTop
        self.isAudioMuted = isAudioMuted
    }

    /// Clamps a saved window frame into the nearest currently attached display's visible frame
    /// so unplugging an external monitor never leaves a restored window stranded off-screen.
    static func clamp(_ rect: NSRect, toScreens visibleFrames: [NSRect]) -> NSRect {
        guard !visibleFrames.isEmpty else { return rect }
        let targetScreen = visibleFrames.first(where: { $0.intersects(rect) }) ?? visibleFrames[0]
        let w = min(max(200, rect.width), targetScreen.width)
        let h = min(max(140, rect.height), targetScreen.height)
        let clampedX = min(max(rect.origin.x, targetScreen.minX), targetScreen.maxX - w)
        let clampedY = min(max(rect.origin.y, targetScreen.minY), targetScreen.maxY - h)
        return NSRect(x: clampedX.rounded(), y: clampedY.rounded(), width: w.rounded(), height: h.rounded())
    }

    @MainActor
    static func clampToActiveScreens(_ rect: NSRect) -> NSRect {
        let frames = NSScreen.screens.map(\.visibleFrame)
        return clamp(rect, toScreens: frames)
    }
}

struct WorkspacePresetEntry: Codable, Equatable, Sendable {
    var isDesktop: Bool
    var processName: String
    var titleHint: String
    var layout: SavedWindowLayout
}

struct WorkspacePreset: Codable, Equatable, Sendable {
    var id: String
    var name: String
    var entries: [WorkspacePresetEntry]
}
