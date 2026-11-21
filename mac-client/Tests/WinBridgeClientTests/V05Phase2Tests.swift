import AppKit
import Foundation
import Testing
@testable import WinBridgeCore

@Suite("v0.5 Phase 2 — Dual-Address Failover & Window Layout Memory")
struct V05Phase2Tests {
    @Test("HostProfile decodes legacy v0.4 JSON without fallbackHost cleanly")
    func hostProfileBackwardCompatibleDecode() throws {
        let legacyJSON = """
        [
          {
            "id": "default",
            "name": "Home PC",
            "host": "192.168.1.50",
            "port": 9876,
            "useTls": true,
            "macAddress": "AA:BB:CC:DD:EE:FF"
          }
        ]
        """
        let decoded = try JSONDecoder().decode([HostProfile].self, from: Data(legacyJSON.utf8))
        #expect(decoded.count == 1)
        #expect(decoded[0].host == "192.168.1.50")
        #expect(decoded[0].fallbackHost == "")
        #expect(decoded[0].macAddress == "AA:BB:CC:DD:EE:FF")
    }

    @Test("PairingCode parses fallback / backup query parameter")
    func pairingCodeFallbackParam() {
        let parsed = PairingCode.parse(
            "winbridge://192.168.1.50:9876?fallback=100.64.12.34&token=abc&tls=1"
        )
        #expect(parsed?.host == "192.168.1.50")
        #expect(parsed?.fallbackHost == "100.64.12.34")
        #expect(parsed?.token == "abc")

        let alias = PairingCode.parse("winbridge://192.168.1.50?backup=win-pc.ts.net")
        #expect(alias?.fallbackHost == "win-pc.ts.net")
    }

    @Test("Dual-address failover policy falls over on timeout/network errors but never on security rejections")
    func failoverHostPolicy() {
        #expect(WinBridgeConnection.shouldFailoverToFallbackHost(
            primaryHost: "192.168.1.50",
            fallbackHost: "100.64.12.34",
            error: ConnectionError.connectTimeout
        ))
        #expect(WinBridgeConnection.shouldFailoverToFallbackHost(
            primaryHost: "192.168.1.50",
            fallbackHost: "100.64.12.34",
            error: ConnectionError.connectionClosed
        ))
        // Never bypass auth rejection, MITM cert mismatch, or macOS local network denial.
        #expect(!WinBridgeConnection.shouldFailoverToFallbackHost(
            primaryHost: "192.168.1.50",
            fallbackHost: "100.64.12.34",
            error: ConnectionError.handshakeRejected("bad token")
        ))
        #expect(!WinBridgeConnection.shouldFailoverToFallbackHost(
            primaryHost: "192.168.1.50",
            fallbackHost: "100.64.12.34",
            error: ConnectionError.certificateMismatch(hostKey: "k", expected: "A", actual: "B")
        ))
        #expect(!WinBridgeConnection.shouldFailoverToFallbackHost(
            primaryHost: "192.168.1.50",
            fallbackHost: "100.64.12.34",
            error: ConnectionError.localNetworkProhibited
        ))
        // Empty or identical fallback host must not retry.
        #expect(!WinBridgeConnection.shouldFailoverToFallbackHost(
            primaryHost: "192.168.1.50",
            fallbackHost: "",
            error: ConnectionError.connectTimeout
        ))
        #expect(!WinBridgeConnection.shouldFailoverToFallbackHost(
            primaryHost: "192.168.1.50",
            fallbackHost: "192.168.1.50",
            error: ConnectionError.connectTimeout
        ))
    }

    @Test("SavedWindowLayout clamps off-screen frames safely when an external display is unplugged")
    func savedWindowLayoutClamping() {
        let laptop = NSRect(x: 0, y: 0, width: 1440, height: 900)
        let external = NSRect(x: 1440, y: 0, width: 2560, height: 1440)

        // Window on attached external display stays intact.
        let onExternal = NSRect(x: 1600, y: 200, width: 1000, height: 700)
        #expect(SavedWindowLayout.clamp(onExternal, toScreens: [laptop, external]) == onExternal)

        // When external display is unplugged, window clamps back inside laptop screen.
        let clampedAfterUnplug = SavedWindowLayout.clamp(onExternal, toScreens: [laptop])
        #expect(clampedAfterUnplug.minX >= laptop.minX)
        #expect(clampedAfterUnplug.maxX <= laptop.maxX)
        #expect(clampedAfterUnplug.minY >= laptop.minY)
        #expect(clampedAfterUnplug.maxY <= laptop.maxY)
        #expect(clampedAfterUnplug.width == 1000)
        #expect(clampedAfterUnplug.height == 700)

        // Oversized window shrinks to fit screen bounds.
        let oversized = NSRect(x: -500, y: -200, width: 3000, height: 2000)
        let fitted = SavedWindowLayout.clamp(oversized, toScreens: [laptop])
        #expect(fitted == laptop)
    }

    @Test("WorkspacePreset JSON round-trip and AppState layoutKey rules")
    func workspacePresetAndLayoutKey() throws {
        let preset = WorkspacePreset(
            id: "dev-1",
            name: "开发双屏",
            entries: [
                WorkspacePresetEntry(
                    isDesktop: false,
                    processName: "Code.exe",
                    titleHint: "winbridge",
                    layout: SavedWindowLayout(
                        rect: NSRect(x: 100, y: 120, width: 1200, height: 800),
                        isPinnedOnTop: true,
                        isAudioMuted: false
                    )
                ),
                WorkspacePresetEntry(
                    isDesktop: true,
                    processName: "",
                    titleHint: "",
                    layout: SavedWindowLayout(
                        rect: NSRect(x: 40, y: 40, width: 1280, height: 720),
                        isPinnedOnTop: false,
                        isAudioMuted: true
                    )
                ),
            ]
        )
        let data = try JSONEncoder().encode(preset)
        let decoded = try JSONDecoder().decode(WorkspacePreset.self, from: data)
        #expect(decoded == preset)

        let normalWin = WindowInfo(
            windowId: 0x1001, processName: "Code.exe", title: "VS Code", kind: .normal
        )
        let popupWin = WindowInfo(
            windowId: 0x1002, ownerWindowId: 0x1001, processName: "Code.exe", title: "Menu", kind: .popup
        )
        let desktopWin = WindowInfo(
            windowId: SyntheticWindow.primaryDesktop, processName: "", title: "Desktop", kind: .normal
        )
        #expect(AppState.layoutKey(for: normalWin) == "proc:code.exe")
        #expect(AppState.layoutKey(for: popupWin) == nil)
        #expect(AppState.layoutKey(for: desktopWin) == "__desktop__")
    }
}
