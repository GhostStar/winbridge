import AppKit

/// Latched once at the beginning of a native border gesture. Retina affects
/// rendering, not the source-pixel / local-point conversion used for resizing.
struct BorderResizeGesture {
    let changesHost: Bool
    let sourceAspect: NSSize
    let pixelsPerPoint: NSSize

    init(option: Bool, hostAllowed: Bool, source: NSSize, displayed: NSSize, aspect: NSSize) {
        changesHost = option && hostAllowed
        sourceAspect = aspect
        pixelsPerPoint = NSSize(width: source.width / max(1, displayed.width),
                                height: source.height / max(1, displayed.height))
    }

    func hostSize(for content: NSSize) -> (UInt32, UInt32) {
        func dimension(_ value: CGFloat) -> UInt32 {
            guard value.isFinite else { return 16 }
            return UInt32(min(16_384, max(16, value.rounded())))
        }
        return (dimension(content.width * pixelsPerPoint.width), dimension(content.height * pixelsPerPoint.height))
    }

    static func proportional(_ proposed: NSSize, from current: NSSize, source: NSSize) -> NSSize {
        guard source.width > 0, source.height > 0 else { return proposed }
        let ratio = source.width / source.height
        let widthChange = abs(proposed.width - current.width) / max(1, current.width)
        let heightChange = abs(proposed.height - current.height) / max(1, current.height)
        let width = widthChange >= heightChange ? proposed.width : proposed.height * ratio
        let bounded = max(max(160, 100 * ratio), width)
        return NSSize(width: bounded, height: bounded / ratio)
    }
}

enum ResizeInteractionText {
    private static var chinese: Bool { Locale.preferredLanguages.first?.hasPrefix("zh") == true }
    static var hint: String { chinese ? "⌥ 拖边框：调整主机" : "⌥-drag border: resize host" }
    static var help: String { chinese
        ? "拖动边框默认等比缩放本地画面；按住 Option（⌥）开始拖动才调整 Windows 窗口大小。松手结束，不保留模式。"
        : "Drag a border to scale the local view. Hold Option (⌥) when starting the drag to resize the Windows window. The mode ends on release." }
    static var active: String { chinese ? "调整主机大小" : "Resizing host" }
    static var unavailable: String { chinese ? "此窗口当前不能调整主机大小，已使用本地缩放" : "Host resizing is unavailable for this window; scaling locally" }
}
