import AppKit
import Carbon.HIToolbox
import CoreGraphics
import CoreVideo
import Foundation
import QuartzCore

enum GameLockState: Equatable {
    /// Normal macOS window behavior: free cursor, absolute coordinates.
    case off
    /// Game Lock armed while the Windows game is showing a cursor (e.g. holding Alt or menu open):
    /// visible game cursor confined inside the Mac window, pixel-accurate absolute coordinates.
    case confinedVisible
    /// Game Lock armed while the Windows game hides the cursor (3D camera / combat):
    /// hidden disassociated cursor at window center, sending raw relative dx/dy deltas.
    case relativeLocked
}

/// Renders decoded video frames via CoreAnimation/IOSurface and forwards mouse,
/// trackpad scroll, physical HID keyboard, and native macOS IME (`NSTextInputClient`) input.
@MainActor
final class RemoteWindowView: NSView, @preconcurrency NSTextInputClient {
    let windowId: UInt64
    var cmdMapsToCtrl: () -> Bool = { true }
    var mouseSensitivity: () -> Float = { 1.0 }
    var onSendControl: ((ControlMessage.Payload) -> Void)?
    var onRequestQuit: (() -> Void)?
    var onOpenManager: (() -> Void)?
    var onGameLockStateChanged: ((GameLockState) -> Void)?

    private(set) var gameLockEnabled = false
    private(set) var gameLockState: GameLockState = .off
    private var remoteCursorHidden = false
    private var isCursorHidden = false
    private var pointerLockedAt: Date?

    private let videoLayer = CALayer()
    private var retainedFrame: DecodedImage?
    private var sourceSize: CGSize = .zero
    private var preserveAspectRatio: Bool = true
    private var trackingArea: NSTrackingArea?
    private var activeCursor: NSCursor = .arrow
    private var mouseInside = false

    // Keyboard & IME state
    private var markedText = NSMutableAttributedString()
    private var markedRangeStorage = NSRange(location: NSNotFound, length: 0)
    private var selectedRangeStorage = NSRange(location: 0, length: 0)
    private var lastMouseViewPoint = NSPoint(x: 40, y: 40)

    /// Tracks active physical keydowns so matching keyUps and focus-loss releases are always sent.
    private var heldKeyCodes: [UInt16: UInt32] = [:]
    private var heldModifiers: Set<UInt32> = []

    /// State shared across a single `interpretKeyEvents` call.
    private var currentKeyEvent: NSEvent?
    private var imeHandledCurrentKey = false
    private var hadMarkedTextBeforeKeyDown = false

    init(windowId: UInt64, frame: NSRect) {
        self.windowId = windowId
        super.init(frame: frame)
        wantsLayer = true
        layer = CALayer()
        layer?.backgroundColor = NSColor.black.cgColor

        videoLayer.contentsGravity = .resize
        videoLayer.magnificationFilter = .linear
        videoLayer.minificationFilter = .linear
        videoLayer.isOpaque = false
        layer?.addSublayer(videoLayer)
    }

    static func computeVideoRect(in bounds: NSRect, sourceSize: CGSize, preserveAspect: Bool) -> NSRect {
        guard preserveAspect, sourceSize.width > 0, sourceSize.height > 0,
              bounds.width > 0, bounds.height > 0 else {
            return bounds
        }
        let scale = min(bounds.width / sourceSize.width, bounds.height / sourceSize.height)
        let w = sourceSize.width * scale
        let h = sourceSize.height * scale
        let x = (bounds.width - w) * 0.5
        let y = (bounds.height - h) * 0.5
        return NSRect(x: x, y: y, width: w, height: h)
    }

    var currentVideoRect: NSRect {
        Self.computeVideoRect(in: bounds, sourceSize: sourceSize, preserveAspect: preserveAspectRatio)
    }

    func updateDisplayGeometry(sourceSize: CGSize, preserveAspect: Bool) {
        self.sourceSize = sourceSize
        self.preserveAspectRatio = preserveAspect
        layoutVideoLayer()
    }

    private func layoutVideoLayer() {
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        videoLayer.frame = currentVideoRect
        if let scale = window?.backingScaleFactor {
            videoLayer.contentsScale = scale
        }
        CATransaction.commit()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    /// Top-left origin matches Windows coordinate space directly.
    override var isFlipped: Bool { true }
    override var acceptsFirstResponder: Bool { true }

    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    override func layout() {
        super.layout()
        layoutVideoLayer()
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let trackingArea {
            removeTrackingArea(trackingArea)
        }
        let area = NSTrackingArea(
            rect: bounds,
            options: [.mouseMoved, .mouseEnteredAndExited, .activeAlways, .inVisibleRect, .cursorUpdate],
            owner: self,
            userInfo: nil
        )
        addTrackingArea(area)
        trackingArea = area
    }

    // MARK: - Frame & Cursor Updates

    func display(frame: DecodedImage) {
        retainedFrame = frame
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        videoLayer.contents = frame.layerContents
        CATransaction.commit()
    }

    func applyCursorShape(_ shape: CursorShape) {
        let hidden = CursorMapper.isEffectivelyHidden(shape)
        let cursor = CursorMapper.cursor(from: shape)
        activeCursor = cursor
        if hidden != remoteCursorHidden {
            remoteCursorHidden = hidden
            if gameLockEnabled {
                ClientLogger.shared.info(
                    "GameLock cursor -> \(hidden ? "hidden (3D relative)" : "visible (menu pointer)") type=\(shape.cursorType)"
                )
            }
            reconcileGameLockState()
        }
        if mouseInside && gameLockState != .relativeLocked {
            cursor.set()
        }
        window?.invalidateCursorRects(for: self)
    }

    override func cursorUpdate(with event: NSEvent) {
        if gameLockState != .relativeLocked {
            activeCursor.set()
        }
    }

    override func resetCursorRects() {
        addCursorRect(bounds, cursor: activeCursor)
    }

    // MARK: - Smart 3D Game Mouse Lock (Auto-switches between 3D Relative & Confined Menu Pointer)

    func setGameLockEnabled(_ enabled: Bool) {
        guard enabled != gameLockEnabled else { return }
        gameLockEnabled = enabled
        ClientLogger.shared.info(
            "GameLock \(enabled ? "ENABLED" : "DISABLED") for window 0x\(String(windowId, radix: 16)) (remoteCursorHidden=\(remoteCursorHidden))"
        )
        reconcileGameLockState()
    }

    private func cocoaToCGPoint(_ cocoaScreenPoint: NSPoint) -> CGPoint {
        let primaryHeight = NSScreen.screens.first?.frame.height
            ?? CGDisplayBounds(CGMainDisplayID()).height
        return CGPoint(x: cocoaScreenPoint.x, y: primaryHeight - cocoaScreenPoint.y)
    }

    private func videoRectInCocoaScreen() -> NSRect? {
        guard let window else { return nil }
        let rectInWin = convert(currentVideoRect, to: nil)
        return window.convertToScreen(rectInWin)
    }

    private func warpCursorToVideoCenter() {
        guard let screenRect = videoRectInCocoaScreen() else { return }
        let center = NSPoint(x: screenRect.midX, y: screenRect.midY)
        CGWarpMouseCursorPosition(cocoaToCGPoint(center))
    }

    private func confineCursorIfNeeded() {
        guard gameLockState == .confinedVisible,
              let screenRect = videoRectInCocoaScreen() else { return }
        let inset = screenRect.insetBy(dx: min(4, screenRect.width * 0.1), dy: min(4, screenRect.height * 0.1))
        guard inset.width > 0, inset.height > 0 else { return }
        let pos = NSEvent.mouseLocation
        let clampedX = min(max(pos.x, inset.minX), inset.maxX)
        let clampedY = min(max(pos.y, inset.minY), inset.maxY)
        if clampedX != pos.x || clampedY != pos.y {
            CGWarpMouseCursorPosition(cocoaToCGPoint(NSPoint(x: clampedX, y: clampedY)))
        }
    }

    private func reconcileGameLockState() {
        let desired: GameLockState
        if !gameLockEnabled {
            desired = .off
        } else if remoteCursorHidden {
            desired = .relativeLocked
        } else {
            desired = .confinedVisible
        }
        guard desired != gameLockState else { return }
        let previous = gameLockState
        gameLockState = desired

        switch desired {
        case .relativeLocked:
            guard let window else {
                gameLockState = .off
                gameLockEnabled = false
                return
            }
            window.makeKeyAndOrderFront(nil)
            window.makeFirstResponder(self)
            if previous == .off {
                onSendControl?(.focusWindow(FocusWindow(windowId: windowId, focused: true)))
                onSendControl?(.mouseEvent(MouseEvent(
                    windowId: windowId,
                    kind: .move,
                    normX: 0.5,
                    normY: 0.5
                )))
            }
            warpCursorToVideoCenter()
            CGAssociateMouseAndMouseCursorPosition(0)
            if !isCursorHidden {
                NSCursor.hide()
                isCursorHidden = true
            }
            pointerLockedAt = Date()

        case .confinedVisible:
            pointerLockedAt = nil
            CGAssociateMouseAndMouseCursorPosition(1)
            if isCursorHidden {
                NSCursor.unhide()
                isCursorHidden = false
            }
            if previous == .relativeLocked {
                // Game just revealed cursor (Alt / menu open): show cursor right at window center
                // and align Windows cursor cleanly.
                warpCursorToVideoCenter()
                onSendControl?(.mouseEvent(MouseEvent(
                    windowId: windowId,
                    kind: .move,
                    normX: 0.5,
                    normY: 0.5
                )))
            } else {
                window?.makeKeyAndOrderFront(nil)
                window?.makeFirstResponder(self)
                confineCursorIfNeeded()
            }
            activeCursor.set()

        case .off:
            pointerLockedAt = nil
            CGAssociateMouseAndMouseCursorPosition(1)
            if isCursorHidden {
                NSCursor.unhide()
                isCursorHidden = false
            }
            if mouseInside {
                activeCursor.set()
            } else {
                NSCursor.arrow.set()
            }
        }

        onGameLockStateChanged?(desired)
    }

    // MARK: - Focus & Stuck-Key Safety

    func releaseAllHeldKeys() {
        for (_, hid) in heldKeyCodes {
            sendKeyEvent(hid: hid, isDown: false, isRepeat: false, flags: [])
        }
        heldKeyCodes.removeAll()
        for hid in heldModifiers {
            sendKeyEvent(hid: hid, isDown: false, isRepeat: false, flags: [])
        }
        heldModifiers.removeAll()
        clearMarkedText()
    }

    // MARK: - Mouse & Trackpad

    override func mouseEntered(with event: NSEvent) {
        mouseInside = true
        if gameLockState != .relativeLocked {
            activeCursor.set()
        }
    }

    override func mouseExited(with event: NSEvent) {
        if gameLockState == .confinedVisible {
            confineCursorIfNeeded()
            return
        }
        mouseInside = false
        if gameLockState != .relativeLocked {
            NSCursor.arrow.set()
        }
    }

    override func mouseMoved(with event: NSEvent) {
        if gameLockState == .relativeLocked {
            sendRelativeMove(event: event)
        } else {
            confineCursorIfNeeded()
            sendMouse(event: event, kind: .move, button: .unspecified)
        }
    }

    override func mouseDragged(with event: NSEvent) {
        if gameLockState == .relativeLocked {
            sendRelativeMove(event: event)
        } else {
            confineCursorIfNeeded()
            sendMouse(event: event, kind: .move, button: .left)
        }
    }

    override func rightMouseDragged(with event: NSEvent) {
        if gameLockState == .relativeLocked {
            sendRelativeMove(event: event)
        } else {
            confineCursorIfNeeded()
            sendMouse(event: event, kind: .move, button: .right)
        }
    }

    override func otherMouseDragged(with event: NSEvent) {
        if gameLockState == .relativeLocked {
            sendRelativeMove(event: event)
        } else {
            confineCursorIfNeeded()
            sendMouse(event: event, kind: .move, button: mapOtherButton(event.buttonNumber))
        }
    }

    override func mouseDown(with event: NSEvent) {
        window?.makeFirstResponder(self)
        sendMouse(event: event, kind: .buttonDown, button: .left)
    }

    override func mouseUp(with event: NSEvent) {
        sendMouse(event: event, kind: .buttonUp, button: .left)
    }

    override func rightMouseDown(with event: NSEvent) {
        window?.makeFirstResponder(self)
        sendMouse(event: event, kind: .buttonDown, button: .right)
    }

    override func rightMouseUp(with event: NSEvent) {
        sendMouse(event: event, kind: .buttonUp, button: .right)
    }

    override func otherMouseDown(with event: NSEvent) {
        window?.makeFirstResponder(self)
        sendMouse(event: event, kind: .buttonDown, button: mapOtherButton(event.buttonNumber))
    }

    override func otherMouseUp(with event: NSEvent) {
        sendMouse(event: event, kind: .buttonUp, button: mapOtherButton(event.buttonNumber))
    }

    override func scrollWheel(with event: NSEvent) {
        let (nx, ny) = normalizedPoint(from: event)
        let wheelX: Float
        let wheelY: Float
        if event.hasPreciseScrollingDeltas {
            // Trackpad: ~40pt scroll ~= 120 WHEEL_DELTA.
            // macOS scrollingDeltaY > 0 already matches Windows WHEEL_DELTA > 0 (scroll up).
            // macOS scrollingDeltaX > 0 moves content right (scroll left), opposite of Windows HWHEEL > 0.
            let scale: Float = 3.0
            wheelY = Float(event.scrollingDeltaY) * scale
            wheelX = -Float(event.scrollingDeltaX) * scale
        } else {
            wheelY = Float(event.scrollingDeltaY) * 120.0
            wheelX = -Float(event.scrollingDeltaX) * 120.0
        }
        guard wheelX != 0 || wheelY != 0 else { return }
        let mods = MacKeyToHID.modifierMask(from: event.modifierFlags, cmdMapsToCtrl: cmdMapsToCtrl())
        onSendControl?(.mouseEvent(MouseEvent(
            windowId: windowId,
            kind: .wheel,
            normX: nx,
            normY: ny,
            button: .unspecified,
            wheelDeltaX: wheelX,
            wheelDeltaY: wheelY,
            modifiers: mods,
            isRelative: gameLockState == .relativeLocked
        )))
    }

    private func mapOtherButton(_ buttonNumber: Int) -> MouseButton {
        switch buttonNumber {
        case 2: return .middle
        case 3: return .x1
        case 4: return .x2
        default: return .middle
        }
    }

    private func normalizedPoint(from event: NSEvent) -> (Float, Float) {
        let local = convert(event.locationInWindow, from: nil)
        lastMouseViewPoint = local
        let rect = currentVideoRect
        let w = max(rect.width, 1.0)
        let h = max(rect.height, 1.0)
        var nx = Float((local.x - rect.minX) / w)
        var ny = Float((local.y - rect.minY) / h)
        if gameLockState == .confinedVisible {
            nx = min(max(nx, 0.0), 1.0)
            ny = min(max(ny, 0.0), 1.0)
        }
        return (nx, ny)
    }

    private func sendRelativeMove(event: NSEvent) {
        if let lockedAt = pointerLockedAt, Date().timeIntervalSince(lockedAt) < 0.035 {
            return
        }
        let sens = max(0.1, min(4.0, mouseSensitivity()))
        let dx = Float(event.deltaX) * sens
        let dy = Float(event.deltaY) * sens
        guard dx != 0 || dy != 0 else { return }
        let mods = MacKeyToHID.modifierMask(from: event.modifierFlags, cmdMapsToCtrl: cmdMapsToCtrl())
        onSendControl?(.mouseEvent(MouseEvent(
            windowId: windowId,
            kind: .relativeMove,
            modifiers: mods,
            deltaX: dx,
            deltaY: dy,
            isRelative: true
        )))
    }

    private func sendMouse(event: NSEvent, kind: MouseEventKind, button: MouseButton) {
        let mods = MacKeyToHID.modifierMask(from: event.modifierFlags, cmdMapsToCtrl: cmdMapsToCtrl())
        if gameLockState == .relativeLocked {
            onSendControl?(.mouseEvent(MouseEvent(
                windowId: windowId,
                kind: kind,
                button: button,
                modifiers: mods,
                isRelative: true
            )))
            return
        }
        let (nx, ny) = normalizedPoint(from: event)
        onSendControl?(.mouseEvent(MouseEvent(
            windowId: windowId,
            kind: kind,
            normX: nx,
            normY: ny,
            button: button,
            wheelDeltaX: 0,
            wheelDeltaY: 0,
            modifiers: mods
        )))
    }

    // MARK: - Keyboard & Shortcuts

    override func performKeyEquivalent(with event: NSEvent) -> Bool {
        guard window?.isKeyWindow == true, window?.firstResponder === self else {
            return super.performKeyEquivalent(with: event)
        }
        let flags = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
        guard flags.contains(.command) else {
            return super.performKeyEquivalent(with: event)
        }

        // Cmd+Shift+L toggles Smart 3D Game Mouse Lock locally on the focused remote window.
        if flags == [.command, .shift], event.keyCode == UInt16(kVK_ANSI_L) {
            setGameLockEnabled(!gameLockEnabled)
            return true
        }

        // Keep Cmd+Q (quit WinBridge) and Cmd+, (bring Manager to front) native on macOS.
        if flags == .command {
            if event.keyCode == UInt16(kVK_ANSI_Q) {
                setGameLockEnabled(false)
                releaseAllHeldKeys()
                onRequestQuit?()
                return true
            }
            if event.keyCode == UInt16(kVK_ANSI_Comma) {
                setGameLockEnabled(false)
                releaseAllHeldKeys()
                onOpenManager?()
                return true
            }
        }

        handlePhysicalKeyDown(event)
        return true
    }

    override func flagsChanged(with event: NSEvent) {
        if gameLockEnabled && event.modifierFlags.contains([.control, .option]) {
            setGameLockEnabled(false)
            releaseAllHeldKeys()
            return
        }
        syncModifiers(from: event.modifierFlags)
    }

    override func keyDown(with event: NSEvent) {
        syncModifiers(from: event.modifierFlags)
        let flags = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
        let hasShortcutMod = flags.contains(.command) || flags.contains(.control) || flags.contains(.option)

        // Bypass IME for shortcuts or control keys when no Chinese/IME composition is in progress.
        if !hasMarkedText() && (hasShortcutMod || MacKeyToHID.isNonCharacterControlKey(event.keyCode)) {
            handlePhysicalKeyDown(event)
            return
        }

        currentKeyEvent = event
        imeHandledCurrentKey = false
        hadMarkedTextBeforeKeyDown = hasMarkedText()
        interpretKeyEvents([event])
        currentKeyEvent = nil

        // If IME didn't insert text or manage composition, fall back to raw HID key.
        if !imeHandledCurrentKey && !hadMarkedTextBeforeKeyDown && !hasMarkedText() {
            handlePhysicalKeyDown(event)
        }
    }

    override func keyUp(with event: NSEvent) {
        if let hid = heldKeyCodes.removeValue(forKey: event.keyCode) {
            sendKeyEvent(hid: hid, isDown: false, isRepeat: false, flags: event.modifierFlags)
        }
    }

    private func handlePhysicalKeyDown(_ event: NSEvent) {
        guard let hid = MacKeyToHID.hidUsage(for: event.keyCode) else { return }
        heldKeyCodes[event.keyCode] = hid
        sendKeyEvent(hid: hid, isDown: true, isRepeat: event.isARepeat, flags: event.modifierFlags)
    }

    private func syncModifiers(from flags: NSEvent.ModifierFlags) {
        let raw = flags.rawValue
        let mapCmd = cmdMapsToCtrl()
        var desired = Set<UInt32>()

        if flags.contains(.shift) {
            if (raw & UInt(NX_DEVICERSHIFTKEYMASK)) != 0 { desired.insert(MacKeyToHID.HID.rightShift) }
            if (raw & UInt(NX_DEVICELSHIFTKEYMASK)) != 0 || !desired.contains(MacKeyToHID.HID.rightShift) {
                desired.insert(MacKeyToHID.HID.leftShift)
            }
        }
        if flags.contains(.control) {
            if (raw & UInt(NX_DEVICERCTLKEYMASK)) != 0 { desired.insert(MacKeyToHID.HID.rightControl) }
            if (raw & UInt(NX_DEVICELCTLKEYMASK)) != 0 || !desired.contains(MacKeyToHID.HID.rightControl) {
                desired.insert(MacKeyToHID.HID.leftControl)
            }
        }
        if flags.contains(.option) {
            if (raw & UInt(NX_DEVICERALTKEYMASK)) != 0 { desired.insert(MacKeyToHID.HID.rightAlt) }
            if (raw & UInt(NX_DEVICELALTKEYMASK)) != 0 || !desired.contains(MacKeyToHID.HID.rightAlt) {
                desired.insert(MacKeyToHID.HID.leftAlt)
            }
        }
        if flags.contains(.command) {
            if mapCmd {
                desired.insert(MacKeyToHID.HID.leftControl)
            } else {
                if (raw & UInt(NX_DEVICERCMDKEYMASK)) != 0 { desired.insert(MacKeyToHID.HID.rightGui) }
                if (raw & UInt(NX_DEVICELCMDKEYMASK)) != 0 || !desired.contains(MacKeyToHID.HID.rightGui) {
                    desired.insert(MacKeyToHID.HID.leftGui)
                }
            }
        }

        // Release modifiers no longer held
        for released in heldModifiers.subtracting(desired) {
            sendKeyEvent(hid: released, isDown: false, isRepeat: false, flags: flags)
        }
        // Press newly held modifiers
        for pressed in desired.subtracting(heldModifiers) {
            sendKeyEvent(hid: pressed, isDown: true, isRepeat: false, flags: flags)
        }
        heldModifiers = desired
    }

    private func sendKeyEvent(hid: UInt32, isDown: Bool, isRepeat: Bool, flags: NSEvent.ModifierFlags) {
        let mask = MacKeyToHID.modifierMask(from: flags, cmdMapsToCtrl: cmdMapsToCtrl())
        onSendControl?(.keyEvent(KeyEvent(
            windowId: windowId,
            isDown: isDown,
            isRepeat: isRepeat,
            hidUsage: hid,
            modifiers: mask
        )))
    }

    // MARK: - NSTextInputClient (Native macOS Chinese / CJK IME)

    func insertText(_ string: Any, replacementRange: NSRange) {
        let text: String
        if let attr = string as? NSAttributedString {
            text = attr.string
        } else if let str = string as? String {
            text = str
        } else {
            return
        }

        let wasComposing = hadMarkedTextBeforeKeyDown || hasMarkedText()
        clearMarkedText()
        imeHandledCurrentKey = true

        guard !text.isEmpty else { return }

        // For plain single ASCII keystrokes without IME composition, send the physical HID scancode
        // so terminals, editors, and games receive exact key down/up events.
        // For Chinese/CJK, emoji, multi-char commits, or IME punctuation, inject via KEYEVENTF_UNICODE.
        if !wasComposing,
           text.utf8.count == 1,
           let scalar = text.unicodeScalars.first,
           scalar.value >= 0x20 && scalar.value < 0x7F,
           let ev = currentKeyEvent,
           MacKeyToHID.hidUsage(for: ev.keyCode) != nil {
            handlePhysicalKeyDown(ev)
        } else {
            onSendControl?(.textInput(TextInput(windowId: windowId, text: text)))
        }
    }

    override func doCommand(by selector: Selector) {
        // Non-character editing commands (Enter, Backspace, Arrows, Escape):
        // If IME was actively composing, the IME already handled the key; otherwise forward raw HID key.
        if !hadMarkedTextBeforeKeyDown, let ev = currentKeyEvent {
            imeHandledCurrentKey = true
            handlePhysicalKeyDown(ev)
        }
    }

    func setMarkedText(_ string: Any, selectedRange: NSRange, replacementRange: NSRange) {
        if let attr = string as? NSAttributedString {
            markedText = NSMutableAttributedString(attributedString: attr)
        } else if let str = string as? String {
            markedText = NSMutableAttributedString(string: str)
        } else {
            markedText = NSMutableAttributedString()
        }
        markedRangeStorage = markedText.length > 0
            ? NSRange(location: 0, length: markedText.length)
            : NSRange(location: NSNotFound, length: 0)
        selectedRangeStorage = selectedRange
        imeHandledCurrentKey = true
    }

    func unmarkText() {
        clearMarkedText()
    }

    func selectedRange() -> NSRange {
        selectedRangeStorage
    }

    func markedRange() -> NSRange {
        markedRangeStorage
    }

    func hasMarkedText() -> Bool {
        markedText.length > 0
    }

    func attributedSubstring(forProposedRange range: NSRange, actualRange: NSRangePointer?) -> NSAttributedString? {
        nil
    }

    func validAttributesForMarkedText() -> [NSAttributedString.Key] {
        []
    }

    func firstRect(forCharacterRange range: NSRange, actualRange: NSRangePointer?) -> NSRect {
        guard let window else { return .zero }
        let clampedX = min(max(lastMouseViewPoint.x, 12), max(12, bounds.width - 12))
        let clampedY = min(max(lastMouseViewPoint.y, 12), max(12, bounds.height - 12))
        let viewRect = NSRect(x: clampedX, y: clampedY, width: 1, height: 18)
        let winRect = convert(viewRect, to: nil)
        return window.convertToScreen(winRect)
    }

    func characterIndex(for point: NSPoint) -> Int {
        0
    }

    private func clearMarkedText() {
        markedText = NSMutableAttributedString()
        markedRangeStorage = NSRange(location: NSNotFound, length: 0)
        selectedRangeStorage = NSRange(location: 0, length: 0)
    }
}
