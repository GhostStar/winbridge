import Foundation
import Testing
@testable import WinBridgeCore

@Suite("Phase B — Spotlight Quick Launcher Fuzzy Search & Ranking")
struct PhaseBTests {
    private static func makeWindow(id: UInt64, title: String, process: String) -> WindowInfo {
        WindowInfo(
            windowId: id,
            ownerWindowId: 0,
            pid: UInt32(id),
            processName: process,
            title: title,
            kind: .normal,
            x: 0,
            y: 0,
            width: 1280,
            height: 720,
            offsetX: 0,
            offsetY: 0,
            dpi: 96,
            isMinimized: false,
            isMaximized: false,
            isResizable: true,
            hasAlpha: false,
            zOrder: 1,
            iconPng: Data()
        )
    }

    @Test("Fuzzy matcher scores exact, prefix, substring, and initials cleanly")
    func fuzzyMatchScoring() {
        let exact = QuickLauncherMatcher.matchScore(query: "code", primary: "Code")
        let prefix = QuickLauncherMatcher.matchScore(query: "code", primary: "Code Insiders")
        let sub = QuickLauncherMatcher.matchScore(query: "code", primary: "Visual Studio Code")
        let fuzzy = QuickLauncherMatcher.matchScore(query: "vsc", primary: "Visual Studio Code")
        let miss = QuickLauncherMatcher.matchScore(query: "xyz", primary: "Visual Studio Code")

        #expect(exact != nil && prefix != nil && sub != nil && fuzzy != nil)
        #expect(miss == nil)
        #expect(exact! > prefix!)
        #expect(prefix! > sub!)
        #expect(sub! > fuzzy!)

        // Secondary process name matching works too
        let procMatch = QuickLauncherMatcher.matchScore(query: "wechat", primary: "微信", secondary: "WeChat.exe")
        #expect(procMatch != nil)
    }

    @Test("Search prioritizes open Windows windows above apps and handles offline actions")
    func searchRankingAndOfflineActions() {
        let w1 = Self.makeWindow(id: 101, title: "README.md - Visual Studio Code", process: "Code.exe")
        let w2 = Self.makeWindow(id: 102, title: "Google Chrome", process: "chrome.exe")
        let apps = [
            AppEntry(appId: "a1", name: "Visual Studio Code", source: "Start Menu", iconPng: Data()),
            AppEntry(appId: "a2", name: "Notepad", source: "Start Menu", iconPng: Data()),
            AppEntry(appId: "a3", name: "Steam", source: "Desktop", iconPng: Data()),
        ]

        // Empty query puts subscribed windows first, then other open windows, desktop, pinned apps, other apps
        let emptyResults = QuickLauncherMatcher.search(
            query: "",
            isConnected: true,
            host: "192.168.1.10",
            port: 9876,
            macAddress: nil,
            windows: [w1, w2],
            subscribedRootIds: [102],
            apps: apps,
            pinnedAppIds: ["a3"],
            isDesktopOpen: false
        )
        #expect(emptyResults.first == .window(w2, isSubscribed: true))
        #expect(emptyResults[1] == .window(w1, isSubscribed: false))
        #expect(emptyResults[2] == .desktop(isOpen: false))
        #expect(emptyResults[3] == .app(apps[2], isPinned: true))

        // Query "vsc" matches both open VS Code window and installed VS Code app, keeping open window on top
        let vscResults = QuickLauncherMatcher.search(
            query: "vsc",
            isConnected: true,
            host: "192.168.1.10",
            port: 9876,
            macAddress: nil,
            windows: [w1, w2],
            subscribedRootIds: [101],
            apps: apps,
            pinnedAppIds: [],
            isDesktopOpen: false
        )
        #expect(vscResults.count == 2)
        #expect(vscResults[0] == .window(w1, isSubscribed: true))
        #expect(vscResults[1] == .app(apps[0], isPinned: false))

        // Offline mode exposes quick connect and Wake-on-LAN actions
        let offline = QuickLauncherMatcher.search(
            query: "",
            isConnected: false,
            host: "192.168.1.10",
            port: 9876,
            macAddress: "aa-bb-cc-dd-ee-ff",
            windows: [],
            subscribedRootIds: [],
            apps: [],
            pinnedAppIds: [],
            isDesktopOpen: false
        )
        #expect(offline == [
            .connectHost(host: "192.168.1.10", port: 9876),
            .wakeHost(macAddress: "AA:BB:CC:DD:EE:FF", host: "192.168.1.10"),
        ])
    }
}
