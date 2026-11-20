import Carbon.HIToolbox
import Testing
@testable import WinBridgeCore

@Test func coreMacKeyCodesMapToExpectedUSBHID() {
    #expect(MacKeyToHID.hidUsage(for: UInt16(kVK_ANSI_A)) == 0x04)
    #expect(MacKeyToHID.hidUsage(for: UInt16(kVK_ANSI_Z)) == 0x1D)
    #expect(MacKeyToHID.hidUsage(for: UInt16(kVK_Return)) == 0x28)
    #expect(MacKeyToHID.hidUsage(for: UInt16(kVK_Escape)) == 0x29)
    #expect(MacKeyToHID.hidUsage(for: UInt16(kVK_Delete)) == 0x2A)        // Backspace
    #expect(MacKeyToHID.hidUsage(for: UInt16(kVK_ForwardDelete)) == 0x4C) // Delete
    #expect(MacKeyToHID.hidUsage(for: UInt16(kVK_UpArrow)) == 0x52)
    #expect(MacKeyToHID.hidUsage(for: UInt16(kVK_RightControl)) == 0xE4)
    #expect(MacKeyToHID.hidUsage(for: UInt16(kVK_ANSI_KeypadEnter)) == 0x58)
}
