import AppKit
import Carbon.HIToolbox
import Foundation

struct KeyShortcut: Codable, Equatable, Sendable {
    var keyCode: UInt16
    /// Raw value of masked `NSEvent.ModifierFlags` (`[.command, .shift, .option, .control]`).
    var modifiersRaw: UInt

    static let allowedModifiers: NSEvent.ModifierFlags = [.command, .shift, .option, .control]

    init(keyCode: UInt16, modifiers: NSEvent.ModifierFlags) {
        self.keyCode = keyCode
        self.modifiersRaw = modifiers.intersection(Self.allowedModifiers).rawValue
    }

    var modifierFlags: NSEvent.ModifierFlags {
        NSEvent.ModifierFlags(rawValue: modifiersRaw).intersection(Self.allowedModifiers)
    }

    var carbonModifiers: UInt32 {
        let f = modifierFlags
        var out: UInt32 = 0
        if f.contains(.command) { out |= UInt32(cmdKey) }
        if f.contains(.shift)   { out |= UInt32(shiftKey) }
        if f.contains(.option)  { out |= UInt32(optionKey) }
        if f.contains(.control) { out |= UInt32(controlKey) }
        return out
    }

    func matches(event: NSEvent) -> Bool {
        let flags = event.modifierFlags.intersection(Self.allowedModifiers)
        return event.keyCode == keyCode && flags == modifierFlags
    }

    var menuKeyEquivalent: String {
        Self.menuKeyMap[keyCode] ?? ""
    }

    var displayString: String {
        let f = modifierFlags
        var parts = ""
        if f.contains(.control) { parts += "⌃" }
        if f.contains(.option)  { parts += "⌥" }
        if f.contains(.shift)   { parts += "⇧" }
        if f.contains(.command) { parts += "⌘" }
        parts += Self.displayName(for: keyCode)
        return parts
    }

    static func displayName(for keyCode: UInt16) -> String {
        if let name = keyNameMap[keyCode] {
            return name
        }
        return "Key(\(keyCode))"
    }

    private static let keyNameMap: [UInt16: String] = [
        UInt16(kVK_Space): "Space",
        UInt16(kVK_Return): "⏎",
        UInt16(kVK_Tab): "⇥",
        UInt16(kVK_ANSI_A): "A", UInt16(kVK_ANSI_B): "B", UInt16(kVK_ANSI_C): "C", UInt16(kVK_ANSI_D): "D",
        UInt16(kVK_ANSI_E): "E", UInt16(kVK_ANSI_F): "F", UInt16(kVK_ANSI_G): "G", UInt16(kVK_ANSI_H): "H",
        UInt16(kVK_ANSI_I): "I", UInt16(kVK_ANSI_J): "J", UInt16(kVK_ANSI_K): "K", UInt16(kVK_ANSI_L): "L",
        UInt16(kVK_ANSI_M): "M", UInt16(kVK_ANSI_N): "N", UInt16(kVK_ANSI_O): "O", UInt16(kVK_ANSI_P): "P",
        UInt16(kVK_ANSI_Q): "Q", UInt16(kVK_ANSI_R): "R", UInt16(kVK_ANSI_S): "S", UInt16(kVK_ANSI_T): "T",
        UInt16(kVK_ANSI_U): "U", UInt16(kVK_ANSI_V): "V", UInt16(kVK_ANSI_W): "W", UInt16(kVK_ANSI_X): "X",
        UInt16(kVK_ANSI_Y): "Y", UInt16(kVK_ANSI_Z): "Z",
        UInt16(kVK_ANSI_0): "0", UInt16(kVK_ANSI_1): "1", UInt16(kVK_ANSI_2): "2", UInt16(kVK_ANSI_3): "3",
        UInt16(kVK_ANSI_4): "4", UInt16(kVK_ANSI_5): "5", UInt16(kVK_ANSI_6): "6", UInt16(kVK_ANSI_7): "7",
        UInt16(kVK_ANSI_8): "8", UInt16(kVK_ANSI_9): "9",
        UInt16(kVK_ANSI_Minus): "-", UInt16(kVK_ANSI_Equal): "=",
        UInt16(kVK_ANSI_LeftBracket): "[", UInt16(kVK_ANSI_RightBracket): "]",
        UInt16(kVK_ANSI_Backslash): "\\", UInt16(kVK_ANSI_Semicolon): ";",
        UInt16(kVK_ANSI_Quote): "'", UInt16(kVK_ANSI_Comma): ",",
        UInt16(kVK_ANSI_Period): ".", UInt16(kVK_ANSI_Slash): "/", UInt16(kVK_ANSI_Grave): "`",
        UInt16(kVK_F1): "F1", UInt16(kVK_F2): "F2", UInt16(kVK_F3): "F3", UInt16(kVK_F4): "F4",
        UInt16(kVK_F5): "F5", UInt16(kVK_F6): "F6", UInt16(kVK_F7): "F7", UInt16(kVK_F8): "F8",
        UInt16(kVK_F9): "F9", UInt16(kVK_F10): "F10", UInt16(kVK_F11): "F11", UInt16(kVK_F12): "F12",
    ]

    private static let menuKeyMap: [UInt16: String] = [
        UInt16(kVK_Space): " ",
        UInt16(kVK_ANSI_A): "a", UInt16(kVK_ANSI_B): "b", UInt16(kVK_ANSI_C): "c", UInt16(kVK_ANSI_D): "d",
        UInt16(kVK_ANSI_E): "e", UInt16(kVK_ANSI_F): "f", UInt16(kVK_ANSI_G): "g", UInt16(kVK_ANSI_H): "h",
        UInt16(kVK_ANSI_I): "i", UInt16(kVK_ANSI_J): "j", UInt16(kVK_ANSI_K): "k", UInt16(kVK_ANSI_L): "l",
        UInt16(kVK_ANSI_M): "m", UInt16(kVK_ANSI_N): "n", UInt16(kVK_ANSI_O): "o", UInt16(kVK_ANSI_P): "p",
        UInt16(kVK_ANSI_Q): "q", UInt16(kVK_ANSI_R): "r", UInt16(kVK_ANSI_S): "s", UInt16(kVK_ANSI_T): "t",
        UInt16(kVK_ANSI_U): "u", UInt16(kVK_ANSI_V): "v", UInt16(kVK_ANSI_W): "w", UInt16(kVK_ANSI_X): "x",
        UInt16(kVK_ANSI_Y): "y", UInt16(kVK_ANSI_Z): "z",
        UInt16(kVK_ANSI_0): "0", UInt16(kVK_ANSI_1): "1", UInt16(kVK_ANSI_2): "2", UInt16(kVK_ANSI_3): "3",
        UInt16(kVK_ANSI_4): "4", UInt16(kVK_ANSI_5): "5", UInt16(kVK_ANSI_6): "6", UInt16(kVK_ANSI_7): "7",
        UInt16(kVK_ANSI_8): "8", UInt16(kVK_ANSI_9): "9",
        UInt16(kVK_ANSI_Minus): "-", UInt16(kVK_ANSI_Equal): "=",
        UInt16(kVK_ANSI_LeftBracket): "[", UInt16(kVK_ANSI_RightBracket): "]",
        UInt16(kVK_ANSI_Comma): ",", UInt16(kVK_ANSI_Period): ".", UInt16(kVK_ANSI_Slash): "/",
    ]
}

struct ShortcutBindings: Codable, Equatable, Sendable {
    var quickLauncher: KeyShortcut
    var pinOnTop: KeyShortcut
    var muteWindow: KeyShortcut
    var toggleHud: KeyShortcut
    var toggleGameLock: KeyShortcut
    var pullClipboardFiles: KeyShortcut

    static let defaults = ShortcutBindings(
        quickLauncher: KeyShortcut(keyCode: UInt16(kVK_Space), modifiers: [.option]),
        pinOnTop: KeyShortcut(keyCode: UInt16(kVK_ANSI_P), modifiers: [.command, .shift]),
        muteWindow: KeyShortcut(keyCode: UInt16(kVK_ANSI_M), modifiers: [.command, .shift]),
        toggleHud: KeyShortcut(keyCode: UInt16(kVK_ANSI_H), modifiers: [.command, .shift]),
        toggleGameLock: KeyShortcut(keyCode: UInt16(kVK_ANSI_L), modifiers: [.command, .shift]),
        pullClipboardFiles: KeyShortcut(keyCode: UInt16(kVK_ANSI_V), modifiers: [.command, .shift, .option])
    )
}
