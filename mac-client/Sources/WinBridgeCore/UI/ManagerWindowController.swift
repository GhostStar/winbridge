import AppKit
import Foundation

@MainActor
final class ManagerWindowController: NSWindowController, NSTableViewDataSource, NSTableViewDelegate, NSSearchFieldDelegate {
    private let state: AppState

    private let hostField = NSTextField()
    private let portField = NSTextField()
    private let tokenField = NSSecureTextField()
    private let codecPopup = NSPopUpButton()
    private let connectButton = NSButton(title: "连接", target: nil, action: nil)

    private let cmdCtrlCheck = NSButton(checkboxWithTitle: "Cmd → Ctrl", target: nil, action: nil)
    private let clipboardCheck = NSButton(checkboxWithTitle: "剪贴板同步", target: nil, action: nil)
    private let popupsCheck = NSButton(checkboxWithTitle: "追踪弹窗", target: nil, action: nil)
    private let syncResizeCheck = NSButton(checkboxWithTitle: "同步缩放 Windows", target: nil, action: nil)
    private let desktopButton = NSButton(title: "查看完整桌面", target: nil, action: nil)

    private let audioCheck = NSButton(checkboxWithTitle: "开启音频", target: nil, action: nil)
    private let volumeSlider = NSSlider(value: 0.85, minValue: 0.0, maxValue: 1.0, target: nil, action: nil)
    private let volumeLabel = NSTextField(labelWithString: "85%")
    private let sensSlider = NSSlider(value: 1.0, minValue: 0.2, maxValue: 3.0, target: nil, action: nil)
    private let sensLabel = NSTextField(labelWithString: "1.0x")

    private let statusLabel = NSTextField(labelWithString: "未连接")
    private let rttLabel = NSTextField(labelWithString: "")
    private let bannerLabel = NSTextField(labelWithString: "")

    private let tabControl = NSSegmentedControl(labels: ["已打开窗口", "启动应用"], trackingMode: .selectOne, target: nil, action: nil)
    private let searchField = NSSearchField()
    private let refreshButton = NSButton(title: "刷新", target: nil, action: nil)

    private let windowsTable = NSTableView()
    private let windowsScroll = NSScrollView()
    private let appsTable = NSTableView()
    private let appsScroll = NSScrollView()
    private var filteredApps: [AppEntry] = []

    init(state: AppState) {
        self.state = state
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 780, height: 540),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.title = "WinBridge — Windows 窗口投射"
        window.minSize = NSSize(width: 700, height: 440)
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
        portField.widthAnchor.constraint(equalToConstant: 64).isActive = true

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
        connectButton.widthAnchor.constraint(equalToConstant: 74).isActive = true

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

        // --- Row 2: Feature Toggles + Full Desktop Fallback ---
        cmdCtrlCheck.target = self
        cmdCtrlCheck.action = #selector(settingsChanged)
        clipboardCheck.target = self
        clipboardCheck.action = #selector(settingsChanged)
        popupsCheck.target = self
        popupsCheck.action = #selector(settingsChanged)
        syncResizeCheck.target = self
        syncResizeCheck.action = #selector(settingsChanged)
        syncResizeCheck.toolTip = "关闭时仅在 Mac 本地等比缩放画面，不改变 Windows 原窗口大小"

        desktopButton.bezelStyle = .rounded
        desktopButton.image = NSImage(systemSymbolName: "desktopcomputer", accessibilityDescription: nil)
        desktopButton.imagePosition = .imageLeading
        desktopButton.target = self
        desktopButton.action = #selector(desktopClicked)
        desktopButton.toolTip = "临时打开完整的 Windows 主显示器桌面（含任务栏与系统托盘）"

        let optionsSpacer = NSView()
        optionsSpacer.setContentHuggingPriority(.defaultLow, for: .horizontal)

        let optionsRow = NSStackView(views: [cmdCtrlCheck, clipboardCheck, popupsCheck, syncResizeCheck, optionsSpacer, desktopButton])
        optionsRow.orientation = .horizontal
        optionsRow.alignment = .centerY
        optionsRow.spacing = 10

        // --- Row 3: Audio Switch & Volume ---
        audioCheck.target = self
        audioCheck.action = #selector(settingsChanged)
        audioCheck.toolTip = "开启后实时转发 Windows 系统音频到 Mac，可随时动态开关"

        volumeSlider.isContinuous = true
        volumeSlider.controlSize = .small
        volumeSlider.widthAnchor.constraint(equalToConstant: 92).isActive = true
        volumeSlider.target = self
        volumeSlider.action = #selector(volumeChanged)

        volumeLabel.font = .monospacedDigitSystemFont(ofSize: 11.5, weight: .regular)
        volumeLabel.textColor = .secondaryLabelColor
        volumeLabel.widthAnchor.constraint(equalToConstant: 36).isActive = true

        sensSlider.isContinuous = true
        sensSlider.controlSize = .small
        sensSlider.widthAnchor.constraint(equalToConstant: 92).isActive = true
        sensSlider.target = self
        sensSlider.action = #selector(volumeChanged)
        sensSlider.toolTip = "3D 游戏鼠标锁定模式（⇧⌘L 开启 / Ctrl+Option 释放）下的相对视角灵敏度倍率"

        sensLabel.font = .monospacedDigitSystemFont(ofSize: 11.5, weight: .regular)
        sensLabel.textColor = .secondaryLabelColor
        sensLabel.widthAnchor.constraint(equalToConstant: 34).isActive = true

        let audioSpacer = NSView()
        audioSpacer.setContentHuggingPriority(.defaultLow, for: .horizontal)

        statusLabel.font = .systemFont(ofSize: 12, weight: .medium)
        rttLabel.font = .monospacedDigitSystemFont(ofSize: 11, weight: .regular)
        rttLabel.textColor = .secondaryLabelColor

        let audioStatusRow = NSStackView(views: [
            audioCheck,
            NSTextField(labelWithString: "音量:"),
            volumeSlider,
            volumeLabel,
            NSTextField(labelWithString: "锁定灵敏度:"),
            sensSlider,
            sensLabel,
            audioSpacer,
            statusLabel,
            rttLabel,
        ])
        audioStatusRow.orientation = .horizontal
        audioStatusRow.alignment = .centerY
        audioStatusRow.spacing = 8

        // --- Row 4: Tab Switcher + Search + Refresh ---
        tabControl.selectedSegment = 0
        tabControl.target = self
        tabControl.action = #selector(tabChanged)

        searchField.placeholderString = "搜索桌面 / 开始菜单应用…"
        searchField.controlSize = .regular
        searchField.widthAnchor.constraint(equalToConstant: 230).isActive = true
        searchField.delegate = self
        searchField.isHidden = true

        refreshButton.bezelStyle = .rounded
        refreshButton.target = self
        refreshButton.action = #selector(refreshClicked)

        let tabSpacer = NSView()
        tabSpacer.setContentHuggingPriority(.defaultLow, for: .horizontal)

        let tabRow = NSStackView(views: [tabControl, searchField, tabSpacer, refreshButton])
        tabRow.orientation = .horizontal
        tabRow.alignment = .centerY
        tabRow.spacing = 10

        // --- Table 1: Open Windows ---
        setupWindowsTable()

        // --- Table 2: Launchable Apps ---
        setupAppsTable()

        // Container holding both scroll views
        let listContainer = NSView()
        listContainer.translatesAutoresizingMaskIntoConstraints = false
        windowsScroll.translatesAutoresizingMaskIntoConstraints = false
        appsScroll.translatesAutoresizingMaskIntoConstraints = false
        appsScroll.isHidden = true
        listContainer.addSubview(windowsScroll)
        listContainer.addSubview(appsScroll)
        NSLayoutConstraint.activate([
            windowsScroll.topAnchor.constraint(equalTo: listContainer.topAnchor),
            windowsScroll.leadingAnchor.constraint(equalTo: listContainer.leadingAnchor),
            windowsScroll.trailingAnchor.constraint(equalTo: listContainer.trailingAnchor),
            windowsScroll.bottomAnchor.constraint(equalTo: listContainer.bottomAnchor),
            appsScroll.topAnchor.constraint(equalTo: listContainer.topAnchor),
            appsScroll.leadingAnchor.constraint(equalTo: listContainer.leadingAnchor),
            appsScroll.trailingAnchor.constraint(equalTo: listContainer.trailingAnchor),
            appsScroll.bottomAnchor.constraint(equalTo: listContainer.bottomAnchor),
            listContainer.heightAnchor.constraint(greaterThanOrEqualToConstant: 240),
        ])

        // --- Bottom Banner ---
        bannerLabel.font = .systemFont(ofSize: 11.5)
        bannerLabel.lineBreakMode = .byTruncatingTail
        bannerLabel.maximumNumberOfLines = 1

        let mainStack = NSStackView(views: [connRow, optionsRow, audioStatusRow, tabRow, listContainer, bannerLabel])
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
            audioStatusRow.widthAnchor.constraint(equalTo: mainStack.widthAnchor),
            tabRow.widthAnchor.constraint(equalTo: mainStack.widthAnchor),
            listContainer.widthAnchor.constraint(equalTo: mainStack.widthAnchor),
            bannerLabel.widthAnchor.constraint(equalTo: mainStack.widthAnchor),
        ])
    }

    private func setupWindowsTable() {
        let colWin = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("window"))
        colWin.title = "Windows 窗口（双击投射 / 切到前台）"
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

        windowsTable.addTableColumn(colWin)
        windowsTable.addTableColumn(colMeta)
        windowsTable.addTableColumn(colMode)
        windowsTable.addTableColumn(colAction)
        windowsTable.rowHeight = 36
        windowsTable.usesAlternatingRowBackgroundColors = true
        windowsTable.columnAutoresizingStyle = .firstColumnOnlyAutoresizingStyle
        windowsTable.dataSource = self
        windowsTable.delegate = self
        windowsTable.target = self
        windowsTable.doubleAction = #selector(windowsTableDoubleClicked)

        windowsScroll.documentView = windowsTable
        windowsScroll.hasVerticalScroller = true
        windowsScroll.borderType = .bezelBorder
    }

    private func setupAppsTable() {
        let colPin = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("app_pin"))
        colPin.title = "置顶"
        colPin.width = 44

        let colName = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("app_name"))
        colName.title = "Windows 应用（双击启动并自动接管窗口）"
        colName.minWidth = 260
        colName.resizingMask = .autoresizingMask

        let colSource = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("app_source"))
        colSource.title = "来源"
        colSource.width = 82

        let colMode = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("app_mode"))
        colMode.title = "采集模式"
        colMode.width = 104

        let colLaunch = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("app_launch"))
        colLaunch.title = "操作"
        colLaunch.width = 82

        appsTable.addTableColumn(colPin)
        appsTable.addTableColumn(colName)
        appsTable.addTableColumn(colSource)
        appsTable.addTableColumn(colMode)
        appsTable.addTableColumn(colLaunch)
        appsTable.rowHeight = 36
        appsTable.usesAlternatingRowBackgroundColors = true
        appsTable.columnAutoresizingStyle = .uniformColumnAutoresizingStyle
        appsTable.dataSource = self
        appsTable.delegate = self
        appsTable.target = self
        appsTable.doubleAction = #selector(appsTableDoubleClicked)

        appsScroll.documentView = appsTable
        appsScroll.hasVerticalScroller = true
        appsScroll.borderType = .bezelBorder
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
        sensSlider.floatValue = state.settings.mouseSensitivity
        sensLabel.stringValue = String(format: "%.1fx", state.settings.mouseSensitivity)
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
        s.mouseSensitivity = sensSlider.floatValue
        volumeSlider.isEnabled = s.audioEnabled
        volumeLabel.stringValue = "\(Int(round(s.audioVolume * 100)))%"
        sensLabel.stringValue = String(format: "%.1fx", s.mouseSensitivity)
        state.settings = s
    }

    private func rebuildFilteredApps() {
        let query = searchField.stringValue.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        let base = query.isEmpty
            ? state.availableApps
            : state.availableApps.filter {
                $0.name.lowercased().contains(query) || $0.source.lowercased().contains(query)
            }
        // Pinned apps always float to the top.
        filteredApps = base.sorted { a, b in
            let pa = state.isPinned(a.appId)
            let pb = state.isPinned(b.appId)
            if pa != pb { return pa }
            return false
        }
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
            desktopButton.isEnabled = false
            desktopButton.title = "查看完整桌面"
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
            desktopButton.isEnabled = false
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
            desktopButton.isEnabled = true
            desktopButton.title = state.isDesktopOpen ? "关闭桌面窗口" : "查看完整桌面"
            statusLabel.stringValue = "● \(hostName)（\(codec.displayName)）"
            statusLabel.textColor = .systemGreen
            if let rtt = state.rttMs {
                rttLabel.stringValue = String(format: "%.1f ms", rtt)
            } else {
                rttLabel.stringValue = ""
            }
        }

        audioCheck.state = state.settings.audioEnabled ? .on : .off
        volumeSlider.floatValue = state.settings.audioVolume
        volumeSlider.isEnabled = state.settings.audioEnabled
        volumeLabel.stringValue = "\(Int(round(state.settings.audioVolume * 100)))%"
        sensSlider.floatValue = state.settings.mouseSensitivity
        sensLabel.stringValue = String(format: "%.1fx", state.settings.mouseSensitivity)

        tabControl.setLabel("已打开窗口 (\(state.availableWindows.count))", forSegment: 0)
        tabControl.setLabel("启动应用 (\(state.availableApps.count))", forSegment: 1)

        bannerLabel.stringValue = state.lastBannerMessage
        bannerLabel.textColor = state.lastBannerIsError ? .systemRed : .secondaryLabelColor

        rebuildFilteredApps()
        windowsTable.reloadData()
        appsTable.reloadData()
    }

    // MARK: - Actions

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

    @objc private func volumeChanged() {
        persistInputsToSettings()
    }

    @objc private func desktopClicked() {
        state.toggleDesktopView()
    }

    @objc private func tabChanged() {
        let isAppsTab = (tabControl.selectedSegment == 1)
        windowsScroll.isHidden = isAppsTab
        appsScroll.isHidden = !isAppsTab
        searchField.isHidden = !isAppsTab
        if isAppsTab && state.availableApps.isEmpty {
            state.refreshAppList()
        }
    }

    @objc private func refreshClicked() {
        if tabControl.selectedSegment == 1 {
            state.refreshAppList()
        } else {
            state.refreshWindowList()
        }
    }

    func controlTextDidChange(_ obj: Notification) {
        rebuildFilteredApps()
        appsTable.reloadData()
    }

    // --- Windows table actions ---

    @objc private func windowsTableDoubleClicked() {
        let row = windowsTable.clickedRow
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

    // --- Apps table actions ---

    @objc private func appsTableDoubleClicked() {
        let row = appsTable.clickedRow
        guard row >= 0 && row < filteredApps.count else { return }
        state.launchApp(filteredApps[row])
    }

    @objc private func appPinClicked(_ sender: NSButton) {
        let row = sender.tag
        guard row >= 0 && row < filteredApps.count else { return }
        state.togglePin(filteredApps[row].appId)
    }

    @objc private func appModeChanged(_ sender: NSPopUpButton) {
        let row = sender.tag
        guard row >= 0 && row < filteredApps.count else { return }
        let mode: CaptureMode = (sender.indexOfSelectedItem == 1) ? .monitorCrop : .window
        state.setCaptureMode(mode, forApp: filteredApps[row])
    }

    @objc private func appLaunchClicked(_ sender: NSButton) {
        let row = sender.tag
        guard row >= 0 && row < filteredApps.count else { return }
        state.launchApp(filteredApps[row])
    }

    // MARK: - NSTableViewDataSource / Delegate

    func numberOfRows(in tableView: NSTableView) -> Int {
        if tableView === appsTable {
            return filteredApps.count
        }
        return state.availableWindows.count
    }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        guard let colId = tableColumn?.identifier.rawValue else { return nil }
        if tableView === appsTable {
            return appCellView(colId: colId, row: row)
        }
        return windowCellView(colId: colId, row: row)
    }

    private func windowCellView(colId: String, row: Int) -> NSView? {
        guard row >= 0 && row < state.availableWindows.count else { return nil }
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

    private func appCellView(colId: String, row: Int) -> NSView? {
        guard row >= 0 && row < filteredApps.count else { return nil }
        let app = filteredApps[row]
        let pinned = state.isPinned(app.appId)

        switch colId {
        case "app_pin":
            let btn = NSButton(
                image: NSImage(systemSymbolName: pinned ? "star.fill" : "star", accessibilityDescription: "置顶到菜单栏")!,
                target: self,
                action: #selector(appPinClicked(_:))
            )
            btn.isBordered = false
            btn.contentTintColor = pinned ? .systemYellow : .tertiaryLabelColor
            btn.tag = row
            btn.toolTip = pinned ? "取消置顶（从菜单栏移除）" : "置顶到顶部菜单栏一键启动"
            btn.translatesAutoresizingMaskIntoConstraints = false
            let cell = NSView()
            cell.addSubview(btn)
            NSLayoutConstraint.activate([
                btn.centerXAnchor.constraint(equalTo: cell.centerXAnchor),
                btn.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
            ])
            return cell

        case "app_name":
            let cell = NSView()
            let iconView = NSImageView()
            iconView.translatesAutoresizingMaskIntoConstraints = false
            iconView.imageScaling = .scaleProportionallyDown
            if !app.iconPng.isEmpty, let img = NSImage(data: app.iconPng) {
                iconView.image = img
            } else {
                iconView.image = NSImage(systemSymbolName: "app", accessibilityDescription: nil)
            }
            let titleLabel = NSTextField(labelWithString: app.name)
            titleLabel.font = .systemFont(ofSize: 12.5, weight: pinned ? .semibold : .regular)
            titleLabel.lineBreakMode = .byTruncatingTail
            titleLabel.translatesAutoresizingMaskIntoConstraints = false

            cell.addSubview(iconView)
            cell.addSubview(titleLabel)
            NSLayoutConstraint.activate([
                iconView.leadingAnchor.constraint(equalTo: cell.leadingAnchor, constant: 4),
                iconView.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
                iconView.widthAnchor.constraint(equalToConstant: 22),
                iconView.heightAnchor.constraint(equalToConstant: 22),
                titleLabel.leadingAnchor.constraint(equalTo: iconView.trailingAnchor, constant: 8),
                titleLabel.trailingAnchor.constraint(equalTo: cell.trailingAnchor, constant: -4),
                titleLabel.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
            ])
            return cell

        case "app_source":
            let text = app.source == "Desktop" ? "桌面" : "开始菜单"
            let label = NSTextField(labelWithString: text)
            label.font = .systemFont(ofSize: 11.5)
            label.textColor = .secondaryLabelColor
            label.translatesAutoresizingMaskIntoConstraints = false
            let cell = NSView()
            cell.addSubview(label)
            NSLayoutConstraint.activate([
                label.leadingAnchor.constraint(equalTo: cell.leadingAnchor, constant: 4),
                label.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
            ])
            return cell

        case "app_mode":
            let popup = NSPopUpButton()
            popup.controlSize = .small
            popup.font = .systemFont(ofSize: 11.5)
            popup.addItems(withTitles: ["单窗", "屏幕裁剪"])
            popup.selectItem(at: state.captureMode(forApp: app) == .monitorCrop ? 1 : 0)
            popup.tag = row
            popup.target = self
            popup.action = #selector(appModeChanged(_:))
            popup.translatesAutoresizingMaskIntoConstraints = false
            let cell = NSView()
            cell.addSubview(popup)
            NSLayoutConstraint.activate([
                popup.centerXAnchor.constraint(equalTo: cell.centerXAnchor),
                popup.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
                popup.widthAnchor.constraint(equalToConstant: 92),
            ])
            return cell

        case "app_launch":
            let btn = NSButton(title: "启动", target: self, action: #selector(appLaunchClicked(_:)))
            btn.bezelStyle = .rounded
            btn.controlSize = .small
            btn.tag = row
            btn.translatesAutoresizingMaskIntoConstraints = false
            let cell = NSView()
            cell.addSubview(btn)
            NSLayoutConstraint.activate([
                btn.centerXAnchor.constraint(equalTo: cell.centerXAnchor),
                btn.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
                btn.widthAnchor.constraint(equalToConstant: 60),
            ])
            return cell

        default:
            return nil
        }
    }
}
