import AppKit
import Carbon.HIToolbox
import CoreGraphics
import CoreVideo
import Foundation
import QuartzCore

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
    var onPointerLockChanged: ((Bool) -> Void)?

    private(set) var isPointerLocked = false
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

    func applyCursor(_ cursor: NSCursor) {
        activeCursor = cursor
        if mouseInside && !isPointerLocked {
            cursor.set()
        }
        window?.invalidateCursorRects(for: self)
    }

    override func cursorUpdate(with event: NSEvent) {
        if !isPointerLocked {
            activeCursor.set()
        }
    }

    override func resetCursorRects() {
        addCursorRect(bounds, cursor: activeCursor)
    }

    // MARK: - 3D Game Relative Mouse Lock

    func setPointerLocked(_ locked: Bool) {
        guard locked != isPointerLocked else { return }
        if locked {
            guard let window else { return }
            window.makeKeyAndOrderFront(nil)
            window.makeFirstResponder(self)

            // Center Windows cursor inside the target window first, then pin macOS cursor to
            // the window's center and disassociate hardware motion so it can never leave bounds.
            onSendControl?(.focusWindow(FocusWindow(windowId: windowId, focused: true)))
            onSendControl?(.mouseEvent(MouseEvent(
                windowId: windowId,
                kind: .move,
                normX: 0.5,
                normY: 0.5
            )))

            let rect = currentVideoRect
            let centerInWin = convert(NSPoint(x: rect.midX, y: rect.midY), to: nil)
            let centerOnScreen = window.convertPoint(toScreen: centerInWin)
            let primaryHeight = NSScreen.screens.first?.frame.height
                ?? CGDisplayBounds(CGMainDisplayID()).height
            let cgCenter = CGPoint(x: centerOnScreen.x, y: primaryHeight - centerOnScreen.y)

            CGWarpMouseCursorPosition(cgCenter)
            CGAssociateMouseAndMouseCursorPosition(0)
            if !isCursorHidden {
                NSCursor.hide()
                isCursorHidden = true
            }
            isPointerLocked = true
            pointerLockedAt = Date()
            ClientLogger.shared.info("Pointer lock ENABLED for window 0x\(String(windowId, radix: 16))")
            onPointerLockChanged?(true)
        } else {
            isPointerLocked = false
            pointerLockedAt = nil
            CGAssociateMouseAndMouseCursorPosition(1)
            if isCursorHidden {
                NSCursor.unhide()
                isCursorHidden = false
            }
            ClientLogger.shared.info("Pointer lock DISABLED for window 0x\(String(windowId, radix: 16))")
            onPointerLockChanged?(false)
        }
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
        if !isPointerLocked {
            activeCursor.set()
        }
    }

    override func mouseExited(with event: NSEvent) {
        mouseInside = false
        if !isPointerLocked {
            NSCursor.arrow.set()
        }
    }

    override func mouseMoved(with event: NSEvent) {
        if isPointerLocked {
            sendRelativeMove(event: event)
        } else {
            sendMouse(event: event, kind: .move, button: .unspecified)
        }
    }

    override func mouseDragged(with event: NSEvent) {
        if isPointerLocked {
            sendRelativeMove(event: event)
        } else {
            sendMouse(event: event, kind: .move, button: .left)
        }
    }

    override func rightMouseDragged(with event: NSEvent) {
        if isPointerLocked {
            sendRelativeMove(event: event)
        } else {
            sendMouse(event: event, kind: .move, button: .right)
        }
    }

    override func otherMouseDragged(with event: NSEvent) {
        if isPointerLocked {
            sendRelativeMove(event: event)
        } else {
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
            isRelative: isPointerLocked
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
        return (Float((local.x - rect.minX) / w), Float((local.y - rect.minY) / h))
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
        if isPointerLocked {
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

        // Cmd+Shift+L toggles 3D game Mouse Lock locally on the focused remote window.
        if flags == [.command, .shift], event.keyCode == UInt16(kVK_ANSI_L) {
            setPointerLocked(!isPointerLocked)
            return true
        }

        // Keep Cmd+Q (quit WinBridge) and Cmd+, (bring Manager to front) native on macOS.
        if flags == .command {
            if event.keyCode == UInt16(kVK_ANSI_Q) {
                setPointerLocked(false)
                releaseAllHeldKeys()
                onRequestQuit?()
                return true
            }
            if event.keyCode == UInt16(kVK_ANSI_Comma) {
                setPointerLocked(false)
                releaseAllHeldKeys()
                onOpenManager?()
                return true
            }
        }

        handlePhysicalKeyDown(event)
        return true
    }

    override func flagsChanged(with event: NSEvent) {
        if isPointerLocked && event.modifierFlags.contains([.control, .option]) {
            setPointerLocked(false)
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
