import Carbon.HIToolbox
import Foundation

/// Registers a system-wide macOS hotkey via Carbon `RegisterEventHotKey`.
/// Requires zero Accessibility or Input Monitoring permissions and intercepts keys
/// even when another app or a remote Windows window has focus.
@MainActor
final class GlobalHotKey {
    private var hotKeyRef: EventHotKeyRef?
    private var handlerRef: EventHandlerRef?
    private var callback: (() -> Void)?

    private static var activeInstance: GlobalHotKey?

    /// Registers `Option + Space` (`kVK_Space` + `optionKey`).
    @discardableResult
    func registerOptionSpace(handler: @escaping () -> Void) -> Bool {
        unregister()
        self.callback = handler
        Self.activeInstance = self

        var eventSpec = EventTypeSpec(
            eventClass: OSType(kEventClassKeyboard),
            eventKind: UInt32(kEventHotKeyPressed)
        )

        let statusInstall = InstallEventHandler(
            GetApplicationEventTarget(),
            { _, eventRef, _ -> OSStatus in
                var hotKeyID = EventHotKeyID()
                let err = GetEventParameter(
                    eventRef,
                    EventParamName(kEventParamDirectObject),
                    EventParamType(typeEventHotKeyID),
                    nil,
                    MemoryLayout<EventHotKeyID>.size,
                    nil,
                    &hotKeyID
                )
                if err == noErr && hotKeyID.signature == GlobalHotKey.signature && hotKeyID.id == GlobalHotKey.hotKeyId {
                    DispatchQueue.main.async {
                        GlobalHotKey.activeInstance?.callback?()
                    }
                    return noErr
                }
                return OSStatus(eventNotHandledErr)
            },
            1,
            &eventSpec,
            nil,
            &handlerRef
        )
        guard statusInstall == noErr else {
            ClientLogger.shared.error("GlobalHotKey InstallEventHandler failed: \(statusInstall)")
            unregister()
            return false
        }

        let id = EventHotKeyID(signature: Self.signature, id: Self.hotKeyId)
        let statusReg = RegisterEventHotKey(
            UInt32(kVK_Space),
            UInt32(optionKey),
            id,
            GetApplicationEventTarget(),
            0,
            &hotKeyRef
        )
        guard statusReg == noErr else {
            ClientLogger.shared.error("GlobalHotKey RegisterEventHotKey(Option+Space) failed: \(statusReg)")
            unregister()
            return false
        }
        ClientLogger.shared.info("Registered global hotkey Option+Space for Quick Launcher")
        return true
    }

    func unregister() {
        if let ref = hotKeyRef {
            UnregisterEventHotKey(ref)
            hotKeyRef = nil
        }
        if let handler = handlerRef {
            RemoveEventHandler(handler)
            handlerRef = nil
        }
        callback = nil
        if Self.activeInstance === self {
            Self.activeInstance = nil
        }
    }

    // 'WBRG' four-char OSType signature
    private static let signature: OSType = 0x5742_5247
    private static let hotKeyId: UInt32 = 1
}
