import AppKit
import Foundation
import UniformTypeIdentifiers

private struct PromisedHostItem: Sendable {
    let remotePath: String
    let suggestedFileName: String
}

private final class SendableCompletionBox: @unchecked Sendable {
    private let handler: (Error?) -> Void
    init(_ handler: @escaping (Error?) -> Void) {
        self.handler = handler
    }
    func call(_ error: Error?) {
        handler(error)
    }
}

private final class HostFilePromiseDelegate: NSObject, NSFilePromiseProviderDelegate, @unchecked Sendable {
    private weak var state: AppState?
    private let queue: OperationQueue = {
        let q = OperationQueue()
        q.name = "com.winbridge.client.filepromise"
        q.qualityOfService = .userInitiated
        return q
    }()

    init(state: AppState) {
        self.state = state
    }

    func filePromiseProvider(_ filePromiseProvider: NSFilePromiseProvider, fileNameForType fileType: String) -> String {
        (filePromiseProvider.userInfo as? PromisedHostItem)?.suggestedFileName ?? "winbridge-file.bin"
    }

    func operationQueue(for filePromiseProvider: NSFilePromiseProvider) -> OperationQueue {
        queue
    }

    func filePromiseProvider(
        _ filePromiseProvider: NSFilePromiseProvider,
        writePromiseTo url: URL,
        completionHandler: @escaping (Error?) -> Void
    ) {
        guard let item = filePromiseProvider.userInfo as? PromisedHostItem else {
            completionHandler(NSError(
                domain: "com.winbridge.client",
                code: 10,
                userInfo: [NSLocalizedDescriptionKey: "无效的远端文件拖放条目"]
            ))
            return
        }
        let box = SendableCompletionBox(completionHandler)
        let weakState = state
        Task { @MainActor in
            guard let strongState = weakState else {
                box.call(NSError(
                    domain: "com.winbridge.client",
                    code: 11,
                    userInfo: [NSLocalizedDescriptionKey: "WinBridge 客户端已关闭"]
                ))
                return
            }
            strongState.pullHostFileToDestination(
                remotePath: item.remotePath,
                destinationURL: url
            ) { err in
                box.call(err)
            }
        }
    }
}

/// Browses Windows allowed roots (WinBridge drop / Desktop / Downloads / Documents)
/// and supports both button pull and native macOS drag-out (`NSFilePromiseProvider`) to Finder/Desktop.
@MainActor
final class FilePullWindowController: NSWindowController, NSTableViewDataSource, NSTableViewDelegate {
    private let state: AppState
    private let promiseDelegate: HostFilePromiseDelegate
    private let pathField = NSTextField(string: "")
    private let statusLabel = NSTextField(labelWithString: "提示：可直接把列表中的文件或文件夹拖到 Mac 桌面 / Finder（文件夹自动打包为 .zip）")
    private let table = NSTableView()
    private var entries: [HostFileEntry] = []
    private var currentPath = ""

    init(state: AppState) {
        self.state = state
        self.promiseDelegate = HostFilePromiseDelegate(state: state)
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 600, height: 470),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.title = "从 Windows 取回文件"
        window.minSize = NSSize(width: 480, height: 340)
        super.init(window: window)
        build()
        state.onHostFilesListed = { [weak self] res in
            self?.apply(res)
        }
        state.onFilePullStatusChanged = { [weak self] text, isError in
            self?.updateStatus(text, isError: isError)
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

        // Quick root shortcuts
        let dropRootBtn = makeShortcutButton(title: "WinBridge", rootAlias: "root:drop")
        let desktopBtn = makeShortcutButton(title: "桌面", rootAlias: "root:desktop")
        let downloadsBtn = makeShortcutButton(title: "下载", rootAlias: "root:downloads")
        let docsBtn = makeShortcutButton(title: "文档", rootAlias: "root:documents")
        let shortcutLabel = NSTextField(labelWithString: "快捷目录：")
        shortcutLabel.font = .systemFont(ofSize: 12, weight: .medium)
        shortcutLabel.textColor = .secondaryLabelColor
        let shortcutRow = NSStackView(views: [shortcutLabel, dropRootBtn, desktopBtn, downloadsBtn, docsBtn])
        shortcutRow.orientation = .horizontal
        shortcutRow.spacing = 6

        pathField.placeholderString = "空 = Downloads\\WinBridge，或输入允许目录内的路径后回车"
        pathField.target = self
        pathField.action = #selector(refresh(_:))
        let refreshBtn = NSButton(title: "刷新", target: self, action: #selector(refresh(_:)))
        let upBtn = NSButton(title: "上一级", target: self, action: #selector(up(_:)))

        let pullBtn = NSButton(title: "取回选中项到 Mac（写入 ⌘V）", target: self, action: #selector(pullSelected(_:)))
        pullBtn.bezelStyle = .rounded
        pullBtn.keyEquivalent = "\r"
        let clipBtn = NSButton(title: "取回 Windows 剪贴板文件 (⌥⇧⌘V)", target: self, action: #selector(pullClipboard(_:)))
        let openDownloadsBtn = NSButton(title: "在 Finder 中查看接收目录", target: self, action: #selector(openMacDropFolder(_:)))

        let scroll = NSScrollView()
        scroll.hasVerticalScroller = true
        scroll.translatesAutoresizingMaskIntoConstraints = false
        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("name"))
        column.title = "名称（支持多选，可直接拖出到 Mac 桌面 / Finder）"
        column.width = 430
        column.resizingMask = .autoresizingMask
        table.addTableColumn(column)
        let sizeCol = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("size"))
        sizeCol.title = "大小"
        sizeCol.width = 110
        table.addTableColumn(sizeCol)
        table.headerView = NSTableHeaderView()
        table.allowsMultipleSelection = true
        table.columnAutoresizingStyle = .firstColumnOnlyAutoresizingStyle
        table.setDraggingSourceOperationMask(.copy, forLocal: false)
        table.dataSource = self
        table.delegate = self
        table.doubleAction = #selector(openRow(_:))
        table.target = self
        scroll.documentView = table

        statusLabel.font = .systemFont(ofSize: 11)
        statusLabel.textColor = .secondaryLabelColor
        statusLabel.lineBreakMode = .byTruncatingMiddle

        let pathRow = NSStackView(views: [pathField, refreshBtn, upBtn])
        pathRow.orientation = .horizontal
        pathRow.spacing = 6
        pathField.setContentHuggingPriority(.defaultLow, for: .horizontal)

        let buttons = NSStackView(views: [pullBtn, clipBtn, openDownloadsBtn])
        buttons.orientation = .horizontal
        buttons.spacing = 8

        let stack = NSStackView(views: [shortcutRow, pathRow, scroll, buttons, statusLabel])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 8
        stack.translatesAutoresizingMaskIntoConstraints = false
        stack.edgeInsets = NSEdgeInsets(top: 12, left: 12, bottom: 12, right: 12)
        content.addSubview(stack)

        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            stack.topAnchor.constraint(equalTo: content.topAnchor),
            stack.bottomAnchor.constraint(equalTo: content.bottomAnchor),
            pathRow.widthAnchor.constraint(equalTo: stack.widthAnchor, constant: -24),
            scroll.widthAnchor.constraint(equalTo: stack.widthAnchor, constant: -24),
            scroll.heightAnchor.constraint(greaterThanOrEqualToConstant: 240),
            statusLabel.widthAnchor.constraint(equalTo: stack.widthAnchor, constant: -24),
        ])
    }

    private func makeShortcutButton(title: String, rootAlias: String) -> NSButton {
        let btn = NSButton(title: title, target: self, action: #selector(openShortcutRoot(_:)))
        btn.bezelStyle = .recessed
        btn.controlSize = .small
        btn.identifier = NSUserInterfaceItemIdentifier(rootAlias)
        return btn
    }

    @objc private func openShortcutRoot(_ sender: NSButton) {
        guard let alias = sender.identifier?.rawValue else { return }
        pathField.stringValue = ""
        currentPath = alias
        state.requestHostFileList(path: alias)
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
        let rows = table.selectedRowIndexes
        guard !rows.isEmpty else {
            updateStatus("请先选中一个或多个文件 / 文件夹", isError: true)
            return
        }
        for row in rows where row >= 0 && row < entries.count {
            let entry = entries[row]
            state.pullHostFile(path: joined(entry.name), copyToPasteboard: true)
        }
    }

    @objc private func pullClipboard(_ sender: Any?) {
        state.pullClipboardFilesToPasteboard(isAuto: false)
    }

    @objc private func openMacDropFolder(_ sender: Any?) {
        let base = FileManager.default.urls(for: .downloadsDirectory, in: .userDomainMask).first
            ?? URL(fileURLWithPath: NSTemporaryDirectory())
        let dir = base.appendingPathComponent("WinBridge", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        NSWorkspace.shared.open(dir)
    }

    @objc private func openRow(_ sender: Any?) {
        let row = table.clickedRow
        guard row >= 0, row < entries.count else { return }
        let entry = entries[row]
        if entry.isDir {
            enter(entry)
        } else {
            state.pullHostFile(path: joined(entry.name), copyToPasteboard: true)
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
        pathField.stringValue = res.path
        entries = res.entries
        table.reloadData()
        if !res.error.isEmpty {
            updateStatus("无法打开目录：\(res.error)", isError: true)
        } else {
            updateStatus("共 \(entries.count) 项 · 可直接将列表中的文件/文件夹拖到 Mac 桌面或 Finder", isError: false)
        }
    }

    private func updateStatus(_ text: String, isError: Bool) {
        statusLabel.stringValue = text
        statusLabel.textColor = isError ? .systemRed : .secondaryLabelColor
    }

    // MARK: - NSTableViewDataSource & Drag-Out Promise

    func numberOfRows(in tableView: NSTableView) -> Int { entries.count }

    func tableView(_ tableView: NSTableView, pasteboardWriterForRow row: Int) -> (any NSPasteboardWriting)? {
        guard row >= 0, row < entries.count else { return nil }
        let entry = entries[row]
        let suggestedName = entry.isDir ? "\(entry.name).zip" : entry.name
        let ext = (suggestedName as NSString).pathExtension
        let fileType: String
        if entry.isDir {
            fileType = UTType.zip.identifier
        } else if !ext.isEmpty, let inferred = UTType(filenameExtension: ext) {
            fileType = inferred.identifier
        } else {
            fileType = UTType.data.identifier
        }
        let provider = NSFilePromiseProvider(fileType: fileType, delegate: promiseDelegate)
        provider.userInfo = PromisedHostItem(
            remotePath: joined(entry.name),
            suggestedFileName: suggestedName
        )
        return provider
    }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        let entry = entries[row]
        let id = tableColumn?.identifier ?? NSUserInterfaceItemIdentifier("name")
        let cell = tableView.makeView(withIdentifier: id, owner: self) as? NSTextField
            ?? NSTextField(labelWithString: "")
        cell.identifier = id
        cell.lineBreakMode = .byTruncatingMiddle
        if id.rawValue == "size" {
            cell.stringValue = entry.isDir ? "文件夹 (.zip)" : AppState.formatByteCount(entry.sizeBytes)
        } else {
            cell.stringValue = (entry.isDir ? "📁 " : "📄 ") + entry.name
        }
        return cell
    }
}
