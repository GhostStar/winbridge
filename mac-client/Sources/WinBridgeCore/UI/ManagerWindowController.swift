import AppKit
import Foundation

@MainActor
final class ManagerWindowController: NSWindowController, NSTableViewDataSource, NSTableViewDelegate {
    private let state: AppState

    private let hostField = NSTextField()
    private let portField = NSTextField()
    private let tokenField = NSSecureTextField()
    private let codecPopup = NSPopUpButton()
    private let connectButton = NSButton(title: "连接", target: nil, action: nil)
    private let refreshButton = NSButton(title: "刷新窗口", target: nil, action: nil)

    private let statusLabel = NSTextField(labelWithString: "未连接")
    private let rttLabel = NSTextField(labelWithString: "")
    private let bannerLabel = NSTextField(labelWithString: "")

    private let cmdCtrlCheck = NSButton(checkboxWithTitle: "Cmd → Ctrl 快捷映射", target: nil, action: nil)
    private let clipboardCheck = NSButton(checkboxWithTitle: "剪贴板双向同步", target: nil, action: nil)
    private let popupsCheck = NSButton(checkboxWithTitle: "追踪菜单 / 弹窗", target: nil, action: nil)
    private let syncResizeCheck = NSButton(checkboxWithTitle: "同步缩放 Windows", target: nil, action: nil)

    private let audioCheck = NSButton(checkboxWithTitle: "开启音频", target: nil, action: nil)
    private let volumeSlider = NSSlider(value: 0.85, minValue: 0.0, maxValue: 1.0, target: nil, action: nil)
    private let volumeLabel = NSTextField(labelWithString: "85%")

    private let tableView = NSTableView()
    private let scrollView = NSScrollView()

    init(state: AppState) {
        self.state = state
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 760, height: 500),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.title = "WinBridge — Windows 窗口投射"
        window.minSize = NSSize(width: 680, height: 400)
        window.isReleasedWhenClosed = false
        window.center()
        super.init(window: window)

        buildUI(in: window)
        loadFromSettings()
        refreshUI()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    private func buildUI(in window: NSWindow) {
        let root = NSView()
        root.translatesAutoresizingMaskIntoConstraints = false
        window.contentView = root

        // --- Row 1: Connection inputs ---
        hostField.placeholderString = "Windows IP (如 192.168.1.100)"
        hostField.setContentHuggingPriority(.defaultLow, for: .horizontal)

        portField.placeholderString = "9876"
        portField.widthAnchor.constraint(equalToConstant: 66).isActive = true

        tokenField.placeholderString = "Token (开发模式可留空)"
        tokenField.widthAnchor.constraint(equalToConstant: 140).isActive = true

        for pref in CodecPreference.allCases {
            codecPopup.addItem(withTitle: pref.title)
        }
        codecPopup.widthAnchor.constraint(equalToConstant: 145).isActive = true
        codecPopup.target = self
        codecPopup.action = #selector(settingsChanged)

        connectButton.bezelStyle = .rounded
        connectButton.keyEquivalent = "\r"
        connectButton.target = self
        connectButton.action = #selector(toggleConnect)
        connectButton.widthAnchor.constraint(equalToConstant: 76).isActive = true

        let connRow = NSStackView(views: [
            NSTextField(labelWithString: "主机:"),
            hostField,
            NSTextField(labelWithString: "端口:"),
            portField,
            tokenField,
            codecPopup,
            connectButton,
        ])
        connRow.orientation = .horizontal
        connRow.alignment = .centerY
        connRow.spacing = 8

        // --- Row 2: Toggles & Refresh ---
        cmdCtrlCheck.target = self
        cmdCtrlCheck.action = #selector(settingsChanged)
        clipboardCheck.target = self
        clipboardCheck.action = #selector(settingsChanged)
        popupsCheck.target = self
        popupsCheck.action = #selector(settingsChanged)
        syncResizeCheck.target = self
        syncResizeCheck.action = #selector(settingsChanged)
        syncResizeCheck.toolTip = "关闭时仅在 Mac 本地等比缩放画面，不改变 Windows 原窗口大小"

        refreshButton.bezelStyle = .rounded
        refreshButton.target = self
        refreshButton.action = #selector(refreshClicked)

        let spacer = NSView()
        spacer.setContentHuggingPriority(.defaultLow, for: .horizontal)

        let optionsRow = NSStackView(views: [cmdCtrlCheck, clipboardCheck, popupsCheck, syncResizeCheck, spacer, refreshButton])
        optionsRow.orientation = .horizontal
        optionsRow.alignment = .centerY
        optionsRow.spacing = 10

        // --- Row 2b: Audio Switch & Volume ---
        audioCheck.target = self
        audioCheck.action = #selector(settingsChanged)
        audioCheck.toolTip = "开启后实时转发 Windows 系统音频到 Mac，可随时动态开关"

        volumeSlider.isContinuous = true
        volumeSlider.controlSize = .small
        volumeSlider.widthAnchor.constraint(equalToConstant: 120).isActive = true
        volumeSlider.target = self
        volumeSlider.action = #selector(volumeChanged)

        volumeLabel.font = .monospacedDigitSystemFont(ofSize: 11.5, weight: .regular)
        volumeLabel.textColor = .secondaryLabelColor
        volumeLabel.widthAnchor.constraint(equalToConstant: 36).isActive = true

        let audioHint = NSTextField(labelWithString: "（需保持 Windows 声音未静音；随时可开关）")
        audioHint.font = .systemFont(ofSize: 11)
        audioHint.textColor = .tertiaryLabelColor

        let audioSpacer = NSView()
        audioSpacer.setContentHuggingPriority(.defaultLow, for: .horizontal)

        let audioRow = NSStackView(views: [audioCheck, NSTextField(labelWithString: "音量:"), volumeSlider, volumeLabel, audioHint, audioSpacer])
        audioRow.orientation = .horizontal
        audioRow.alignment = .centerY
        audioRow.spacing = 8

        // --- Row 3: Status strip ---
        statusLabel.font = .systemFont(ofSize: 12, weight: .medium)
        rttLabel.font = .monospacedDigitSystemFont(ofSize: 11, weight: .regular)
        rttLabel.textColor = .secondaryLabelColor
        let statusSpacer = NSView()
        statusSpacer.setContentHuggingPriority(.defaultLow, for: .horizontal)
        let tipLabel = NSTextField(labelWithString: "提示：Option+点红叉仅脱离窗口；直接点红叉向 Windows 发关闭指令")
        tipLabel.font = .systemFont(ofSize: 11)
        tipLabel.textColor = .tertiaryLabelColor

        let statusRow = NSStackView(views: [statusLabel, rttLabel, statusSpacer, tipLabel])
        statusRow.orientation = .horizontal
        statusRow.alignment = .centerY
        statusRow.spacing = 8

        // --- Table of Windows ---
        let colWin = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("window"))
        colWin.title = "Windows 窗口（双击打开 / 切换）"
        colWin.minWidth = 260
        colWin.resizingMask = .autoresizingMask

        let colMeta = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("meta"))
        colMeta.title = "分辨率 / DPI"
        colMeta.width = 130

        let colMode = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("mode"))
        colMode.title = "采集模式"
        colMode.width = 104

        let colAction = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("action"))
        colAction.title = "操作"
        colAction.width = 86

        tableView.addTableColumn(colWin)
        tableView.addTableColumn(colMeta)
        tableView.addTableColumn(colMode)
        tableView.addTableColumn(colAction)
        tableView.rowHeight = 36
        tableView.usesAlternatingRowBackgroundColors = true
        tableView.columnAutoresizingStyle = .firstColumnOnlyAutoresizingStyle
        tableView.dataSource = self
        tableView.delegate = self
        tableView.target = self
        tableView.doubleAction = #selector(tableDoubleClicked)

        scrollView.documentView = tableView
        scrollView.hasVerticalScroller = true
        scrollView.borderType = .bezelBorder

        // --- Bottom Banner ---
        bannerLabel.font = .systemFont(ofSize: 11.5)
        bannerLabel.lineBreakMode = .byTruncatingTail
        bannerLabel.maximumNumberOfLines = 1

        let mainStack = NSStackView(views: [connRow, optionsRow, audioRow, statusRow, scrollView, bannerLabel])
        mainStack.orientation = .vertical
        mainStack.alignment = .leading
        mainStack.spacing = 10
        mainStack.translatesAutoresizingMaskIntoConstraints = false
        root.addSubview(mainStack)

        NSLayoutConstraint.activate([
            mainStack.topAnchor.constraint(equalTo: root.topAnchor, constant: 14),
            mainStack.leadingAnchor.constraint(equalTo: root.leadingAnchor, constant: 14),
            mainStack.trailingAnchor.constraint(equalTo: root.trailingAnchor, constant: -14),
            mainStack.bottomAnchor.constraint(equalTo: root.bottomAnchor, constant: -12),
            connRow.widthAnchor.constraint(equalTo: mainStack.widthAnchor),
            optionsRow.widthAnchor.constraint(equalTo: mainStack.widthAnchor),
            audioRow.widthAnchor.constraint(equalTo: mainStack.widthAnchor),
            statusRow.widthAnchor.constraint(equalTo: mainStack.widthAnchor),
            scrollView.widthAnchor.constraint(equalTo: mainStack.widthAnchor),
            scrollView.heightAnchor.constraint(greaterThanOrEqualToConstant: 220),
            bannerLabel.widthAnchor.constraint(equalTo: mainStack.widthAnchor),
        ])
    }

    private func loadFromSettings() {
        hostField.stringValue = state.settings.host
        portField.stringValue = String(state.settings.port)
        tokenField.stringValue = state.settings.authToken
        if let idx = CodecPreference.allCases.firstIndex(of: state.settings.codecPreference) {
            codecPopup.selectItem(at: idx)
        }
        cmdCtrlCheck.state = state.settings.cmdMapsToCtrl ? .on : .off
        clipboardCheck.state = state.settings.syncClipboard ? .on : .off
        popupsCheck.state = state.settings.followPopups ? .on : .off
        syncResizeCheck.state = state.settings.syncWindowResize ? .on : .off
        audioCheck.state = state.settings.audioEnabled ? .on : .off
        volumeSlider.floatValue = state.settings.audioVolume
        volumeSlider.isEnabled = state.settings.audioEnabled
        volumeLabel.stringValue = "\(Int(round(state.settings.audioVolume * 100)))%"
    }

    private func persistInputsToSettings() {
        var s = state.settings
        s.host = hostField.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        if let p = UInt16(portField.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)), p > 0 {
            s.port = p
        }
        s.authToken = tokenField.stringValue
        let codecIdx = codecPopup.indexOfSelectedItem
        if codecIdx >= 0 && codecIdx < CodecPreference.allCases.count {
            s.codecPreference = CodecPreference.allCases[codecIdx]
        }
        s.cmdMapsToCtrl = (cmdCtrlCheck.state == .on)
        s.syncClipboard = (clipboardCheck.state == .on)
        s.followPopups = (popupsCheck.state == .on)
        s.syncWindowResize = (syncResizeCheck.state == .on)
        s.audioEnabled = (audioCheck.state == .on)
        s.audioVolume = volumeSlider.floatValue
        volumeSlider.isEnabled = s.audioEnabled
        volumeLabel.stringValue = "\(Int(round(s.audioVolume * 100)))%"
        state.settings = s
    }

    func refreshUI() {
        switch state.connectionState {
        case .disconnected:
            connectButton.title = "连接"
            connectButton.isEnabled = true
            hostField.isEnabled = true
            portField.isEnabled = true
            tokenField.isEnabled = true
            codecPopup.isEnabled = true
            refreshButton.isEnabled = false
            statusLabel.stringValue = "● 未连接"
            statusLabel.textColor = .secondaryLabelColor
            rttLabel.stringValue = ""

        case .connecting:
            connectButton.title = "连接中"
            connectButton.isEnabled = false
            hostField.isEnabled = false
            portField.isEnabled = false
            tokenField.isEnabled = false
            codecPopup.isEnabled = false
            refreshButton.isEnabled = false
            statusLabel.stringValue = "● 正在连接…"
            statusLabel.textColor = .systemOrange
            rttLabel.stringValue = ""

        case .connected(let hostName, let codec):
            connectButton.title = "断开"
            connectButton.isEnabled = true
            hostField.isEnabled = false
            portField.isEnabled = false
            tokenField.isEnabled = false
            codecPopup.isEnabled = false
            refreshButton.isEnabled = true
            statusLabel.stringValue = "● 已连接：\(hostName)（\(codec.displayName)）"
            statusLabel.textColor = .systemGreen
            if let rtt = state.rttMs {
                rttLabel.stringValue = String(format: "RTT %.1f ms", rtt)
            } else {
                rttLabel.stringValue = ""
            }
        }

        audioCheck.state = state.settings.audioEnabled ? .on : .off
        volumeSlider.floatValue = state.settings.audioVolume
        volumeSlider.isEnabled = state.settings.audioEnabled
        volumeLabel.stringValue = "\(Int(round(state.settings.audioVolume * 100)))%"

        bannerLabel.stringValue = state.lastBannerMessage
        bannerLabel.textColor = state.lastBannerIsError ? .systemRed : .secondaryLabelColor
        tableView.reloadData()
    }

    // MARK: - Actions

    @objc private func volumeChanged() {
        persistInputsToSettings()
    }

    @objc private func toggleConnect() {
        persistInputsToSettings()
        switch state.connectionState {
        case .disconnected:
            state.connect()
        case .connected, .connecting:
            state.disconnect()
        }
    }

    @objc private func settingsChanged() {
        persistInputsToSettings()
    }

    @objc private func refreshClicked() {
        state.refreshWindowList()
    }

    @objc private func tableDoubleClicked() {
        let row = tableView.clickedRow
        guard row >= 0 && row < state.availableWindows.count else { return }
        state.focusOrSubscribe(state.availableWindows[row])
    }

    @objc private func rowModeChanged(_ sender: NSPopUpButton) {
        let row = sender.tag
        guard row >= 0 && row < state.availableWindows.count else { return }
        let win = state.availableWindows[row]
        let mode: CaptureMode = (sender.indexOfSelectedItem == 1) ? .monitorCrop : .window
        state.setCaptureMode(mode, for: win)
    }

    @objc private func rowActionClicked(_ sender: NSButton) {
        let row = sender.tag
        guard row >= 0 && row < state.availableWindows.count else { return }
        let win = state.availableWindows[row]
        if state.subscribedRootIds.contains(win.windowId) {
            state.unsubscribeWindow(win.windowId)
        } else {
            state.subscribeWindow(win)
        }
    }

    // MARK: - NSTableViewDataSource / Delegate

    func numberOfRows(in tableView: NSTableView) -> Int {
        state.availableWindows.count
    }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        guard row >= 0 && row < state.availableWindows.count, let colId = tableColumn?.identifier.rawValue else {
            return nil
        }
        let info = state.availableWindows[row]
        let isOpen = state.subscribedRootIds.contains(info.windowId)

        switch colId {
        case "window":
            let cell = NSView()
            let iconView = NSImageView()
            iconView.translatesAutoresizingMaskIntoConstraints = false
            iconView.imageScaling = .scaleProportionallyDown
            if !info.iconPng.isEmpty, let img = NSImage(data: info.iconPng) {
                iconView.image = img
            } else {
                iconView.image = NSImage(systemSymbolName: "macwindow", accessibilityDescription: nil)
            }

            let titleText = info.title.isEmpty ? "(无标题)" : info.title
            let titleLabel = NSTextField(labelWithString: titleText)
            titleLabel.font = .systemFont(ofSize: 12.5, weight: isOpen ? .semibold : .regular)
            titleLabel.lineBreakMode = .byTruncatingTail

            let subText = info.isMinimized ? "\(info.processName)（已最小化）" : info.processName
            let subLabel = NSTextField(labelWithString: subText)
            subLabel.font = .systemFont(ofSize: 10.5)
            subLabel.textColor = info.isMinimized ? .systemOrange : .secondaryLabelColor
            subLabel.lineBreakMode = .byTruncatingTail

            let textStack = NSStackView(views: [titleLabel, subLabel])
            textStack.orientation = .vertical
            textStack.alignment = .leading
            textStack.spacing = 1
            textStack.translatesAutoresizingMaskIntoConstraints = false

            cell.addSubview(iconView)
            cell.addSubview(textStack)
            NSLayoutConstraint.activate([
                iconView.leadingAnchor.constraint(equalTo: cell.leadingAnchor, constant: 4),
                iconView.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
                iconView.widthAnchor.constraint(equalToConstant: 22),
                iconView.heightAnchor.constraint(equalToConstant: 22),
                textStack.leadingAnchor.constraint(equalTo: iconView.trailingAnchor, constant: 8),
                textStack.trailingAnchor.constraint(equalTo: cell.trailingAnchor, constant: -4),
                textStack.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
            ])
            return cell

        case "meta":
            let pct = Int(round(Double(info.dpi > 0 ? info.dpi : 96) / 96.0 * 100.0))
            let label = NSTextField(labelWithString: "\(info.width)×\(info.height) · \(pct)%")
            label.font = .monospacedDigitSystemFont(ofSize: 11.5, weight: .regular)
            label.textColor = .secondaryLabelColor
            label.translatesAutoresizingMaskIntoConstraints = false
            let cell = NSView()
            cell.addSubview(label)
            NSLayoutConstraint.activate([
                label.leadingAnchor.constraint(equalTo: cell.leadingAnchor, constant: 4),
                label.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
            ])
            return cell

        case "mode":
            let popup = NSPopUpButton()
            popup.controlSize = .small
            popup.font = .systemFont(ofSize: 11.5)
            popup.addItems(withTitles: ["单窗", "屏幕裁剪"])
            popup.selectItem(at: state.captureMode(for: info) == .monitorCrop ? 1 : 0)
            popup.tag = row
            popup.target = self
            popup.action = #selector(rowModeChanged(_:))
            popup.toolTip = "单窗支持遮挡多开；屏幕裁剪兼容 MuMu 模拟器/Vulkan/游戏"
            popup.translatesAutoresizingMaskIntoConstraints = false
            let cell = NSView()
            cell.addSubview(popup)
            NSLayoutConstraint.activate([
                popup.centerXAnchor.constraint(equalTo: cell.centerXAnchor),
                popup.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
                popup.widthAnchor.constraint(equalToConstant: 92),
            ])
            return cell

        case "action":
            let btn = NSButton(title: isOpen ? "脱离" : "投射", target: self, action: #selector(rowActionClicked(_:)))
            btn.bezelStyle = .rounded
            btn.controlSize = .small
            btn.tag = row
            btn.translatesAutoresizingMaskIntoConstraints = false
            let cell = NSView()
            cell.addSubview(btn)
            NSLayoutConstraint.activate([
                btn.centerXAnchor.constraint(equalTo: cell.centerXAnchor),
                btn.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
                btn.widthAnchor.constraint(equalToConstant: 64),
            ])
            return cell

        default:
            return nil
        }
    }
}
