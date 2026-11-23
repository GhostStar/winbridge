import AppKit
import Foundation

private final class PopupPanel: NSPanel {
    override var canBecomeKey: Bool { false }
    override var canBecomeMain: Bool { false }
}

private final class OwnedPanel: NSPanel {
    override var canBecomeKey: Bool { true }
    override var canBecomeMain: Bool { false }
}

enum PerformanceHUDFormatter {
    static func format(
        codecName: String,
        fps: Double,
        mbps: Double,
        rttMs: Double?,
        width: UInt16,
        height: UInt16
    ) -> String {
        let rttStr = rttMs.map { String(format: "%.1fms", $0) } ?? "--"
        let dims = (width > 0 && height > 0) ? " · \(width)×\(height)" : ""
        return String(format: "%@ · %.0f FPS · %.1f Mbps · RTT %@%@", codecName, fps, mbps, rttStr, dims)
    }
}

@MainActor
final class RemoteWindowController: NSWindowController, NSWindowDelegate {
    let windowId: UInt64
    private(set) var info: WindowInfo
    let remoteView: RemoteWindowView
    let decoder: WindowVideoDecoder

    var onSendControl: ((ControlMessage.Payload) -> Void)?
    var onClipboardAction: ((String, UInt64) -> Void)?
    var onDetachRequested: ((UInt64) -> Void)?
    var onOwnerMovedOrResized: ((UInt64) -> Void)?
    var onRequestQuit: (() -> Void)?
    var onOpenManager: (() -> Void)?
    var onToggleHud: (() -> Void)?
    var onToggleMuteWindow: ((UInt64) -> Void)?
    var onPinOnTopChanged: (() -> Void)?
    var onLayoutChanged: ((UInt64) -> Void)?
    var onPullClipboardFiles: (() -> Void)?
    var onFilesDropped: ((UInt64, [URL]) -> Void)?
    var cmdMapsToCtrl: () -> Bool = { true }
    var compatibleTrackpadScroll: () -> Bool = { true }
    var mouseSensitivity: () -> Float = { 1.0 }
    var suppressInactiveHover: () -> Bool = { false }
    var shortcuts: () -> ShortcutBindings = { .defaults }
    var currentRttMs: () -> Double? = { nil }
    var showPerformanceHud: () -> Bool = { false }

    private var baseTitle: String = ""
    private(set) var isPinnedOnTop = false
    private(set) var isAudioMuted = false
    private var isApplyingRemoteGeometry = false
    private var isLiveResizing = false
    private var localResizeSeq: UInt32 = 0
    private var resizeGesture: BorderResizeGesture?
    private var pendingHostResize: (sequence: UInt32, gesture: BorderResizeGesture)?
    private var resizeAckTimer: Timer?
    private var resizeDebounceTimer: Timer?
    private var layoutSaveTimer: Timer?
    private(set) var isLocallyHidden = false
    private var localChromeResize = false
    private var preZoomFrame: NSRect?
    private weak var presentationOwner: RemoteWindowController?
    private var localPanelOffset = NSPoint.zero
    private var panelUsesLocalSize = false
    private var allowImmediateClose = false
    private var lastFrameSize: (UInt16, UInt16)?

    // Rolling Performance HUD counters
    private var hudTimer: Timer?
    private var hudWindowStart = Date()
    private var hudFramesInWindow: Int = 0
    private var hudBytesInWindow: Int = 0
    private var hudLastCodec: String = "—"

    init(info: WindowInfo, owner: RemoteWindowController?) {
        self.windowId = info.windowId
        self.info = info
        self.decoder = WindowVideoDecoder(windowId: info.windowId)

        let contentSize = Self.macContentSize(for: info)
        let contentRect = NSRect(origin: .zero, size: contentSize)
        self.remoteView = RemoteWindowView(windowId: info.windowId, frame: contentRect)

        let window: NSWindow
        switch info.kind {
        case .popup:
            let panel = PopupPanel(
                contentRect: contentRect,
                styleMask: [.borderless, .nonactivatingPanel],
                backing: .buffered,
                defer: false
            )
            panel.isFloatingPanel = true
            panel.level = .popUpMenu
            panel.hasShadow = true
            panel.isOpaque = !info.hasAlpha
            panel.backgroundColor = .clear
            panel.hidesOnDeactivate = true
            panel.collectionBehavior = [.moveToActiveSpace, .transient]
            window = panel

        case .dialog:
            let win = OwnedPanel(
                contentRect: contentRect,
                styleMask: info.usesBorderlessPresentation ? [.borderless] : [.titled, .closable, .resizable],
                backing: .buffered,
                defer: false
            )
            win.title = info.title.isEmpty ? info.processName : info.title
            win.isReleasedWhenClosed = false
            win.hidesOnDeactivate = true
            win.collectionBehavior = [.fullScreenAuxiliary]
            window = win

        case .normal, .unspecified:
            let win = NSWindow(
                contentRect: contentRect,
                styleMask: [.titled, .closable, .miniaturizable, .resizable],
                backing: .buffered,
                defer: false
            )
            win.title = info.title.isEmpty ? info.processName : info.title
            win.isReleasedWhenClosed = false
            window = win
        }

        window.isExcludedFromWindowsMenu = info.kind == .dialog || info.kind == .popup
        remoteView.inputEnabled = !info.isDisabled
        window.contentView = remoteView
        window.acceptsMouseMovedEvents = true
        window.minSize = info.usesBorderlessPresentation ? NSSize(width: 16, height: 16) : NSSize(width: 160, height: 100)

        self.baseTitle = info.title.isEmpty ? info.processName : info.title
        super.init(window: window)
        window.delegate = self

        remoteView.compatibleTrackpadScroll = { [weak self] in self?.compatibleTrackpadScroll() ?? true }
        remoteView.cmdMapsToCtrl = { [weak self] in self?.cmdMapsToCtrl() ?? true }
        remoteView.mouseSensitivity = { [weak self] in self?.mouseSensitivity() ?? 1.0 }
        remoteView.suppressInactiveHover = { [weak self] in self?.suppressInactiveHover() ?? false }
        remoteView.shortcuts = { [weak self] in self?.shortcuts() ?? .defaults }
        remoteView.onClipboardAction = { [weak self] action in
            guard let self else { return }
            self.onClipboardAction?(action, self.windowId)
        }
        remoteView.onSendControl = { [weak self] payload in self?.onSendControl?(payload) }
        remoteView.onRequestQuit = { [weak self] in self?.onRequestQuit?() }
        remoteView.onOpenManager = { [weak self] in self?.onOpenManager?() }
        remoteView.onToggleHud = { [weak self] in self?.onToggleHud?() }
        remoteView.onTogglePinOnTop = { [weak self] in self?.togglePinOnTop() }
        remoteView.onToggleMuteWindow = { [weak self] in
            guard let self else { return }
            self.onToggleMuteWindow?(self.windowId)
        }
        remoteView.onPullClipboardFiles = { [weak self] in self?.onPullClipboardFiles?() }
        remoteView.onFilesDropped = { [weak self] urls in
            guard let self else { return }
            self.onFilesDropped?(self.windowId, urls)
        }
        remoteView.onGameLockStateChanged = { [weak self] _ in self?.updateWindowTitle() }

        updateResizeBehavior()
        applyTransparency(info.hasAlpha)

        decoder.onDecodedFrame = { [weak self] image, w, h in
            DispatchQueue.main.async {
                guard let self else { return }
                let changed = self.lastFrameSize?.0 != w || self.lastFrameSize?.1 != h
                self.lastFrameSize = (w, h)
                if changed {
                    self.updateResizeBehavior()
                }
                self.applyTransparency(image.hasAlpha)
                self.remoteView.display(frame: image)
            }
        }
        decoder.onRequestKeyframe = { [weak self] in
            DispatchQueue.main.async {
                guard let self else { return }
                self.onSendControl?(.requestKeyFrame(RequestKeyFrame(windowId: self.windowId)))
            }
        }

        if info.kind == .popup || info.kind == .dialog, let owner {
            repositionRelative(to: owner)
        } else {
            window.center()
        }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    static func dpiScale(for info: WindowInfo) -> CGFloat {
        let dpi = info.dpi > 0 ? CGFloat(info.dpi) : 96.0
        return max(1.0, dpi / 96.0)
    }

    static func macContentSize(for info: WindowInfo) -> NSSize {
        let scale = dpiScale(for: info)
        var w = max(16.0, round(CGFloat(max(info.width, 1)) / scale))
        var h = max(16.0, round(CGFloat(max(info.height, 1)) / scale))
        if info.windowId == SyntheticWindow.primaryDesktop, let screen = NSScreen.main?.visibleFrame {
            let maxW = screen.width * 0.78
            let maxH = screen.height * 0.78
            let fit = min(1.0, min(maxW / w, maxH / h))
            w = max(320.0, round(w * fit))
            h = max(200.0, round(h * fit))
        }
        return NSSize(width: w, height: h)
    }

    private var isDesktop: Bool { windowId == SyntheticWindow.primaryDesktop }

    private var activeSourceSize: CGSize {
        if let (fw, fh) = lastFrameSize, fw > 0, fh > 0 {
            return CGSize(width: CGFloat(fw), height: CGFloat(fh))
        }
        return CGSize(width: CGFloat(max(info.width, 1)), height: CGFloat(max(info.height, 1)))
    }

    var videoRectInScreen: NSRect {
        guard let window else { return .zero }
        let rectInWin = remoteView.convert(remoteView.currentVideoRect, to: nil)
        return window.convertToScreen(rectInWin)
    }

    func applyTransparency(_ enabled:Bool) {
        window?.isOpaque = !enabled
        window?.backgroundColor = enabled ? .clear : .black
        // WGC already includes the source shadow in its alpha pixels.
        window?.hasShadow = !enabled
        remoteView.setTransparency(enabled)
    }

    // Chrome follows presentation metadata, including updates on an existing HWND.
    // Keep the captured content at the same screen rect when adding/removing a title.
    private func updateNativeChrome() {
        guard let window else { return }
        let content = window.contentRect(forFrameRect: window.frame)
        var style: NSWindow.StyleMask = info.usesBorderlessPresentation ? [.borderless] : [.titled, .closable, .resizable]
        if info.usesIndependentLayout { style.insert(.miniaturizable) }
        if info.kind == .popup { style.insert(.nonactivatingPanel) }
        isApplyingRemoteGeometry = true
        window.styleMask = style
        window.minSize = info.usesBorderlessPresentation ? NSSize(width: 16, height: 16) : NSSize(width: 160, height: 100)
        window.setFrame(window.frameRect(forContentRect: content), display: true)
        window.isExcludedFromWindowsMenu = isLocallyHidden || !info.usesIndependentLayout
        isApplyingRemoteGeometry = false
    }

    func updateResizeBehavior() {
        let src = activeSourceSize
        guard let window else { return }
        if info.kind == .popup {
            remoteView.updateDisplayGeometry(sourceSize: src, preserveAspect: false)
            return
        }
        // A permanent AppKit aspect constraint would also constrain Option-resize.
        // The delegate applies aspect locking only to local border gestures.
        window.resizeIncrements = NSSize(width: 1, height: 1)
        remoteView.updateDisplayGeometry(sourceSize: src, preserveAspect: true)
    }

    func present(owner: RemoteWindowController?) {
        guard let window else { return }
        guard !isLocallyHidden else { return }
        if info.kind == .popup || info.kind == .dialog {
            if let owner, let ownerWindow = owner.window {
                repositionRelative(to: owner)
                if window.parent !== ownerWindow {
                    ownerWindow.addChildWindow(window, ordered: .above)
                }
            }
            window.orderFront(nil)
            if info.kind == .dialog {
                window.makeKeyAndOrderFront(nil)
                window.makeFirstResponder(remoteView)
            }
        } else {
            showWindow(nil)
            window.makeKeyAndOrderFront(nil)
            window.makeFirstResponder(remoteView)
        }
    }

    func applyUpdate(_ update: WindowUpdated, owner: RemoteWindowController?) {
        guard let newInfo = update.window else { return }
        let oldInfo = self.info
        self.info = newInfo
        if newInfo.kind != oldInfo.kind { updateHudVisibility() }
        if newInfo.kind != oldInfo.kind || newInfo.usesBorderlessPresentation != oldInfo.usesBorderlessPresentation {
            updateNativeChrome()
        }
        if newInfo.isDisabled && !oldInfo.isDisabled { remoteView.releaseAllHeldKeys() }
        remoteView.inputEnabled = !isLocallyHidden && !newInfo.isDisabled

        guard let window else { return }
        if newInfo.kind != .popup {
            let newTitle = newInfo.title.isEmpty ? newInfo.processName : newInfo.title
            if baseTitle != newTitle {
                baseTitle = newTitle
                updateWindowTitle()
            }
        }

        if !hostResizeAllowed && (resizeGesture?.changesHost == true || pendingHostResize != nil) {
            cancelResizeInteraction()
        }
        if isLocallyHidden { return }
        if let owner, newInfo.kind == .dialog || newInfo.kind == .popup {
            presentationOwner = owner
            if let ownerWindow = owner.window, window.parent !== ownerWindow {
                ownerWindow.addChildWindow(window, ordered: .above)
            }
        }
        // Protect both normal windows and panels before owner-relative layout.
        if isLiveResizing { return }
        if let pending = pendingHostResize {
            guard update.ackResizeSeq >= pending.sequence else { return }
            pendingHostResize = nil
            resizeAckTimer?.invalidate(); resizeAckTimer = nil
            let target = NSSize(width: CGFloat(newInfo.width) / pending.gesture.pixelsPerPoint.width,
                                height: CGFloat(newInfo.height) / pending.gesture.pixelsPerPoint.height)
            applyProgrammaticContentSize(target)
            panelUsesLocalSize = info.kind == .dialog
            rememberPanelOffset()
            updateResizeBehavior()
            return
        }
        if newInfo.kind == .popup || newInfo.kind == .dialog {
            if let owner, newInfo.kind == .popup || newInfo.offsetX != oldInfo.offsetX || newInfo.offsetY != oldInfo.offsetY
                || newInfo.width != oldInfo.width || newInfo.height != oldInfo.height || newInfo.dpi != oldInfo.dpi {
                repositionRelative(to: owner)
            }
            if newInfo.kind == .popup { return }
        }
        let dimsChanged = newInfo.width != oldInfo.width || newInfo.height != oldInfo.height || newInfo.dpi != oldInfo.dpi
        if dimsChanged { updateResizeBehavior() }
        // Default behavior is always local scaling; remote snapshots do not reset zoom.
        if dimsChanged && oldInfo.width > 0 && oldInfo.height > 0 && newInfo.width > 0 && newInfo.height > 0 {
            let oldRatio = CGFloat(oldInfo.width) / CGFloat(oldInfo.height)
            let newRatio = CGFloat(newInfo.width) / CGFloat(newInfo.height)
            if abs(oldRatio - newRatio) > 0.002 {
                let width = window.contentLayoutRect.width
                applyProgrammaticContentSize(NSSize(width: width, height: width / newRatio))
                rememberPanelOffset()
            }
        }
    }

    private func applyProgrammaticContentSize(_ targetSize: NSSize) {
        guard let window else { return }
        isApplyingRemoteGeometry = true
        let oldFrame = window.frame
        let newFrameSize = window.frameRect(forContentRect: NSRect(origin: .zero, size: targetSize)).size
        let newOriginY = oldFrame.maxY - newFrameSize.height
        let newFrame = NSRect(origin: NSPoint(x: oldFrame.minX, y: newOriginY), size: newFrameSize)
        window.setFrame(newFrame, display: true)
        isApplyingRemoteGeometry = false
    }

    private func relativeFrame(to owner: RemoteWindowController) -> NSRect {
        guard let window else { return .zero }
        let ownerVideoScreen = owner.videoRectInScreen
        let scaleX = ownerVideoScreen.width / CGFloat(max(owner.info.width, 1))
        let scaleY = ownerVideoScreen.height / CGFloat(max(owner.info.height, 1))

        let contentW = max(16.0, round(CGFloat(max(info.width, 1)) * scaleX))
        let contentH = max(16.0, round(CGFloat(max(info.height, 1)) * scaleY))

        let leftX = ownerVideoScreen.minX + CGFloat(info.offsetX) * scaleX
        // Windows offsetY grows downward from owner top; macOS screen Y grows upward.
        let topY = ownerVideoScreen.maxY - CGFloat(info.offsetY) * scaleY
        let contentOriginY = topY - contentH
        let contentScreenRect = NSRect(x: round(leftX), y: round(contentOriginY), width: contentW, height: contentH)
        var frameRect = window.frameRect(forContentRect: contentScreenRect)
        if info.kind == .dialog && panelUsesLocalSize {
            frameRect.origin.y += frameRect.height - window.frame.height
            frameRect.size = window.frame.size
        }
        return frameRect
    }

    func repositionRelative(to owner: RemoteWindowController) {
        presentationOwner = owner
        guard !isLiveResizing, pendingHostResize == nil, let window else { return }
        var frameRect = relativeFrame(to: owner)
        if info.kind == .dialog {
            frameRect.origin.x += localPanelOffset.x
            frameRect.origin.y += localPanelOffset.y
        }
        isApplyingRemoteGeometry = true
        window.setFrame(frameRect, display: true)
        isApplyingRemoteGeometry = false
    }

    func handleMediaFrame(_ frame: MediaFrame) {
        if !frame.header.flags.contains(.endOfStream) && !frame.payload.isEmpty {
            hudFramesInWindow += 1
            hudBytesInWindow += frame.payload.count
            hudLastCodec = frame.header.codec.displayName
        }
        decoder.decode(frame)
    }

    func updateHudVisibility() {
        let visible = info.usesIndependentLayout && showPerformanceHud()
        remoteView.setHudVisible(visible)
        if visible {
            if hudTimer == nil {
                hudWindowStart = Date()
                hudFramesInWindow = 0
                hudBytesInWindow = 0
                hudTimer = Timer.scheduledTimer(withTimeInterval: 0.5, repeats: true) { [weak self] _ in
                    Task { @MainActor in
                        self?.refreshHudStats()
                    }
                }
            }
            refreshHudStats()
        } else {
            hudTimer?.invalidate()
            hudTimer = nil
        }
    }

    private func refreshHudStats() {
        let now = Date()
        let elapsed = max(0.05, now.timeIntervalSince(hudWindowStart))
        let fps = Double(hudFramesInWindow) / elapsed
        let mbps = (Double(hudBytesInWindow) * 8.0) / (elapsed * 1_000_000.0)
        hudWindowStart = now
        hudFramesInWindow = 0
        hudBytesInWindow = 0
        let w = lastFrameSize?.0 ?? UInt16(min(info.width, UInt32(UInt16.max)))
        let h = lastFrameSize?.1 ?? UInt16(min(info.height, UInt32(UInt16.max)))
        remoteView.updateHudText(PerformanceHUDFormatter.format(
            codecName: hudLastCodec,
            fps: fps,
            mbps: mbps,
            rttMs: currentRttMs(),
            width: w,
            height: h
        ))
    }

    func applyCursor(_ shape: CursorShape) {
        remoteView.applyCursorShape(shape)
    }

    func showToast(_ text: String, style: TransferToastStyle, autoHideSeconds: TimeInterval?) {
        guard info.kind != .popup else { return }
        remoteView.showToast(text, style: style, autoHideSeconds: autoHideSeconds)
    }

    func setPointerLock(_ enabled: Bool) {
        guard info.kind != .popup else { return }
        remoteView.setGameLockEnabled(enabled)
    }

    func togglePointerLock() {
        guard info.kind != .popup else { return }
        remoteView.setGameLockEnabled(!remoteView.gameLockEnabled)
    }

    func unlockPointerIfNeeded() {
        if remoteView.gameLockEnabled {
            remoteView.setGameLockEnabled(false)
        }
    }

    func togglePinOnTop() {
        setPinnedOnTop(!isPinnedOnTop, silent: false)
    }

    func setPinnedOnTop(_ pinned: Bool, silent: Bool = false) {
        guard info.kind != .popup, let window else { return }
        guard pinned != isPinnedOnTop else { return }
        isPinnedOnTop = pinned
        // Keep floating window pinned on the current macOS Desktop/Space only (do not add .canJoinAllSpaces).
        window.level = pinned ? .floating : .normal
        updateWindowTitle()
        if !silent {
            showToast(
                pinned ? "📌 窗口已置顶（当前桌面）" : "窗口已取消置顶",
                style: .progress,
                autoHideSeconds: 1.8
            )
        }
        onPinOnTopChanged?()
        onLayoutChanged?(windowId)
    }

    func setAudioMuted(_ muted: Bool) {
        guard info.kind != .popup else { return }
        guard muted != isAudioMuted else { return }
        isAudioMuted = muted
        updateWindowTitle()
        onLayoutChanged?(windowId)
    }

    func snapshotLayout() -> SavedWindowLayout? {
        guard info.usesIndependentLayout, let window else { return nil }
        return SavedWindowLayout(
            rect: window.frame,
            isPinnedOnTop: isPinnedOnTop,
            isAudioMuted: isAudioMuted
        )
    }

    func applySavedLayout(_ layout: SavedWindowLayout) {
        guard info.usesIndependentLayout, let window else { return }
        let clamped = SavedWindowLayout.clampToActiveScreens(layout.rect)
        isApplyingRemoteGeometry = true
        window.setFrame(clamped, display: true)
        isApplyingRemoteGeometry = false
        setPinnedOnTop(layout.isPinnedOnTop, silent: true)
        setAudioMuted(layout.isAudioMuted)
    }

    private func scheduleLayoutSave() {
        guard info.usesIndependentLayout else { return }
        layoutSaveTimer?.invalidate()
        layoutSaveTimer = Timer.scheduledTimer(withTimeInterval: 0.35, repeats: false) { [weak self] _ in
            Task { @MainActor in
                guard let self else { return }
                self.onLayoutChanged?(self.windowId)
            }
        }
    }

    private func updateWindowTitle() {
        guard info.kind != .popup, let window else { return }
        var tags: [String] = []
        if resizeGesture?.changesHost == true { tags.append(ResizeInteractionText.active) }
        if isPinnedOnTop {
            tags.append("置顶")
        }
        if isAudioMuted {
            tags.append("静音")
        }
        switch remoteView.gameLockState {
        case .off:
            break
        case .confinedVisible:
            tags.append("游戏锁定 · 菜单指针 · Ctrl+Option 释放")
        case .relativeLocked:
            tags.append("游戏锁定 · 3D 视角 · Ctrl+Option 释放")
        }
        if tags.isEmpty {
            window.title = baseTitle
        } else {
            window.title = "\(baseTitle) [\(tags.joined(separator: " · "))]"
        }
    }

    func hideLocally() {
        guard !isLocallyHidden else { return }
        cancelResizeInteraction()
        unlockPointerIfNeeded()
        remoteView.releaseAllHeldKeys()
        isLocallyHidden = true
        remoteView.inputEnabled = false
        window?.isExcludedFromWindowsMenu = true
        if let window, let parent = window.parent { parent.removeChildWindow(window) }
        window?.orderOut(nil)
    }

    func restoreLocally(owner: RemoteWindowController?) {
        isLocallyHidden = false
        remoteView.inputEnabled = !info.isDisabled
        window?.isExcludedFromWindowsMenu = info.kind == .dialog || info.kind == .popup
        if window?.isMiniaturized == true { window?.deminiaturize(nil) }
        present(owner: owner)
        onSendControl?(.requestKeyFrame(RequestKeyFrame(windowId: windowId)))
    }

    func forceCloseLocally() {
        cancelResizeInteraction()
        layoutSaveTimer?.invalidate()
        layoutSaveTimer = nil
        hudTimer?.invalidate()
        hudTimer = nil
        unlockPointerIfNeeded()
        remoteView.releaseAllHeldKeys()
        decoder.invalidate()
        allowImmediateClose = true
        if let window, let parent = window.parent {
            parent.removeChildWindow(window)
        }
        window?.close()
    }

    // MARK: - NSWindowDelegate

    func windowDidBecomeKey(_ notification: Notification) {
        guard info.kind != .popup, !info.isDisabled, !isLocallyHidden else { return }
        window?.makeFirstResponder(remoteView)
        onSendControl?(.focusWindow(FocusWindow(windowId: windowId, focused: true)))
    }

    func windowDidResignKey(_ notification: Notification) {
        unlockPointerIfNeeded()
        remoteView.releaseAllHeldKeys()
        if info.kind != .popup {
            onSendControl?(.focusWindow(FocusWindow(windowId: windowId, focused: false)))
        }
    }

    func windowWillClose(_ notification: Notification) {
        unlockPointerIfNeeded()
    }

    private var hostResizeAllowed: Bool {
        !isDesktop && info.kind != .popup && info.isResizable && !info.isDisabled && !isLocallyHidden && info.width > 0 && info.height > 0
    }

    // Internal entry point also used by native-window regression tests.
    func beginBorderResize(option: Bool, pointerDown: Bool) {
        guard pointerDown, !isLiveResizing, !localChromeResize, !isApplyingRemoteGeometry, !isLocallyHidden, info.kind != .popup,
              let window else { return }
        resizeAckTimer?.invalidate(); resizeAckTimer = nil
        pendingHostResize = nil
        isLiveResizing = true
        remoteView.releaseAllHeldKeys()
        remoteView.localBorderGesture = true
        resizeGesture = BorderResizeGesture(option: option, hostAllowed: hostResizeAllowed,
            source: NSSize(width: CGFloat(info.width), height: CGFloat(info.height)),
            displayed: remoteView.currentVideoRect.size, aspect: activeSourceSize)
        if info.kind == .dialog { panelUsesLocalSize = true }
        window.resizeIncrements = NSSize(width: 1, height: 1)
        if option && !hostResizeAllowed { showToast(ResizeInteractionText.unavailable, style: .progress, autoHideSeconds: 2) }
        updateWindowTitle()
    }

    func windowWillStartLiveResize(_ notification: Notification) {
        guard let resizingWindow = notification.object as? NSWindow else { return }
        beginBorderResize(option: resizingWindow.resizeFlags.contains(.option), pointerDown: NSEvent.pressedMouseButtons & 1 != 0)
    }

    func windowWillResize(_ sender: NSWindow, to proposedFrame: NSSize) -> NSSize {
        guard !isApplyingRemoteGeometry, !localChromeResize, info.kind != .popup else { return proposedFrame }
        if resizeGesture?.changesHost == true { return proposedFrame }
        let proposed = sender.contentRect(forFrameRect: NSRect(origin: .zero, size: proposedFrame)).size
        let size = BorderResizeGesture.proportional(proposed, from: sender.contentLayoutRect.size,
                                                    source: resizeGesture?.sourceAspect ?? activeSourceSize)
        return sender.frameRect(forContentRect: NSRect(origin: .zero, size: size)).size
    }

    func windowDidResize(_ notification: Notification) {
        guard !isApplyingRemoteGeometry, info.kind != .popup else { return }
        onOwnerMovedOrResized?(windowId)
        scheduleLayoutSave()
        if isLiveResizing, resizeGesture?.changesHost == true { scheduleDebouncedResize() }
    }

    func windowDidMove(_ notification: Notification) {
        guard !isApplyingRemoteGeometry, info.kind != .popup else { return }
        rememberPanelOffset()
        onOwnerMovedOrResized?(windowId)
        scheduleLayoutSave()
    }

    func windowDidEndLiveResize(_ notification: Notification) {
        let wasLiveResizing = isLiveResizing
        resizeDebounceTimer?.invalidate(); resizeDebounceTimer = nil
        if wasLiveResizing, resizeGesture?.changesHost == true { sendCurrentSizeToHost() }
        isLiveResizing = false
        remoteView.localBorderGesture = false
        resizeGesture = nil
        if wasLiveResizing && info.kind == .dialog { panelUsesLocalSize = true; rememberPanelOffset() }
        updateWindowTitle()
        updateResizeBehavior()
        if wasLiveResizing { scheduleLayoutSave() }
    }

    private func cancelResizeInteraction() {
        resizeDebounceTimer?.invalidate(); resizeDebounceTimer = nil
        resizeAckTimer?.invalidate(); resizeAckTimer = nil
        isLiveResizing = false
        remoteView.localBorderGesture = false
        resizeGesture = nil
        pendingHostResize = nil
        updateWindowTitle()
        updateResizeBehavior()
    }

    private func rememberPanelOffset() {
        guard info.kind == .dialog, let owner = presentationOwner, let window else { return }
        let expected = relativeFrame(to: owner)
        localPanelOffset = NSPoint(x: window.frame.minX - expected.minX, y: window.frame.minY - expected.minY)
    }

    func windowDidDeminiaturize(_ notification: Notification) {
        // Request fresh IDR keyframe upon restoring minimized macOS window.
        onSendControl?(.requestKeyFrame(RequestKeyFrame(windowId: windowId)))
    }

    func windowShouldClose(_ sender: NSWindow) -> Bool {
        if allowImmediateClose {
            return true
        }
        onDetachRequested?(windowId)
        return false
    }

    // Native title-bar actions always affect only the local viewer.
    func windowShouldZoom(_ sender: NSWindow, toFrame newFrame: NSRect) -> Bool {
        cancelResizeInteraction()
        isApplyingRemoteGeometry = true
        var target = preZoomFrame ?? newFrame
        if preZoomFrame == nil {
            let content = sender.contentRect(forFrameRect: target)
            let ratio = activeSourceSize.width / max(1, activeSourceSize.height)
            let width = min(content.width, content.height * ratio)
            target = sender.frameRect(forContentRect: NSRect(x: content.minX, y: content.maxY - width / ratio,
                                                            width: width, height: width / ratio))
        }
        preZoomFrame = preZoomFrame == nil ? sender.frame : nil
        sender.setFrame(target, display: true)
        isApplyingRemoteGeometry = false
        return false
    }

    func windowWillEnterFullScreen(_ notification: Notification) { cancelResizeInteraction(); localChromeResize = true }
    func windowDidEnterFullScreen(_ notification: Notification) { localChromeResize = false }
    func windowWillExitFullScreen(_ notification: Notification) { localChromeResize = true }
    func windowDidExitFullScreen(_ notification: Notification) { localChromeResize = false }

    private func scheduleDebouncedResize() {
        guard resizeDebounceTimer == nil else { return }
        let timer = Timer(timeInterval: 0.1, repeats: false) { [weak self] _ in
            // Common run-loop modes include AppKit's native resize tracking loop.
            MainActor.assumeIsolated {
                self?.resizeDebounceTimer = nil
                self?.sendCurrentSizeToHost()
            }
        }
        resizeDebounceTimer = timer
        RunLoop.main.add(timer, forMode: .common)
    }

    private func sendCurrentSizeToHost() {
        guard hostResizeAllowed, !localChromeResize, isLiveResizing,
              let gesture = resizeGesture, gesture.changesHost, let window else { return }
        let (physW, physH) = gesture.hostSize(for: window.contentLayoutRect.size)
        localResizeSeq &+= 1
        pendingHostResize = (localResizeSeq, gesture)
        resizeAckTimer?.invalidate()
        let sequence = localResizeSeq
        let timer = Timer(timeInterval: 2, repeats: false) { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self, !self.isLiveResizing, self.pendingHostResize?.sequence == sequence else { return }
                self.pendingHostResize = nil
                self.resizeAckTimer = nil
                self.updateResizeBehavior()
            }
        }
        resizeAckTimer = timer
        RunLoop.main.add(timer, forMode: .common)
        onSendControl?(.resizeWindow(ResizeWindow(windowId: windowId, width: physW, height: physH, resizeSeq: localResizeSeq)))
    }
}
