import AppKit
import Carbon.HIToolbox
import Foundation

/// Translates macOS Carbon/AppKit virtual keyCodes into platform-neutral
/// USB HID Usage Page 0x07 (Keyboard/Keypad) codes matching `win-host/src/input/hid_scancode.rs`.
enum MacKeyToHID {
    enum HID {
        static let leftControl: UInt32  = 0xE0
        static let leftShift: UInt32    = 0xE1
        static let leftAlt: UInt32      = 0xE2
        static let leftGui: UInt32      = 0xE3
        static let rightControl: UInt32 = 0xE4
        static let rightShift: UInt32   = 0xE5
        static let rightAlt: UInt32     = 0xE6
        static let rightGui: UInt32     = 0xE7
    }

    private static let table: [UInt16: UInt32] = [
        // Letters (ANSI)
        UInt16(kVK_ANSI_A): 0x04,
        UInt16(kVK_ANSI_B): 0x05,
        UInt16(kVK_ANSI_C): 0x06,
        UInt16(kVK_ANSI_D): 0x07,
        UInt16(kVK_ANSI_E): 0x08,
        UInt16(kVK_ANSI_F): 0x09,
        UInt16(kVK_ANSI_G): 0x0A,
        UInt16(kVK_ANSI_H): 0x0B,
        UInt16(kVK_ANSI_I): 0x0C,
        UInt16(kVK_ANSI_J): 0x0D,
        UInt16(kVK_ANSI_K): 0x0E,
        UInt16(kVK_ANSI_L): 0x0F,
        UInt16(kVK_ANSI_M): 0x10,
        UInt16(kVK_ANSI_N): 0x11,
        UInt16(kVK_ANSI_O): 0x12,
        UInt16(kVK_ANSI_P): 0x13,
        UInt16(kVK_ANSI_Q): 0x14,
        UInt16(kVK_ANSI_R): 0x15,
        UInt16(kVK_ANSI_S): 0x16,
        UInt16(kVK_ANSI_T): 0x17,
        UInt16(kVK_ANSI_U): 0x18,
        UInt16(kVK_ANSI_V): 0x19,
        UInt16(kVK_ANSI_W): 0x1A,
        UInt16(kVK_ANSI_X): 0x1B,
        UInt16(kVK_ANSI_Y): 0x1C,
        UInt16(kVK_ANSI_Z): 0x1D,

        // Number row
        UInt16(kVK_ANSI_1): 0x1E,
        UInt16(kVK_ANSI_2): 0x1F,
        UInt16(kVK_ANSI_3): 0x20,
        UInt16(kVK_ANSI_4): 0x21,
        UInt16(kVK_ANSI_5): 0x22,
        UInt16(kVK_ANSI_6): 0x23,
        UInt16(kVK_ANSI_7): 0x24,
        UInt16(kVK_ANSI_8): 0x25,
        UInt16(kVK_ANSI_9): 0x26,
        UInt16(kVK_ANSI_0): 0x27,

        // Basic controls & punctuation
        UInt16(kVK_Return): 0x28,
        UInt16(kVK_Escape): 0x29,
        UInt16(kVK_Delete): 0x2A,             // Backspace
        UInt16(kVK_Tab): 0x2B,
        UInt16(kVK_Space): 0x2C,
        UInt16(kVK_ANSI_Minus): 0x2D,
        UInt16(kVK_ANSI_Equal): 0x2E,
        UInt16(kVK_ANSI_LeftBracket): 0x2F,
        UInt16(kVK_ANSI_RightBracket): 0x30,
        UInt16(kVK_ANSI_Backslash): 0x31,
        UInt16(kVK_ANSI_Semicolon): 0x33,
        UInt16(kVK_ANSI_Quote): 0x34,
        UInt16(kVK_ANSI_Grave): 0x35,
        UInt16(kVK_ANSI_Comma): 0x36,
        UInt16(kVK_ANSI_Period): 0x37,
        UInt16(kVK_ANSI_Slash): 0x38,
        UInt16(kVK_CapsLock): 0x39,
        UInt16(kVK_ISO_Section): 0x64,

        // Function keys
        UInt16(kVK_F1): 0x3A,
        UInt16(kVK_F2): 0x3B,
        UInt16(kVK_F3): 0x3C,
        UInt16(kVK_F4): 0x3D,
        UInt16(kVK_F5): 0x3E,
        UInt16(kVK_F6): 0x3F,
        UInt16(kVK_F7): 0x40,
        UInt16(kVK_F8): 0x41,
        UInt16(kVK_F9): 0x42,
        UInt16(kVK_F10): 0x43,
        UInt16(kVK_F11): 0x44,
        UInt16(kVK_F12): 0x45,
        UInt16(kVK_F13): 0x46, // PrintScreen on Windows
        UInt16(kVK_F14): 0x47, // ScrollLock
        UInt16(kVK_F15): 0x48, // Pause

        // Navigation
        UInt16(kVK_Help): 0x49,          // Insert
        UInt16(kVK_Home): 0x4A,
        UInt16(kVK_PageUp): 0x4B,
        UInt16(kVK_ForwardDelete): 0x4C, // Delete
        UInt16(kVK_End): 0x4D,
        UInt16(kVK_PageDown): 0x4E,
        UInt16(kVK_RightArrow): 0x4F,
        UInt16(kVK_LeftArrow): 0x50,
        UInt16(kVK_DownArrow): 0x51,
        UInt16(kVK_UpArrow): 0x52,

        // Keypad
        UInt16(kVK_ANSI_KeypadClear): 0x53,
        UInt16(kVK_ANSI_KeypadDivide): 0x54,
        UInt16(kVK_ANSI_KeypadMultiply): 0x55,
        UInt16(kVK_ANSI_KeypadMinus): 0x56,
        UInt16(kVK_ANSI_KeypadPlus): 0x57,
        UInt16(kVK_ANSI_KeypadEnter): 0x58,
        UInt16(kVK_ANSI_Keypad1): 0x59,
        UInt16(kVK_ANSI_Keypad2): 0x5A,
        UInt16(kVK_ANSI_Keypad3): 0x5B,
        UInt16(kVK_ANSI_Keypad4): 0x5C,
        UInt16(kVK_ANSI_Keypad5): 0x5D,
        UInt16(kVK_ANSI_Keypad6): 0x5E,
        UInt16(kVK_ANSI_Keypad7): 0x5F,
        UInt16(kVK_ANSI_Keypad8): 0x60,
        UInt16(kVK_ANSI_Keypad9): 0x61,
        UInt16(kVK_ANSI_Keypad0): 0x62,
        UInt16(kVK_ANSI_KeypadDecimal): 0x63,
        UInt16(kVK_ANSI_KeypadEquals): 0x67,

        // Modifiers
        UInt16(kVK_Control): HID.leftControl,
        UInt16(kVK_Shift): HID.leftShift,
        UInt16(kVK_Option): HID.leftAlt,
        UInt16(kVK_Command): HID.leftGui,
        UInt16(kVK_RightControl): HID.rightControl,
        UInt16(kVK_RightShift): HID.rightShift,
        UInt16(kVK_RightOption): HID.rightAlt,
        UInt16(kVK_RightCommand): HID.rightGui,
    ]

    static func hidUsage(for keyCode: UInt16) -> UInt32? {
        table[keyCode]
    }

    static func modifierMask(from flags: NSEvent.ModifierFlags, cmdMapsToCtrl: Bool) -> UInt32 {
        var mask: KeyModifierMask = []
        let raw = flags.rawValue
        if flags.contains(.shift) {
            if (raw & UInt(NX_DEVICERSHIFTKEYMASK)) != 0 { mask.insert(.rShift) }
            if (raw & UInt(NX_DEVICELSHIFTKEYMASK)) != 0 || !mask.contains(.rShift) { mask.insert(.lShift) }
        }
        if flags.contains(.control) {
            if (raw & UInt(NX_DEVICERCTLKEYMASK)) != 0 { mask.insert(.rCtrl) }
            if (raw & UInt(NX_DEVICELCTLKEYMASK)) != 0 || !mask.contains(.rCtrl) { mask.insert(.lCtrl) }
        }
        if flags.contains(.option) {
            if (raw & UInt(NX_DEVICERALTKEYMASK)) != 0 { mask.insert(.rAlt) }
            if (raw & UInt(NX_DEVICELALTKEYMASK)) != 0 || !mask.contains(.rAlt) { mask.insert(.lAlt) }
        }
        if flags.contains(.command) {
            if cmdMapsToCtrl {
                mask.insert(.lCtrl)
            } else {
                if (raw & UInt(NX_DEVICERCMDKEYMASK)) != 0 { mask.insert(.rWin) }
                if (raw & UInt(NX_DEVICELCMDKEYMASK)) != 0 || !mask.contains(.rWin) { mask.insert(.lWin) }
            }
        }
        return mask.rawValue
    }

    /// True for keys that should bypass IME when no marked text is active.
    static func isNonCharacterControlKey(_ keyCode: UInt16) -> Bool {
        switch Int(keyCode) {
        case kVK_Escape, kVK_Return, kVK_ANSI_KeypadEnter, kVK_Tab,
             kVK_Delete, kVK_ForwardDelete,
             kVK_LeftArrow, kVK_RightArrow, kVK_UpArrow, kVK_DownArrow,
             kVK_Home, kVK_End, kVK_PageUp, kVK_PageDown, kVK_Help,
             kVK_F1, kVK_F2, kVK_F3, kVK_F4, kVK_F5, kVK_F6,
             kVK_F7, kVK_F8, kVK_F9, kVK_F10, kVK_F11, kVK_F12,
             kVK_F13, kVK_F14, kVK_F15:
            return true
        default:
            return false
        }
    }
}
