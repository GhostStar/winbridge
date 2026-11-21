import AppKit
import Foundation

@MainActor
public enum WinBridgeAppLauncher {
    private static var retainedDelegate: AppDelegate?

    public static func run() {
        let app = NSApplication.shared
        app.setActivationPolicy(.regular)
        let delegate = AppDelegate()
        retainedDelegate = delegate
        app.delegate = delegate
        app.run()
    }
}

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    private let state = AppState()
    private var managerController: ManagerWindowController?
    private var filePullController: FilePullWindowController?
    private var statusItem: NSStatusItem?
    private var windowMenu: NSMenu?

    func applicationDidFinishLaunching(_ notification: Notification) {
        setupMainMenu()
        setupStatusItem()

        let manager = ManagerWindowController(state: state)
        self.managerController = manager

        state.onStateChanged = { [weak self] in
            self?.managerController?.refreshUI()
            self?.rebuildStatusMenu()
            self?.rebuildWindowMenu()
        }
        state.onOpenManager = { [weak self] in
            self?.showManager()
        }
        state.startDiscovery()

        showManager()
        NSApp.activate(ignoringOtherApps: true)
    }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        if !flag {
            showManager()
        }
        return true
    }

    func applicationWillResignActive(_ notification: Notification) {
        state.unlockAllPointers()
    }

    func applicationWillTerminate(_ notification: Notification) {
        state.stopDiscovery()
        state.unlockAllPointers()
        state.disconnect()
    }

    @objc func showManager() {
        managerController?.showWindow(nil)
        managerController?.window?.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    @objc private func refreshWindowsAction() {
        state.refreshWindowList()
    }

    @objc private func requestKeyframeAction() {
        state.requestKeyFrameForActiveWindow()
    }

    @objc private func showFilePullAction() {
        if filePullController == nil {
            filePullController = FilePullWindowController(state: state)
        }
        filePullController?.show()
    }

    @objc private func detachActiveWindowAction() {
        state.detachActiveWindow()
    }

    @objc private func togglePointerLockAction() {
        state.togglePointerLockForActiveWindow()
    }

    @objc private func toggleAutoGameLockAction() {
        state.toggleAutoGameLockForActiveWindow()
    }

    @objc private func toggleHudAction() {
        state.togglePerformanceHud()
    }

    @objc private func disconnectAction() {
        state.disconnect()
    }

    @objc private func resetTrustAction() {
        state.resetTrustedFingerprintForCurrentHost()
    }

    @objc private func toggleAudioAction() {
        state.toggleAudioEnabled()
    }

    @objc private func toggleAudioProcessOnlyAction() {
        state.toggleAudioProcessOnly()
    }

    @objc private func pullClipboardFilesAction() {
        state.pullClipboardFilesToPasteboard(isAuto: false)
    }

    @objc private func toggleAutoPullClipboardFilesAction() {
        state.toggleAutoPullClipboardFiles()
    }

    @objc private func setVideoProfileAction(_ sender: NSMenuItem) {
        guard let raw = sender.representedObject as? String,
              let profile = VideoProfilePreference(rawValue: raw) else { return }
        state.setVideoProfile(profile)
    }

    @objc private func setProcessVideoProfileAction(_ sender: NSMenuItem) {
        guard let raw = sender.representedObject as? String,
              let profile = VideoProfilePreference(rawValue: raw) else { return }
        state.setVideoProfileForActiveProcess(profile)
    }

    @objc private func toggleAdaptiveBitrateAction() {
        state.toggleAdaptiveBitrate()
    }

    @objc private func toggleDesktopAction() {
        state.toggleDesktopView()
    }

    @objc private func launchPinnedAppAction(_ sender: NSMenuItem) {
        guard let appId = sender.representedObject as? String,
              let app = state.availableApps.first(where: { $0.appId == appId }) else { return }
        state.launchApp(app)
    }

    private func setupMainMenu() {
        let mainMenu = NSMenu()

        // App menu
        let appItem = NSMenuItem()
        let appMenu = NSMenu()
        appMenu.addItem(withTitle: "打开 WinBridge 管理窗口", action: #selector(showManager), keyEquivalent: ",")
        appMenu.addItem(.separator())
        appMenu.addItem(withTitle: "退出 WinBridge", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
        appItem.submenu = appMenu
        mainMenu.addItem(appItem)

        // Edit menu (enables Cmd+C/V/A in Manager IP/Token fields; RemoteWindowView intercepts when focused)
        let editItem = NSMenuItem()
        let editMenu = NSMenu(title: "编辑")
        editMenu.addItem(withTitle: "撤销", action: Selector(("undo:")), keyEquivalent: "z")
        editMenu.addItem(withTitle: "重做", action: Selector(("redo:")), keyEquivalent: "Z")
        editMenu.addItem(.separator())
        editMenu.addItem(withTitle: "剪切", action: #selector(NSText.cut(_:)), keyEquivalent: "x")
        editMenu.addItem(withTitle: "拷贝", action: #selector(NSText.copy(_:)), keyEquivalent: "c")
        editMenu.addItem(withTitle: "粘贴", action: #selector(NSText.paste(_:)), keyEquivalent: "v")
        editMenu.addItem(withTitle: "全选", action: #selector(NSText.selectAll(_:)), keyEquivalent: "a")
        editItem.submenu = editMenu
        mainMenu.addItem(editItem)

        // Window menu
        let winItem = NSMenuItem()
        let winMenu = NSMenu(title: "窗口")
        windowMenu = winMenu
        rebuildWindowMenu()
        winItem.submenu = winMenu
        mainMenu.addItem(winItem)

        NSApp.mainMenu = mainMenu
    }

    private func rebuildWindowMenu() {
        guard let winMenu = windowMenu else { return }
        winMenu.removeAllItems()
        winMenu.addItem(withTitle: "刷新 Windows 窗口列表", action: #selector(refreshWindowsAction), keyEquivalent: "")
        let lockItem = NSMenuItem(
            title: "锁定 / 解锁游戏鼠标 (3D 相对视角)",
            action: #selector(togglePointerLockAction),
            keyEquivalent: "L"
        )
        lockItem.keyEquivalentModifierMask = [.command, .shift]
        winMenu.addItem(lockItem)
        winMenu.addItem(withTitle: "记住当前游戏自动开启鼠标锁定", action: #selector(toggleAutoGameLockAction), keyEquivalent: "")
        let hudItem = NSMenuItem(
            title: "显示 / 隐藏性能 HUD",
            action: #selector(toggleHudAction),
            keyEquivalent: "H"
        )
        hudItem.keyEquivalentModifierMask = [.command, .shift]
        winMenu.addItem(hudItem)
        winMenu.addItem(withTitle: "刷新当前窗口关键帧 (IDR)", action: #selector(requestKeyframeAction), keyEquivalent: "")
        winMenu.addItem(withTitle: "从 Windows 取回文件…", action: #selector(showFilePullAction), keyEquivalent: "")
        let pullClipItem = NSMenuItem(
            title: "取回 Windows 剪贴板文件 (写入 Mac ⌘V)",
            action: #selector(pullClipboardFilesAction),
            keyEquivalent: "V"
        )
        pullClipItem.keyEquivalentModifierMask = [.command, .shift, .option]
        winMenu.addItem(pullClipItem)
        let autoPullItem = NSMenuItem(
            title: "剪贴板文件自动取回 (≤20 MB)",
            action: #selector(toggleAutoPullClipboardFilesAction),
            keyEquivalent: ""
        )
        autoPullItem.state = state.settings.autoPullClipboardFiles ? .on : .off
        winMenu.addItem(autoPullItem)
        appendVideoQualityItems(to: winMenu, includeProcess: true)
        winMenu.addItem(.separator())
        winMenu.addItem(withTitle: "脱离当前远端窗口", action: #selector(detachActiveWindowAction), keyEquivalent: "")
        winMenu.addItem(.separator())
        winMenu.addItem(withTitle: "重置当前主机 TLS 证书信任", action: #selector(resetTrustAction), keyEquivalent: "")
        winMenu.addItem(withTitle: "断开连接", action: #selector(disconnectAction), keyEquivalent: "")
    }

    private func setupStatusItem() {
        let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        if let button = item.button {
            button.image = NSImage(systemSymbolName: "rectangle.on.rectangle.angled", accessibilityDescription: "WinBridge")
            button.toolTip = "WinBridge"
        }
        statusItem = item
        rebuildStatusMenu()
    }

    private func rebuildStatusMenu() {
        let menu = NSMenu()
        switch state.connectionState {
        case .disconnected:
            menu.addItem(withTitle: "状态：未连接", action: nil, keyEquivalent: "")
        case .connecting:
            menu.addItem(withTitle: "状态：连接中…", action: nil, keyEquivalent: "")
        case .connected(let host, let codec):
            menu.addItem(withTitle: "已连接：\(host) (\(codec.displayName))", action: nil, keyEquivalent: "")
        }
        menu.addItem(.separator())
        menu.addItem(withTitle: "打开管理窗口…", action: #selector(showManager), keyEquivalent: "")
        if case .connected = state.connectionState {
            let desktopItem = NSMenuItem(
                title: state.isDesktopOpen ? "关闭 Windows 完整桌面" : "查看 Windows 完整桌面",
                action: #selector(toggleDesktopAction),
                keyEquivalent: ""
            )
            desktopItem.image = NSImage(systemSymbolName: "desktopcomputer", accessibilityDescription: nil)
            menu.addItem(desktopItem)

            let audioItem = NSMenuItem(title: "开启 Windows 音频", action: #selector(toggleAudioAction), keyEquivalent: "")
            audioItem.state = state.settings.audioEnabled ? .on : .off
            menu.addItem(audioItem)
            let processAudio = NSMenuItem(
                title: "只采集已投射进程",
                action: #selector(toggleAudioProcessOnlyAction),
                keyEquivalent: ""
            )
            processAudio.state = state.settings.audioProcessOnly ? .on : .off
            menu.addItem(processAudio)
            menu.addItem(withTitle: "从 Windows 取回文件…", action: #selector(showFilePullAction), keyEquivalent: "")
            menu.addItem(withTitle: "取回 Windows 剪贴板文件 (⌥⇧⌘V)", action: #selector(pullClipboardFilesAction), keyEquivalent: "")
            let autoPullStatus = NSMenuItem(
                title: "剪贴板文件自动取回 (≤20 MB)",
                action: #selector(toggleAutoPullClipboardFilesAction),
                keyEquivalent: ""
            )
            autoPullStatus.state = state.settings.autoPullClipboardFiles ? .on : .off
            menu.addItem(autoPullStatus)
            appendVideoQualityItems(to: menu, includeProcess: true)

            let pinned = state.pinnedApps
            if !pinned.isEmpty {
                menu.addItem(.separator())
                let header = NSMenuItem(title: "快速启动 Windows 应用", action: nil, keyEquivalent: "")
                header.isEnabled = false
                menu.addItem(header)
                for app in pinned {
                    let item = NSMenuItem(title: app.name, action: #selector(launchPinnedAppAction(_:)), keyEquivalent: "")
                    item.representedObject = app.appId
                    if !app.iconPng.isEmpty, let img = NSImage(data: app.iconPng) {
                        img.size = NSSize(width: 16, height: 16)
                        item.image = img
                    }
                    menu.addItem(item)
                }
            }

            menu.addItem(.separator())
            menu.addItem(withTitle: "刷新 Windows 窗口列表", action: #selector(refreshWindowsAction), keyEquivalent: "")
            menu.addItem(withTitle: "断开连接", action: #selector(disconnectAction), keyEquivalent: "")
        }
        menu.addItem(.separator())
        menu.addItem(withTitle: "退出 WinBridge", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "")
        statusItem?.menu = menu
    }

    private func appendVideoQualityItems(to menu: NSMenu, includeProcess: Bool) {
        menu.addItem(.separator())
        let header = NSMenuItem(title: "画质", action: nil, keyEquivalent: "")
        header.isEnabled = false
        menu.addItem(header)
        for profile in VideoProfilePreference.allCases {
            let item = NSMenuItem(title: profile.title, action: #selector(setVideoProfileAction(_:)), keyEquivalent: "")
            item.representedObject = profile.rawValue
            item.state = state.settings.videoProfile == profile ? .on : .off
            menu.addItem(item)
        }
        let adaptive = NSMenuItem(title: "自适应码率", action: #selector(toggleAdaptiveBitrateAction), keyEquivalent: "")
        adaptive.state = state.settings.adaptiveBitrate ? .on : .off
        menu.addItem(adaptive)
        if includeProcess {
            let sub = NSMenuItem(title: "仅当前进程使用…", action: nil, keyEquivalent: "")
            let subMenu = NSMenu()
            for profile in VideoProfilePreference.allCases {
                let item = NSMenuItem(
                    title: profile.title,
                    action: #selector(setProcessVideoProfileAction(_:)),
                    keyEquivalent: ""
                )
                item.representedObject = profile.rawValue
                subMenu.addItem(item)
            }
            sub.submenu = subMenu
            menu.addItem(sub)
        }
    }
}
