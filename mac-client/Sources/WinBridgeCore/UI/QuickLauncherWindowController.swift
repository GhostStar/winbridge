import AppKit
import Carbon.HIToolbox
import Foundation

private final class LauncherPanel: NSPanel {
    override var canBecomeKey: Bool { true }
    override var canBecomeMain: Bool { false }
}

@MainActor
final class QuickLauncherWindowController: NSWindowController, NSWindowDelegate, NSTextFieldDelegate, NSTableViewDataSource, NSTableViewDelegate {
    private let state: AppState

    private let searchField = NSTextField()
    private let hintLabel = NSTextField(labelWithString: "↑↓ 选择 · ⏎ 打开/切换 · Esc 关闭")
    private let emptyLabel = NSTextField(labelWithString: "没有匹配的 Windows 窗口或应用")
    private let tableView = NSTableView()
    private let scrollView = NSScrollView()

    private var items: [QuickLauncherItem] = []

    init(state: AppState) {
        self.state = state
        let panel = LauncherPanel(
            contentRect: NSRect(x: 0, y: 0, width: 620, height: 420),
            styleMask: [.borderless, .fullSizeContentView],
            backing: .buffered,
            defer: false
        )
        panel.level = .floating
        panel.isFloatingPanel = true
        panel.backgroundColor = .clear
        panel.isOpaque = false
        panel.hasShadow = true
        panel.isMovableByWindowBackground = true
        panel.hidesOnDeactivate = false
        panel.collectionBehavior = [.moveToActiveSpace, .transient]
        panel.isReleasedWhenClosed = false
        super.init(window: panel)
        panel.delegate = self

        buildUI(in: panel)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    private func buildUI(in panel: NSPanel) {
        let effectView = NSVisualEffectView()
        effectView.material = .hudWindow
        effectView.blendingMode = .behindWindow
        effectView.state = .active
        effectView.wantsLayer = true
        effectView.layer?.cornerRadius = 14
        effectView.layer?.masksToBounds = true
        effectView.layer?.borderWidth = 1
        effectView.layer?.borderColor = NSColor.separatorColor.withAlphaComponent(0.45).cgColor
        panel.contentView = effectView

        let searchIcon = NSImageView()
        searchIcon.image = NSImage(systemSymbolName: "magnifyingglass", accessibilityDescription: nil)
        searchIcon.contentTintColor = .secondaryLabelColor
        searchIcon.symbolConfiguration = NSImage.SymbolConfiguration(pointSize: 18, weight: .medium)
        searchIcon.translatesAutoresizingMaskIntoConstraints = false

        searchField.isBordered = false
        searchField.isBezeled = false
        searchField.drawsBackground = false
        searchField.focusRingType = .none
        searchField.font = .systemFont(ofSize: 20, weight: .regular)
        searchField.placeholderString = "搜索 Windows 窗口或应用…"
        searchField.delegate = self
        searchField.translatesAutoresizingMaskIntoConstraints = false

        let shortcutPill = NSTextField(labelWithString: "⌥Space")
        shortcutPill.font = .monospacedSystemFont(ofSize: 11, weight: .medium)
        shortcutPill.textColor = .tertiaryLabelColor
        shortcutPill.translatesAutoresizingMaskIntoConstraints = false

        let headerView = NSView()
        headerView.translatesAutoresizingMaskIntoConstraints = false
        headerView.addSubview(searchIcon)
        headerView.addSubview(searchField)
        headerView.addSubview(shortcutPill)

        let divider = NSBox()
        divider.boxType = .separator
        divider.translatesAutoresizingMaskIntoConstraints = false

        let col = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("result"))
        col.resizingMask = .autoresizingMask
        tableView.addTableColumn(col)
        tableView.headerView = nil
        tableView.rowHeight = 46
        tableView.backgroundColor = .clear
        tableView.selectionHighlightStyle = .regular
        tableView.intercellSpacing = NSSize(width: 0, height: 2)
        tableView.dataSource = self
        tableView.delegate = self
        tableView.target = self
        tableView.action = #selector(rowClicked)
        tableView.doubleAction = #selector(rowDoubleClicked)

        scrollView.documentView = tableView
        scrollView.drawsBackground = false
        scrollView.hasVerticalScroller = true
        scrollView.autohidesScrollers = true
        scrollView.borderType = .noBorder
        scrollView.translatesAutoresizingMaskIntoConstraints = false

        emptyLabel.font = .systemFont(ofSize: 13)
        emptyLabel.textColor = .secondaryLabelColor
        emptyLabel.alignment = .center
        emptyLabel.isHidden = true
        emptyLabel.translatesAutoresizingMaskIntoConstraints = false

        let footerDivider = NSBox()
        footerDivider.boxType = .separator
        footerDivider.translatesAutoresizingMaskIntoConstraints = false

        hintLabel.font = .systemFont(ofSize: 11)
        hintLabel.textColor = .secondaryLabelColor
        hintLabel.translatesAutoresizingMaskIntoConstraints = false

        effectView.addSubview(headerView)
        effectView.addSubview(divider)
        effectView.addSubview(scrollView)
        effectView.addSubview(emptyLabel)
        effectView.addSubview(footerDivider)
        effectView.addSubview(hintLabel)

        NSLayoutConstraint.activate([
            headerView.topAnchor.constraint(equalTo: effectView.topAnchor, constant: 12),
            headerView.leadingAnchor.constraint(equalTo: effectView.leadingAnchor, constant: 16),
            headerView.trailingAnchor.constraint(equalTo: effectView.trailingAnchor, constant: -16),
            headerView.heightAnchor.constraint(equalToConstant: 36),

            searchIcon.leadingAnchor.constraint(equalTo: headerView.leadingAnchor),
            searchIcon.centerYAnchor.constraint(equalTo: headerView.centerYAnchor),
            searchIcon.widthAnchor.constraint(equalToConstant: 24),

            searchField.leadingAnchor.constraint(equalTo: searchIcon.trailingAnchor, constant: 8),
            searchField.trailingAnchor.constraint(equalTo: shortcutPill.leadingAnchor, constant: -8),
            searchField.centerYAnchor.constraint(equalTo: headerView.centerYAnchor),

            shortcutPill.trailingAnchor.constraint(equalTo: headerView.trailingAnchor),
            shortcutPill.centerYAnchor.constraint(equalTo: headerView.centerYAnchor),

            divider.topAnchor.constraint(equalTo: headerView.bottomAnchor, constant: 10),
            divider.leadingAnchor.constraint(equalTo: effectView.leadingAnchor),
            divider.trailingAnchor.constraint(equalTo: effectView.trailingAnchor),

            scrollView.topAnchor.constraint(equalTo: divider.bottomAnchor, constant: 4),
            scrollView.leadingAnchor.constraint(equalTo: effectView.leadingAnchor, constant: 6),
            scrollView.trailingAnchor.constraint(equalTo: effectView.trailingAnchor, constant: -6),
            scrollView.bottomAnchor.constraint(equalTo: footerDivider.topAnchor, constant: -4),

            emptyLabel.centerXAnchor.constraint(equalTo: scrollView.centerXAnchor),
            emptyLabel.centerYAnchor.constraint(equalTo: scrollView.centerYAnchor),

            footerDivider.leadingAnchor.constraint(equalTo: effectView.leadingAnchor),
            footerDivider.trailingAnchor.constraint(equalTo: effectView.trailingAnchor),
            footerDivider.bottomAnchor.constraint(equalTo: hintLabel.topAnchor, constant: -7),

            hintLabel.leadingAnchor.constraint(equalTo: effectView.leadingAnchor, constant: 16),
            hintLabel.trailingAnchor.constraint(equalTo: effectView.trailingAnchor, constant: -16),
            hintLabel.bottomAnchor.constraint(equalTo: effectView.bottomAnchor, constant: -8),
        ])
    }

    var isShown: Bool {
        window?.isVisible == true
    }

    func toggle() {
        if isShown {
            hide()
        } else {
            show()
        }
    }

    func show() {
        guard let window else { return }
        searchField.stringValue = ""
        positionOnActiveScreen(window)
        refreshResults()

        if case .connected = state.connectionState {
            state.refreshWindowList()
            if state.availableApps.isEmpty {
                state.refreshAppList()
            }
        }

        NSApp.activate(ignoringOtherApps: true)
        window.makeKeyAndOrderFront(nil)
        window.makeFirstResponder(searchField)
    }

    func hide() {
        window?.orderOut(nil)
    }

    private func positionOnActiveScreen(_ window: NSWindow) {
        let mouse = NSEvent.mouseLocation
        let screen = NSScreen.screens.first(where: { $0.frame.contains(mouse) }) ?? NSScreen.main
        guard let visible = screen?.visibleFrame else {
            window.center()
            return
        }
        let width: CGFloat = 620
        let height: CGFloat = 420
        let x = visible.midX - width * 0.5
        let y = visible.minY + (visible.height - height) * 0.68
        window.setFrame(NSRect(x: round(x), y: round(y), width: width, height: height), display: true)
    }

    func refreshResults() {
        let isConnected: Bool
        if case .connected(let hostName, _) = state.connectionState {
            isConnected = true
            hintLabel.stringValue = "已连接 \(hostName) · ↑↓ 选择 · ⏎ 切换/启动 · Esc 关闭"
        } else {
            isConnected = false
            hintLabel.stringValue = "未连接 Windows 主机 · ⏎ 快速连接或唤醒 · Esc 关闭"
        }

        items = QuickLauncherMatcher.search(
            query: searchField.stringValue,
            isConnected: isConnected,
            host: state.settings.host,
            port: state.settings.port,
            macAddress: state.settings.macAddress,
            windows: state.availableWindows,
            subscribedRootIds: state.subscribedRootIds,
            apps: state.availableApps,
            pinnedAppIds: Set(state.settings.pinnedAppIds),
            isDesktopOpen: state.isDesktopOpen
        )

        tableView.reloadData()
        emptyLabel.isHidden = !items.isEmpty
        if !items.isEmpty {
            tableView.selectRowIndexes(IndexSet(integer: 0), byExtendingSelection: false)
            tableView.scrollRowToVisible(0)
        }
    }

    private func moveSelection(delta: Int) {
        guard !items.isEmpty else { return }
        let current = tableView.selectedRow
        let next: Int
        if current < 0 {
            next = 0
        } else {
            next = (current + delta + items.count) % items.count
        }
        tableView.selectRowIndexes(IndexSet(integer: next), byExtendingSelection: false)
        tableView.scrollRowToVisible(next)
    }

    private func activateSelected() {
        let row = tableView.selectedRow
        let index = (row >= 0 && row < items.count) ? row : 0
        guard index < items.count else {
            hide()
            return
        }
        let item = items[index]
        hide()
        switch item {
        case .window(let info, _):
            state.focusOrSubscribe(info)
        case .app(let app, _):
            state.launchApp(app)
        case .desktop:
            if state.isDesktopOpen {
                state.focusDesktopIfOpen()
            } else {
                state.toggleDesktopView()
            }
        case .connectHost:
            state.connect()
        case .wakeHost:
            state.wakeAndConnect()
        }
    }

    // MARK: - NSWindowDelegate & NSTextFieldDelegate

    func windowDidResignKey(_ notification: Notification) {
        hide()
    }

    func controlTextDidChange(_ obj: Notification) {
        refreshResults()
    }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy commandSelector: Selector) -> Bool {
        switch commandSelector {
        case #selector(NSResponder.moveDown(_:)):
            moveSelection(delta: 1)
            return true
        case #selector(NSResponder.moveUp(_:)):
            moveSelection(delta: -1)
            return true
        case #selector(NSResponder.insertNewline(_:)):
            activateSelected()
            return true
        case #selector(NSResponder.cancelOperation(_:)):
            hide()
            return true
        default:
            return false
        }
    }

    @objc private func rowClicked() {
        let row = tableView.clickedRow
        guard row >= 0 && row < items.count else { return }
        tableView.selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
    }

    @objc private func rowDoubleClicked() {
        let row = tableView.clickedRow
        guard row >= 0 && row < items.count else { return }
        tableView.selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
        activateSelected()
    }

    // MARK: - NSTableViewDataSource / Delegate

    func numberOfRows(in tableView: NSTableView) -> Int {
        items.count
    }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        guard row >= 0 && row < items.count else { return nil }
        let item = items[row]

        let cell = NSView()
        let iconView = NSImageView()
        iconView.translatesAutoresizingMaskIntoConstraints = false
        iconView.imageScaling = .scaleProportionallyDown
        if !item.iconPng.isEmpty, let img = NSImage(data: item.iconPng) {
            iconView.image = img
        } else {
            let symbolName: String
            switch item {
            case .window: symbolName = "macwindow"
            case .app: symbolName = "app.fill"
            case .desktop: symbolName = "desktopcomputer"
            case .connectHost: symbolName = "network"
            case .wakeHost: symbolName = "bolt.fill"
            }
            iconView.image = NSImage(systemSymbolName: symbolName, accessibilityDescription: nil)
            iconView.contentTintColor = .controlAccentColor
        }

        let titleLabel = NSTextField(labelWithString: item.title)
        titleLabel.font = .systemFont(ofSize: 13.5, weight: .medium)
        titleLabel.lineBreakMode = .byTruncatingTail

        let subLabel = NSTextField(labelWithString: item.subtitle)
        subLabel.font = .systemFont(ofSize: 11)
        subLabel.textColor = .secondaryLabelColor
        subLabel.lineBreakMode = .byTruncatingTail

        let textStack = NSStackView(views: [titleLabel, subLabel])
        textStack.orientation = .vertical
        textStack.alignment = .leading
        textStack.spacing = 1
        textStack.translatesAutoresizingMaskIntoConstraints = false

        let badge = NSTextField(labelWithString: item.badgeText)
        badge.font = .systemFont(ofSize: 10.5, weight: .medium)
        badge.textColor = .secondaryLabelColor
        badge.setContentHuggingPriority(.required, for: .horizontal)
        badge.translatesAutoresizingMaskIntoConstraints = false

        cell.addSubview(iconView)
        cell.addSubview(textStack)
        cell.addSubview(badge)

        NSLayoutConstraint.activate([
            iconView.leadingAnchor.constraint(equalTo: cell.leadingAnchor, constant: 10),
            iconView.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
            iconView.widthAnchor.constraint(equalToConstant: 26),
            iconView.heightAnchor.constraint(equalToConstant: 26),

            textStack.leadingAnchor.constraint(equalTo: iconView.trailingAnchor, constant: 10),
            textStack.trailingAnchor.constraint(lessThanOrEqualTo: badge.leadingAnchor, constant: -10),
            textStack.centerYAnchor.constraint(equalTo: cell.centerYAnchor),

            badge.trailingAnchor.constraint(equalTo: cell.trailingAnchor, constant: -12),
            badge.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
        ])
        return cell
    }
}
