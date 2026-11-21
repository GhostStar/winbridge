import AppKit

/// Browses the Windows drop folder (and other allowed roots) and pulls a file back to ~/Downloads/WinBridge.
@MainActor
final class FilePullWindowController: NSWindowController, NSTableViewDataSource, NSTableViewDelegate {
    private let state: AppState
    private let pathField = NSTextField(string: "")
    private let table = NSTableView()
    private var entries: [HostFileEntry] = []
    private var currentPath = ""

    init(state: AppState) {
        self.state = state
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 520, height: 420),
            styleMask: [.titled, .closable, .miniaturizable],
            backing: .buffered,
            defer: false
        )
        window.title = "从 Windows 取回文件"
        super.init(window: window)
        build()
        state.onHostFilesListed = { [weak self] res in
            self?.apply(res)
        }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { nil }

    func show() {
        window?.center()
        showWindow(nil)
        NSApp.activate(ignoringOtherApps: true)
        refresh(nil)
    }

    private func build() {
        guard let content = window?.contentView else { return }
        pathField.placeholderString = "空 = Downloads\\WinBridge，或填写允许目录内的路径"
        let refreshBtn = NSButton(title: "刷新", target: self, action: #selector(refresh(_:)))
        let upBtn = NSButton(title: "上一级", target: self, action: #selector(up(_:)))
        let pullBtn = NSButton(title: "取回到 Mac", target: self, action: #selector(pullSelected(_:)))
        let clipBtn = NSButton(title: "取回剪贴板文件", target: self, action: #selector(pullClipboard(_:)))
        pullBtn.bezelStyle = .rounded
        let scroll = NSScrollView()
        scroll.hasVerticalScroller = true
        scroll.translatesAutoresizingMaskIntoConstraints = false
        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("name"))
        column.title = "名称"
        column.width = 360
        table.addTableColumn(column)
        let sizeCol = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("size"))
        sizeCol.title = "大小"
        sizeCol.width = 90
        table.addTableColumn(sizeCol)
        table.headerView = NSTableHeaderView()
        table.dataSource = self
        table.delegate = self
        table.doubleAction = #selector(openRow(_:))
        table.target = self
        scroll.documentView = table

        let row = NSStackView(views: [pathField, refreshBtn, upBtn])
        row.orientation = .horizontal
        pathField.setContentHuggingPriority(.defaultLow, for: .horizontal)
        let buttons = NSStackView(views: [pullBtn, clipBtn])
        buttons.orientation = .horizontal
        let stack = NSStackView(views: [row, scroll, buttons])
        stack.orientation = .vertical
        stack.translatesAutoresizingMaskIntoConstraints = false
        stack.edgeInsets = NSEdgeInsets(top: 12, left: 12, bottom: 12, right: 12)
        content.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            stack.topAnchor.constraint(equalTo: content.topAnchor),
            stack.bottomAnchor.constraint(equalTo: content.bottomAnchor),
            scroll.heightAnchor.constraint(greaterThanOrEqualToConstant: 260),
        ])
    }

    @objc private func refresh(_ sender: Any?) {
        currentPath = pathField.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        state.requestHostFileList(path: currentPath)
    }

    /// Computes the parent directory for a Windows (`C:\Users\me\Downloads`) or POSIX (`/a/b`) path.
    nonisolated static func parentHostPath(_ raw: String) -> String {
        var path = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !path.isEmpty else { return "" }
        let usesBackslash = path.contains("\\")
        let sep: Character = usesBackslash ? "\\" : "/"
        // Strip trailing separators except for roots like `C:\` or `/`.
        while path.count > 1 && path.last == sep {
            if usesBackslash && path.count == 3 && path.dropFirst().first == ":" {
                break
            }
            path.removeLast()
        }
        if usesBackslash && path.count == 3 && path.dropFirst().first == ":" && path.last == "\\" {
            return path
        }
        if !usesBackslash && path == "/" {
            return "/"
        }
        guard let idx = path.lastIndex(of: sep) else {
            return ""
        }
        let prefix = String(path[..<idx])
        if usesBackslash {
            // `C:` -> `C:\`
            if prefix.count == 2 && prefix.last == ":" {
                return prefix + "\\"
            }
            return prefix
        }
        return prefix.isEmpty ? "/" : prefix
    }

    @objc private func up(_ sender: Any?) {
        guard !currentPath.isEmpty else { return }
        let next = Self.parentHostPath(currentPath)
        pathField.stringValue = next == currentPath ? currentPath : next
        refresh(nil)
    }

    @objc private func pullSelected(_ sender: Any?) {
        let row = table.selectedRow
        guard row >= 0, row < entries.count else {
            state.setBanner("请先选中一个文件", isError: true)
            return
        }
        let entry = entries[row]
        guard !entry.isDir else {
            enter(entry)
            return
        }
        state.pullHostFile(path: joined(entry.name))
    }

    @objc private func pullClipboard(_ sender: Any?) {
        state.pullHostFile(path: "clipboard:")
    }

    @objc private func openRow(_ sender: Any?) {
        let row = table.clickedRow
        guard row >= 0, row < entries.count else { return }
        let entry = entries[row]
        if entry.isDir {
            enter(entry)
        } else {
            state.pullHostFile(path: joined(entry.name))
        }
    }

    private func enter(_ entry: HostFileEntry) {
        pathField.stringValue = joined(entry.name)
        refresh(nil)
    }

    private func joined(_ name: String) -> String {
        if currentPath.isEmpty { return name }
        if currentPath.hasSuffix("\\") || currentPath.hasSuffix("/") {
            return currentPath + name
        }
        let sep = currentPath.contains("\\") ? "\\" : "/"
        return currentPath + sep + name
    }

    private func apply(_ res: ListHostFilesResponse) {
        currentPath = res.path
        if pathField.stringValue.isEmpty || !res.error.isEmpty {
            pathField.stringValue = res.path
        }
        entries = res.entries
        table.reloadData()
    }

    func numberOfRows(in tableView: NSTableView) -> Int { entries.count }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        let entry = entries[row]
        let id = tableColumn?.identifier ?? NSUserInterfaceItemIdentifier("name")
        let cell = tableView.makeView(withIdentifier: id, owner: self) as? NSTextField
            ?? NSTextField(labelWithString: "")
        cell.identifier = id
        cell.lineBreakMode = .byTruncatingMiddle
        if id.rawValue == "size" {
            cell.stringValue = entry.isDir ? "文件夹" : byteLabel(entry.sizeBytes)
        } else {
            cell.stringValue = (entry.isDir ? "▸ " : "") + entry.name
        }
        return cell
    }

    private func byteLabel(_ n: UInt64) -> String {
        if n < 1024 { return "\(n) B" }
        if n < 1024 * 1024 { return String(format: "%.1f KB", Double(n) / 1024) }
        return String(format: "%.1f MB", Double(n) / 1024 / 1024)
    }
}
