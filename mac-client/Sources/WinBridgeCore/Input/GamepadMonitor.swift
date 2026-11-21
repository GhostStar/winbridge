import Foundation
import GameController

/// Captures connected macOS GameController devices (Xbox, DualSense, Switch Pro, MFi) and maps
/// them to Windows XInput (`XUSB_GAMEPAD_*`) state packets for virtual Xbox 360 passthrough.
@MainActor
final class GamepadMonitor {
    var shouldForward: () -> Bool = { false }
    var onGamepadState: ((GamepadState) -> Void)?

    private var slots: [ObjectIdentifier: UInt32] = [:]
    private var lastSent: [UInt32: GamepadState] = [:]
    private var pollTimer: Timer?
    private var started = false

    func start() {
        guard !started else { return }
        started = true
        NotificationCenter.default.addObserver(
            self,
            selector: #selector(controllerConnected(_:)),
            name: .GCControllerDidConnect,
            object: nil
        )
        NotificationCenter.default.addObserver(
            self,
            selector: #selector(controllerDisconnected(_:)),
            name: .GCControllerDidDisconnect,
            object: nil
        )
        for controller in GCController.controllers() {
            assignSlot(for: controller)
        }
        pollTimer = Timer.scheduledTimer(withTimeInterval: 1.0 / 120.0, repeats: true) { [weak self] _ in
            Task { @MainActor in
                self?.pollControllers()
            }
        }
    }

    func stop() {
        guard started else { return }
        started = false
        pollTimer?.invalidate()
        pollTimer = nil
        NotificationCenter.default.removeObserver(self)
        for (_, idx) in slots {
            onGamepadState?(GamepadState(index: idx, connected: false))
        }
        slots.removeAll()
        lastSent.removeAll()
    }

    @objc private func controllerConnected(_ note: Notification) {
        guard let controller = note.object as? GCController else { return }
        assignSlot(for: controller)
    }

    @objc private func controllerDisconnected(_ note: Notification) {
        guard let controller = note.object as? GCController else { return }
        let key = ObjectIdentifier(controller)
        if let idx = slots.removeValue(forKey: key) {
            lastSent.removeValue(forKey: idx)
            onGamepadState?(GamepadState(index: idx, connected: false))
        }
    }

    private func assignSlot(for controller: GCController) {
        guard controller.extendedGamepad != nil else { return }
        let key = ObjectIdentifier(controller)
        if slots[key] != nil { return }
        let used = Set(slots.values)
        guard let free = (0..<UInt32(4)).first(where: { !used.contains($0) }) else { return }
        slots[key] = free
    }

    private func pollControllers() {
        guard shouldForward() else { return }
        for controller in GCController.controllers() {
            guard let gp = controller.extendedGamepad else { continue }
            let key = ObjectIdentifier(controller)
            if slots[key] == nil {
                assignSlot(for: controller)
            }
            guard let idx = slots[key] else { continue }
            let state = Self.snapshot(index: idx, gamepad: gp)
            if lastSent[idx] != state {
                lastSent[idx] = state
                onGamepadState?(state)
            }
        }
    }

    nonisolated static func axisToInt16(_ value: Float) -> Int32 {
        let clamped = max(-1.0, min(1.0, value))
        if clamped < 0 {
            return Int32(round(clamped * 32768.0))
        }
        return Int32(round(clamped * 32767.0))
    }

    nonisolated static func triggerToUInt8(_ value: Float) -> UInt32 {
        UInt32(round(max(0.0, min(1.0, value)) * 255.0))
    }

    private static func snapshot(index: UInt32, gamepad gp: GCExtendedGamepad) -> GamepadState {
        var buttons: UInt32 = 0
        // XUSB_GAMEPAD_* bitmask definitions from XInput / ViGEm
        if gp.dpad.up.isPressed { buttons |= 0x0001 }
        if gp.dpad.down.isPressed { buttons |= 0x0002 }
        if gp.dpad.left.isPressed { buttons |= 0x0004 }
        if gp.dpad.right.isPressed { buttons |= 0x0008 }
        if gp.buttonMenu.isPressed { buttons |= 0x0010 } // Start
        if gp.buttonOptions?.isPressed == true { buttons |= 0x0020 } // Back
        if gp.leftThumbstickButton?.isPressed == true { buttons |= 0x0040 }
        if gp.rightThumbstickButton?.isPressed == true { buttons |= 0x0080 }
        if gp.leftShoulder.isPressed { buttons |= 0x0100 }
        if gp.rightShoulder.isPressed { buttons |= 0x0200 }
        if gp.buttonHome?.isPressed == true { buttons |= 0x0400 } // Guide
        if gp.buttonA.isPressed { buttons |= 0x1000 }
        if gp.buttonB.isPressed { buttons |= 0x2000 }
        if gp.buttonX.isPressed { buttons |= 0x4000 }
        if gp.buttonY.isPressed { buttons |= 0x8000 }

        return GamepadState(
            index: index,
            connected: true,
            buttons: buttons,
            leftTrigger: triggerToUInt8(gp.leftTrigger.value),
            rightTrigger: triggerToUInt8(gp.rightTrigger.value),
            thumbLX: axisToInt16(gp.leftThumbstick.xAxis.value),
            thumbLY: axisToInt16(gp.leftThumbstick.yAxis.value),
            thumbRX: axisToInt16(gp.rightThumbstick.xAxis.value),
            thumbRY: axisToInt16(gp.rightThumbstick.yAxis.value)
        )
    }
}
