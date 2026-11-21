import AppKit
import Carbon.HIToolbox
import Foundation

@MainActor
private final class ShortcutCaptureButton: NSButton {
    var shortcut: KeyShortcut {
        didSet { updateLabel() }
    }
    var onShortcutRecorded: ((KeyShortcut) -> Void)?
    private var isRecording = false {
        didSet { updateLabel() }
    }
    private var monitor: Any?

    init(shortcut: KeyShortcut) {
        self.shortcut = shortcut
        super.init(frame: .zero)
        bezelStyle = .rounded
        font = .monospacedSystemFont(ofSize: 12, weight: .medium)
        target = self
        action = #selector(toggleRecording)
        updateLabel()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    deinit {
        if let monitor {
            NSEvent.removeMonitor(monitor)
        }
    }

    func stopRecording() {
        isRecording = false
        if let monitor {
            NSEvent.removeMonitor(monitor)
            self.monitor = nil
        }
    }

    private func updateLabel() {
        if isRecording {
            title = "按下快捷键… (Esc 取消)"
            contentTintColor = .controlAccentColor
        } else {
            title = shortcut.displayString
            contentTintColor = nil
        }
    }

    @objc private func toggleRecording() {
        if isRecording {
            stopRecording()
            return
        }
        isRecording = true
        monitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
            guard let self, self.isRecording else { return event }
            if event.keyCode == UInt16(kVK_Escape) {
                self.stopRecording()
                return nil
            }
            let mods = event.modifierFlags.intersection(KeyShortcut.allowedModifiers)
            guard !mods.isEmpty else {
                NSSound.beep()
                return nil
            }
            let recorded = KeyShortcut(keyCode: event.keyCode, modifiers: mods)
            self.shortcut = recorded
            self.stopRecording()
            self.onShortcutRecorded?(recorded)
            return nil
        }
    }
}

@MainActor
final class ShortcutsWindowController: NSWindowController, NSWindowDelegate {
    private let state: AppState
    private var buttons: [ShortcutCaptureButton] = []

    init(state: AppState) {
        self.state = state
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 470, height: 320),
            styleMask: [.titled, .closable],
            backing: .buffered,
            defer: false
        )
        window.title = "WinBridge — 快捷键设置"
        window.isReleasedWhenClosed = false
        window.center()
        super.init(window: window)
        window.delegate = self
        buildUI(in: window)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    func show() {
        syncFromState()
        showWindow(nil)
        window?.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    func windowWillClose(_ notification: Notification) {
        for btn in buttons {
            btn.stopRecording()
        }
    }

    private func buildUI(in window: NSWindow) {
        let root = NSView()
        root.translatesAutoresizingMaskIntoConstraints = false
        window.contentView = root

        let intro = NSTextField(wrappingLabelWithString: "点击右侧按钮后直接按下新的组合键（需包含 ⌘ / ⌥ / ⌃ / ⇧ 中至少一个修饰键），修改立即生效。")
        intro.font = .systemFont(ofSize: 12)
        intro.textColor = .secondaryLabelColor

        let rowsSpec: [(String, KeyPath<ShortcutBindings, KeyShortcut>, WritableKeyPath<ShortcutBindings, KeyShortcut>)] = [
            ("全局快速启动 / 切换窗口（跨应用生效）", \.quickLauncher, \.quickLauncher),
            ("远端窗口置顶 / 取消置顶", \.pinOnTop, \.pinOnTop),
            ("静音 / 取消静音当前远端窗口", \.muteWindow, \.muteWindow),
            ("显示 / 隐藏性能 HUD", \.toggleHud, \.toggleHud),
            ("锁定 / 解锁 3D 游戏鼠标", \.toggleGameLock, \.toggleGameLock),
            ("取回 Windows 剪贴板文件", \.pullClipboardFiles, \.pullClipboardFiles),
        ]

        var rowViews: [NSView] = [intro]
        buttons.removeAll()
        for (title, getter, setter) in rowsSpec {
            let label = NSTextField(labelWithString: title)
            label.font = .systemFont(ofSize: 13)
            label.setContentHuggingPriority(.defaultLow, for: .horizontal)

            let btn = ShortcutCaptureButton(shortcut: state.settings.shortcuts[keyPath: getter])
            btn.widthAnchor.constraint(equalToConstant: 170).isActive = true
            btn.onShortcutRecorded = { [weak self] newShortcut in
                guard let self else { return }
                var s = self.state.settings
                s.shortcuts[keyPath: setter] = newShortcut
                self.state.settings = s
                self.state.setBanner("已更新快捷键：\(title) → \(newShortcut.displayString)", isError: false)
                self.state.onStateChanged?()
            }
            buttons.append(btn)

            let row = NSStackView(views: [label, btn])
            row.orientation = .horizontal
            row.alignment = .centerY
            row.spacing = 12
            rowViews.append(row)
        }

        let resetBtn = NSButton(title: "恢复全部默认快捷键", target: self, action: #selector(resetDefaults))
        resetBtn.bezelStyle = .rounded

        let closeBtn = NSButton(title: "完成", target: self, action: #selector(closeClicked))
        closeBtn.bezelStyle = .rounded
        closeBtn.keyEquivalent = "\r"

        let footerSpacer = NSView()
        footerSpacer.setContentHuggingPriority(.defaultLow, for: .horizontal)
        let footer = NSStackView(views: [resetBtn, footerSpacer, closeBtn])
        footer.orientation = .horizontal
        footer.alignment = .centerY
        rowViews.append(footer)

        let stack = NSStackView(views: rowViews)
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 12
        stack.translatesAutoresizingMaskIntoConstraints = false
        root.addSubview(stack)

        NSLayoutConstraint.activate([
            stack.topAnchor.constraint(equalTo: root.topAnchor, constant: 16),
            stack.leadingAnchor.constraint(equalTo: root.leadingAnchor, constant: 18),
            stack.trailingAnchor.constraint(equalTo: root.trailingAnchor, constant: -18),
            stack.bottomAnchor.constraint(lessThanOrEqualTo: root.bottomAnchor, constant: -16),
        ])
        for v in rowViews {
            v.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        }
    }

    private func syncFromState() {
        let sc = state.settings.shortcuts
        let values = [
            sc.quickLauncher,
            sc.pinOnTop,
            sc.muteWindow,
            sc.toggleHud,
            sc.toggleGameLock,
            sc.pullClipboardFiles,
        ]
        for (btn, val) in zip(buttons, values) {
            btn.stopRecording()
            btn.shortcut = val
        }
    }

    @objc private func resetDefaults() {
        var s = state.settings
        s.shortcuts = .defaults
        state.settings = s
        syncFromState()
        state.setBanner("已恢复全部默认快捷键", isError: false)
        state.onStateChanged?()
    }

    @objc private func closeClicked() {
        window?.close()
    }
}
