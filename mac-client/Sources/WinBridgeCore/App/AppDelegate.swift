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
    private var statusItem: NSStatusItem?

    func applicationDidFinishLaunching(_ notification: Notification) {
        setupMainMenu()
        setupStatusItem()

        let manager = ManagerWindowController(state: state)
        self.managerController = manager

        state.onStateChanged = { [weak self] in
            self?.managerController?.refreshUI()
            self?.rebuildStatusMenu()
        }
        state.onOpenManager = { [weak self] in
            self?.showManager()
        }

        showManager()
        NSApp.activate(ignoringOtherApps: true)
    }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        if !flag {
            showManager()
        }
        return true
    }

    func applicationWillTerminate(_ notification: Notification) {
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

    @objc private func detachActiveWindowAction() {
        state.detachActiveWindow()
    }

    @objc private func disconnectAction() {
        state.disconnect()
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
        winMenu.addItem(withTitle: "刷新 Windows 窗口列表", action: #selector(refreshWindowsAction), keyEquivalent: "")
        winMenu.addItem(withTitle: "刷新当前窗口关键帧 (IDR)", action: #selector(requestKeyframeAction), keyEquivalent: "")
        winMenu.addItem(withTitle: "脱离当前远端窗口", action: #selector(detachActiveWindowAction), keyEquivalent: "")
        winMenu.addItem(.separator())
        winMenu.addItem(withTitle: "断开连接", action: #selector(disconnectAction), keyEquivalent: "")
        winItem.submenu = winMenu
        mainMenu.addItem(winItem)

        NSApp.mainMenu = mainMenu
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
            menu.addItem(withTitle: "刷新 Windows 窗口列表", action: #selector(refreshWindowsAction), keyEquivalent: "")
            menu.addItem(withTitle: "断开连接", action: #selector(disconnectAction), keyEquivalent: "")
        }
        menu.addItem(.separator())
        menu.addItem(withTitle: "退出 WinBridge", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "")
        statusItem?.menu = menu
    }
}
