import AppKit
import Carbon.HIToolbox
import Foundation
import Testing
@testable import WinBridgeCore

@Suite("v0.5 Phase 1 — Customizable Shortcuts & Display Formatting")
struct V05Phase1Tests {
    @Test("Default shortcut bindings format expected macOS glyphs, Carbon modifiers, and JSON round-trip")
    func shortcutBindingsRoundTrip() throws {
        let defaults = ShortcutBindings.defaults
        #expect(defaults.quickLauncher.displayString == "⌥Space")
        #expect(defaults.quickLauncher.carbonModifiers == UInt32(optionKey))
        #expect(defaults.pinOnTop.displayString == "⇧⌘P")
        #expect(defaults.muteWindow.displayString == "⇧⌘M")
        #expect(defaults.toggleHud.displayString == "⇧⌘H")
        #expect(defaults.toggleGameLock.displayString == "⇧⌘L")
        #expect(defaults.pullClipboardFiles.displayString == "⌥⇧⌘V")
        #expect(defaults.pinOnTop.menuKeyEquivalent == "p")

        var custom = defaults
        custom.quickLauncher = KeyShortcut(keyCode: UInt16(kVK_ANSI_K), modifiers: [.control, .option])
        #expect(custom.quickLauncher.displayString == "⌃⌥K")
        #expect(custom.quickLauncher.carbonModifiers == (UInt32(controlKey) | UInt32(optionKey)))

        let encoded = try JSONEncoder().encode(custom)
        let decoded = try JSONDecoder().decode(ShortcutBindings.self, from: encoded)
        #expect(decoded == custom)
    }
}
