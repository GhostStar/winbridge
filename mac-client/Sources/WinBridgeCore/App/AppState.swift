import AppKit
import Foundation

enum ConnectionState: Equatable {
    case disconnected
    case connecting
    case connected(hostName: String, codec: VideoCodec)
}

private struct RestoreWindowTarget: Equatable {
    let isDesktop: Bool
    let processName: String
    let title: String
}

@MainActor
final class AppState {
    var settings: ClientSettings {
        didSet {
            SettingsStore.save(settings)
            clipboardMonitor.isEnabled = settings.syncClipboard
            audioPlayer.volume = settings.audioVolume
            if oldValue.syncWindowResize != settings.syncWindowResize {
                for (_, wc) in controllers {
                    wc.updateResizeBehavior()
                }
            }
            if oldValue.showPerformanceHud != settings.showPerformanceHud {
                for (_, wc) in controllers {
                    wc.updateHudVisibility()
                }
            }
            if oldValue.audioEnabled != settings.audioEnabled || oldValue.audioQuality != settings.audioQuality {
                applyAudioSetting()
            }
            if !settings.autoReconnect && oldValue.autoReconnect {
                cancelReconnect()
            }
        }
    }

    private(set) var connectionState: ConnectionState = .disconnected
    private(set) var rttMs: Double?
    private(set) var availableWindows: [WindowInfo] = []
    private(set) var availableApps: [AppEntry] = []
    private(set) var availableMonitors: [MonitorInfo] = []
    private(set) var discoveredHosts: [DiscoveredHost] = []
    private(set) var subscribedRootIds: Set<UInt64> = []
    private(set) var lastBannerMessage: String = ""
    private(set) var lastBannerIsError: Bool = false

    var isReconnecting: Bool { reconnectTask != nil }

    var isDesktopOpen: Bool {
        subscribedRootIds.contains(SyntheticWindow.primaryDesktop)
    }

    var pinnedApps: [AppEntry] {
        let pinnedSet = Set(settings.pinnedAppIds)
        return availableApps.filter { pinnedSet.contains($0.appId) }
    }

    private var connection: WinBridgeConnection?
    private var controllers: [UInt64: RemoteWindowController] = [:]
    private let clipboardMonitor = ClipboardMonitor()
    private let audioPlayer = AudioPlayer()
    private let hostDiscovery = HostDiscovery()
    private let gamepadMonitor = GamepadMonitor()

    private var manualDisconnectRequested = false
    private var reconnectTask: Task<Void, Never>?
    private var reconnectAttempt = 0
    private var pendingRestoreTargets: [RestoreWindowTarget] = []
    private var nextTransferId: UInt64 = 1
    private var activeTransferNames: [UInt64: String] = [:]

    var onStateChanged: (() -> Void)?
    var onOpenManager: (() -> Void)?

    init() {
        self.settings = SettingsStore.load()
        clipboardMonitor.isEnabled = settings.syncClipboard
        audioPlayer.volume = settings.audioVolume
        clipboardMonitor.onLocalChange = { [weak self] sync in
            self?.sendControl(.clipboardSync(sync))
        }
        hostDiscovery.onHostsChanged = { [weak self] hosts in
            self?.discoveredHosts = hosts
            self?.onStateChanged?()
        }
        gamepadMonitor.shouldForward = { [weak self] in
            guard let self, case .connected = self.connectionState else { return false }
            return NSApp.keyWindow?.windowController is RemoteWindowController
        }
        gamepadMonitor.onGamepadState = { [weak self] state in
            self?.sendControl(.gamepadState(state))
        }
    }

    func startDiscovery() {
        hostDiscovery.start()
    }

    func stopDiscovery() {
        hostDiscovery.stop()
    }

    func selectDiscoveredHost(_ host: DiscoveredHost) {
        var s = settings
        s.host = host.host
        s.port = host.port
        s.useTls = host.tlsEnabled
        settings = s
        setBanner("已选择局域网主机 \(host.name)（\(host.host):\(host.port)）", isError: false)
        onStateChanged?()
    }

    func toggleAudioEnabled() {
        var s = settings
        s.audioEnabled.toggle()
        settings = s
        onStateChanged?()
    }

    func togglePerformanceHud() {
        var s = settings
        s.showPerformanceHud.toggle()
        settings = s
        onStateChanged?()
    }

    func mouseSensitivity(forProcess processName: String) -> Float {
        let key = processName.lowercased()
        if !key.isEmpty, let saved = settings.processMouseSensitivity[key] {
            return saved
        }
        return settings.mouseSensitivity
    }

    func setMouseSensitivity(_ value: Float, forProcess processName: String?) {
        let clamped = max(0.1, min(4.0, value))
        var s = settings
        s.mouseSensitivity = clamped
        if let proc = processName?.lowercased(), !proc.isEmpty {
            s.processMouseSensitivity[proc] = clamped
        }
        settings = s
    }

    func activeRemoteProcessName() -> String? {
        guard let wc = NSApp.keyWindow?.windowController as? RemoteWindowController else { return nil }
        return wc.info.processName.isEmpty ? nil : wc.info.processName
    }

    func isAutoGameLockEnabled(forProcess processName: String) -> Bool {
        let key = processName.lowercased()
        return !key.isEmpty && (settings.processAutoGameLock[key] == true)
    }

    func toggleAutoGameLockForActiveWindow() {
        guard let wc = NSApp.keyWindow?.windowController as? RemoteWindowController,
              !wc.info.processName.isEmpty else {
            setBanner("请先选中一个正在投射的 Windows 游戏窗口", isError: true)
            onStateChanged?()
            return
        }
        let key = wc.info.processName.lowercased()
        var s = settings
        let next = !(s.processAutoGameLock[key] == true)
        s.processAutoGameLock[key] = next
        settings = s
        setBanner(
            next
                ? "已记住：下次打开 \(wc.info.processName) 时自动开启游戏鼠标锁定"
                : "已取消 \(wc.info.processName) 的自动游戏鼠标锁定",
            isError: false
        )
        onStateChanged?()
    }

    private func applyAudioSetting() {
        guard case .connected = connectionState else {
            audioPlayer.setEnabled(false)
            return
        }
        audioPlayer.setEnabled(settings.audioEnabled)
        sendControl(.setAudioConfig(SetAudioConfig(
            enabled: settings.audioEnabled,
            codec: settings.audioQuality.audioCodec,
            bitrateKbps: settings.audioQuality.bitrateKbps
        )))
    }

    // MARK: - Connection Lifecycle & Auto-Reconnect

    func connect(isAutoReconnect: Bool = false) {
        guard connectionState == .disconnected else { return }
        if !isAutoReconnect {
            cancelReconnect()
            manualDisconnectRequested = false
            reconnectAttempt = 0
            pendingRestoreTargets.removeAll()
        }
        let host = settings.host.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !host.isEmpty else {
            setBanner("请输入 Windows 主机内网 IP 或从局域网发现中选择", isError: true)
            return
        }

        connectionState = .connecting
        ClientLogger.shared.info("Connecting to \(host):\(settings.port) tls=\(settings.useTls) auto=\(isAutoReconnect)")
        setBanner(
            isAutoReconnect
                ? "正在自动重连 \(host):\(settings.port)（第 \(reconnectAttempt) 次）…"
                : "正在连接 \(host):\(settings.port)…",
            isError: false
        )
        onStateChanged?()

        let conn = WinBridgeConnection()
        self.connection = conn

        conn.onControlMessage = { [weak self] msg in
            Task { @MainActor in
                self?.handleControlMessage(msg)
            }
        }
        let player = self.audioPlayer
        conn.onMediaFrame = { [weak self] frame in
            if frame.header.codec == .audioPcmS16Le || frame.header.codec == .audioOpus {
                player.enqueue(frame: frame)
                return
            }
            Task { @MainActor in
                self?.controllers[frame.header.windowId]?.handleMediaFrame(frame)
            }
        }
        conn.onRttUpdated = { [weak self] rtt in
            Task { @MainActor in
                self?.rttMs = rtt
                self?.onStateChanged?()
            }
        }
        conn.onDisconnected = { [weak self] error in
            Task { @MainActor in
                self?.handleDisconnected(error: error)
            }
        }

        let hostKey = CertificateFingerprint.hostKey(host: host, port: settings.port)
        let displayScale = Float(NSScreen.main?.backingScaleFactor ?? 2.0)
        let config = ConnectionConfig(
            host: host,
            port: settings.port,
            authToken: settings.authToken.trimmingCharacters(in: .whitespacesAndNewlines),
            useTls: settings.useTls,
            expectedFingerprint: settings.trustedFingerprints[hostKey],
            clientName: Host.current().localizedName ?? "macOS",
            displayScale: displayScale,
            supportedCodecs: settings.codecPreference.offeredCodecs
        )

        Task {
            do {
                let outcome = try await conn.connect(config: config)
                let ack = outcome.ack
                if let fp = outcome.tlsFingerprint {
                    var s = self.settings
                    s.trustedFingerprints[hostKey] = fp
                    self.settings = s
                }
                self.reconnectAttempt = 0
                self.reconnectTask?.cancel()
                self.reconnectTask = nil
                self.connectionState = .connected(hostName: ack.hostName, codec: ack.selectedCodec)
                self.rttMs = nil
                let tlsNote: String
                if let fp = outcome.tlsFingerprint {
                    let shortFp = CertificateFingerprint.shortDisplay(fp)
                    tlsNote = outcome.isFirstTrust ? " · TLS 首次信任 \(shortFp)" : " · TLS 已验证"
                } else {
                    tlsNote = " · 明文"
                }
                ClientLogger.shared.info("Connected to \(ack.hostName) codec=\(ack.selectedCodec.displayName)\(tlsNote)")
                self.setBanner("已连接到 \(ack.hostName)（\(ack.selectedCodec.displayName)\(tlsNote)）", isError: false)
                self.clipboardMonitor.start()
                self.gamepadMonitor.start()
                if self.settings.audioEnabled {
                    self.applyAudioSetting()
                }
                self.refreshWindowList()
                self.refreshAppList()
                self.refreshMonitors()
                self.onStateChanged?()
            } catch {
                conn.disconnect()
                if self.connection === conn {
                    self.connection = nil
                }
                self.connectionState = .disconnected
                ClientLogger.shared.error("Connect failed: \(error.localizedDescription)")
                self.setBanner("连接失败：\(error.localizedDescription)", isError: true)
                self.onStateChanged?()
                if case ConnectionError.certificateMismatch(let mismatchKey, let expected, let actual) = error {
                    self.cancelReconnect()
                    self.promptCertificateMismatch(hostKey: mismatchKey, expected: expected, actual: actual)
                } else if isAutoReconnect && self.shouldAutoReconnect(for: error) {
                    self.scheduleAutoReconnect(lastError: error)
                }
            }
        }
    }

    func disconnect() {
        manualDisconnectRequested = true
        cancelReconnect()
        pendingRestoreTargets.removeAll()
        if connection != nil {
            connection?.disconnect(error: nil)
        } else {
            setBanner("已断开连接", isError: false)
            onStateChanged?()
        }
    }

    func cancelReconnect() {
        reconnectTask?.cancel()
        reconnectTask = nil
        reconnectAttempt = 0
    }

    func resetTrustedFingerprintForCurrentHost() {
        let host = settings.host.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !host.isEmpty else { return }
        let key = CertificateFingerprint.hostKey(host: host, port: settings.port)
        var s = settings
        if s.trustedFingerprints.removeValue(forKey: key) != nil {
            settings = s
            setBanner("已清除 \(key) 的已信任 TLS 指纹，下次连接将重新校验并记住", isError: false)
        } else {
            setBanner("\(key) 尚未保存过 TLS 证书指纹", isError: false)
        }
        onStateChanged?()
    }

    private func shouldAutoReconnect(for error: Error?) -> Bool {
        guard settings.autoReconnect, !manualDisconnectRequested, let error else { return false }
        if let ce = error as? ConnectionError {
            switch ce {
            case .handshakeRejected, .certificateMismatch, .localNetworkProhibited, .invalidEndpoint:
                return false
            default:
                return true
            }
        }
        return true
    }

    private func scheduleAutoReconnect(lastError: Error) {
        guard settings.autoReconnect, !manualDisconnectRequested else { return }
        reconnectTask?.cancel()
        reconnectAttempt += 1
        let delaySecs = Self.reconnectDelaySeconds(forAttempt: reconnectAttempt)
        let attempt = reconnectAttempt
        setBanner(
            "连接异常（\(lastError.localizedDescription)），\(delaySecs) 秒后自动重连（第 \(attempt) 次）…",
            isError: true
        )
        onStateChanged?()

        reconnectTask = Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: UInt64(delaySecs) * 1_000_000_000)
            guard let self, !Task.isCancelled, self.connectionState == .disconnected else { return }
            self.connect(isAutoReconnect: true)
        }
    }

    nonisolated static func reconnectDelaySeconds(forAttempt attempt: Int) -> Int {
        switch max(1, attempt) {
        case 1: return 1
        case 2: return 2
        case 3: return 4
        case 4: return 8
        default: return 15
        }
    }

    private func snapshotWindowsForRestore() -> [RestoreWindowTarget] {
        var targets: [RestoreWindowTarget] = []
        if subscribedRootIds.contains(SyntheticWindow.primaryDesktop) {
            targets.append(RestoreWindowTarget(isDesktop: true, processName: "", title: ""))
        }
        for id in subscribedRootIds where id != SyntheticWindow.primaryDesktop {
            if let wc = controllers[id], wc.info.kind != .popup {
                targets.append(RestoreWindowTarget(
                    isDesktop: false,
                    processName: wc.info.processName,
                    title: wc.info.title
                ))
            }
        }
        return targets
    }

    private func restoreWindowsIfPending(from windows: [WindowInfo]) {
        guard !pendingRestoreTargets.isEmpty else { return }
        let targets = pendingRestoreTargets
        pendingRestoreTargets.removeAll()
        var restoredIds = Set<UInt64>()
        for target in targets {
            if target.isDesktop {
                if !isDesktopOpen {
                    toggleDesktopView()
                }
                continue
            }
            let exact = windows.first {
                !restoredIds.contains($0.windowId)
                    && $0.processName.caseInsensitiveCompare(target.processName) == .orderedSame
                    && $0.title == target.title
            }
            let byProcess = exact ?? windows.first {
                !restoredIds.contains($0.windowId)
                    && !target.processName.isEmpty
                    && $0.processName.caseInsensitiveCompare(target.processName) == .orderedSame
            }
            if let match = byProcess {
                restoredIds.insert(match.windowId)
                subscribeWindow(match)
            }
        }
    }

    private func promptCertificateMismatch(hostKey: String, expected: String, actual: String) {
        let alert = NSAlert()
        alert.alertStyle = .critical
        alert.messageText = "Windows 主机 TLS 证书指纹发生变化"
        alert.informativeText = """
        目标主机：\(hostKey)

        上次信任指纹：
        \(expected)

        当前收到指纹：
        \(actual)

        如果您刚在 Windows 端重装了系统或重新生成了证书，可点击「信任新证书并重连」；否则请取消连接以防中间人攻击。
        """
        alert.addButton(withTitle: "取消")
        alert.addButton(withTitle: "信任新证书并重连")
        let response = alert.runModal()
        if response == .alertSecondButtonReturn {
            var s = settings
            s.trustedFingerprints[hostKey] = actual
            settings = s
            connect()
        }
    }

    private func handleDisconnected(error: Error?) {
        let shouldReconnect = shouldAutoReconnect(for: error)
        if shouldReconnect && pendingRestoreTargets.isEmpty {
            pendingRestoreTargets = snapshotWindowsForRestore()
        }

        clipboardMonitor.stop()
        gamepadMonitor.stop()
        audioPlayer.stop()
        connection = nil
        connectionState = .disconnected
        rttMs = nil
        availableWindows.removeAll()
        availableApps.removeAll()
        availableMonitors.removeAll()
        subscribedRootIds.removeAll()
        activeTransferNames.removeAll()

        for (_, controller) in controllers {
            controller.forceCloseLocally()
        }
        controllers.removeAll()

        if let error {
            ClientLogger.shared.error("Disconnected: \(error.localizedDescription)")
            if shouldReconnect {
                scheduleAutoReconnect(lastError: error)
                return
            }
            setBanner("连接已断开：\(error.localizedDescription)", isError: true)
        } else {
            ClientLogger.shared.info("Disconnected cleanly")
            setBanner("已断开连接", isError: false)
        }
        onStateChanged?()
    }

    // MARK: - Window Actions

    func refreshWindowList() {
        guard case .connected = connectionState else { return }
        sendControl(.listWindowsReq(ListWindowsRequest(includeIcons: true)))
    }

    func refreshAppList() {
        guard case .connected = connectionState else { return }
        sendControl(.listAppsReq(ListAppsRequest(includeIcons: true)))
    }

    func refreshMonitors() {
        guard case .connected = connectionState else { return }
        sendControl(.listMonitorsReq(ListMonitorsRequest()))
    }

    func subscribeMonitor(_ monitor: MonitorInfo) {
        let info = WindowInfo(
            windowId: monitor.monitorId,
            processName: monitor.virtualAdapter ? "virtual-display" : "monitor",
            title: monitor.friendlyName.isEmpty ? monitor.deviceName : monitor.friendlyName,
            kind: .normal,
            x: monitor.x,
            y: monitor.y,
            width: monitor.width,
            height: monitor.height
        )
        subscribeWindow(info)
    }

    func enableVirtualDisplay() {
        guard case .connected = connectionState else { return }
        setBanner("正在请求把已安装的虚拟显示器设为 2560×1440。没有虚拟适配器时会失败，本程序不安装驱动。", isError: false)
        sendControl(.setVirtualDisplay(SetVirtualDisplay(enable: true, width: 2560, height: 1440, hz: 60)))
        Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: 700_000_000)
            self?.refreshMonitors()
        }
        onStateChanged?()
    }

    func toggleDesktopView() {
        guard case .connected = connectionState else { return }
        if isDesktopOpen {
            unsubscribeWindow(SyntheticWindow.primaryDesktop)
        } else {
            ClientLogger.shared.info("Subscribing primary desktop")
            subscribedRootIds.insert(SyntheticWindow.primaryDesktop)
            sendControl(.subscribeWindow(SubscribeWindow(
                windowId: SyntheticWindow.primaryDesktop,
                targetFps: settings.targetFps,
                maxBitrateKbps: 0,
                followPopups: false,
                captureMode: .monitorCrop
            )))
            onStateChanged?()
        }
    }

    func isPinned(_ appId: String) -> Bool {
        settings.pinnedAppIds.contains(appId)
    }

    func togglePin(_ appId: String) {
        var s = settings
        if let idx = s.pinnedAppIds.firstIndex(of: appId) {
            s.pinnedAppIds.remove(at: idx)
        } else {
            s.pinnedAppIds.append(appId)
        }
        settings = s
        onStateChanged?()
    }

    func captureMode(forApp app: AppEntry) -> CaptureMode {
        if let saved = settings.appCaptureModes[app.appId] {
            return saved
        }
        let lower = app.name.lowercased()
        if lower.contains("mumu") || lower.contains("雷电") || lower.contains("bluestacks") || lower.contains("steam") {
            return .monitorCrop
        }
        return .window
    }

    func setCaptureMode(_ mode: CaptureMode, forApp app: AppEntry) {
        var s = settings
        s.appCaptureModes[app.appId] = mode
        settings = s
        onStateChanged?()
    }

    func launchApp(_ app: AppEntry) {
        guard case .connected = connectionState else { return }
        let mode = captureMode(forApp: app)
        setBanner("正在启动「\(app.name)」，等待新窗口自动弹出…", isError: false)
        sendControl(.launchApp(LaunchApp(
            appId: app.appId,
            autoSubscribe: true,
            targetFps: settings.targetFps,
            maxBitrateKbps: 0,
            followPopups: settings.followPopups,
            captureMode: mode
        )))
        onStateChanged?()
    }

    func captureMode(for info: WindowInfo) -> CaptureMode {
        let key = info.processName.lowercased()
        if let saved = settings.processCaptureModes[key] {
            return saved
        }
        if key.contains("mumu") || key.contains("nemu") || key.contains("dnplayer") || key.contains("bluestacks") {
            return .monitorCrop
        }
        return .window
    }

    func setCaptureMode(_ mode: CaptureMode, for info: WindowInfo) {
        let key = info.processName.lowercased()
        if !key.isEmpty {
            var s = settings
            s.processCaptureModes[key] = mode
            settings = s
        }
        if subscribedRootIds.contains(info.windowId) {
            subscribeWindow(info)
        } else {
            onStateChanged?()
        }
    }

    func subscribeWindow(_ info: WindowInfo) {
        guard case .connected = connectionState else { return }
        subscribedRootIds.insert(info.windowId)
        let mode = captureMode(for: info)
        ClientLogger.shared.info("Subscribing window 0x\(String(info.windowId, radix: 16)) (\(info.processName)) mode=\(mode)")
        sendControl(.subscribeWindow(SubscribeWindow(
            windowId: info.windowId,
            targetFps: settings.targetFps,
            maxBitrateKbps: 0,
            followPopups: settings.followPopups,
            captureMode: mode
        )))
        onStateChanged?()
    }

    func unsubscribeWindow(_ windowId: UInt64) {
        subscribedRootIds.remove(windowId)
        sendControl(.unsubscribeWindow(UnsubscribeWindow(windowId: windowId)))
        closeWindowLocally(windowId)
        onStateChanged?()
    }

    func focusOrSubscribe(_ info: WindowInfo) {
        if let existing = controllers[info.windowId] {
            existing.present(owner: nil)
        } else {
            subscribeWindow(info)
        }
    }

    func requestKeyFrameForActiveWindow() {
        guard let keyWindow = NSApp.keyWindow,
              let wc = keyWindow.windowController as? RemoteWindowController else { return }
        sendControl(.requestKeyFrame(RequestKeyFrame(windowId: wc.windowId)))
    }

    func detachActiveWindow() {
        guard let keyWindow = NSApp.keyWindow,
              let wc = keyWindow.windowController as? RemoteWindowController else { return }
        unsubscribeWindow(wc.windowId)
    }

    func togglePointerLockForActiveWindow() {
        guard let keyWindow = NSApp.keyWindow,
              let wc = keyWindow.windowController as? RemoteWindowController else { return }
        wc.togglePointerLock()
    }

    func unlockAllPointers() {
        for (_, wc) in controllers {
            wc.unlockPointerIfNeeded()
        }
    }

    func sendControl(_ payload: ControlMessage.Payload) {
        connection?.send(payload)
    }

    // MARK: - Drag & Drop File Transfer (Mac -> Windows)

    func sendDroppedFiles(_ urls: [URL], toWindowId windowId: UInt64) {
        guard case .connected = connectionState else { return }
        let maxBytes: UInt64 = 512 * 1024 * 1024
        let chunkSize = 64 * 1024

        Task { @MainActor [weak self] in
            guard let self else { return }
            for url in urls {
                guard case .connected = self.connectionState else { break }
                do {
                    let attrs = try FileManager.default.attributesOfItem(atPath: url.path)
                    let fileSize = (attrs[.size] as? NSNumber)?.uint64Value ?? 0
                    guard fileSize <= maxBytes else {
                        self.setBanner("文件「\(url.lastPathComponent)」超过 512 MiB 上限，已跳过", isError: true)
                        self.onStateChanged?()
                        continue
                    }
                    let transferId = self.nextTransferId
                    self.nextTransferId &+= 1
                    let name = url.lastPathComponent
                    self.activeTransferNames[transferId] = name
                    self.setBanner("正在传输「\(name)」到 Windows…", isError: false)
                    self.onStateChanged?()

                    self.sendControl(.fileTransferStart(FileTransferStart(
                        transferId: transferId,
                        targetWindowId: windowId,
                        fileName: name,
                        totalBytes: fileSize
                    )))

                    let handle = try FileHandle(forReadingFrom: url)
                    defer { try? handle.close() }
                    var offset: UInt64 = 0
                    var chunksSinceYield = 0
                    if fileSize == 0 {
                        self.sendControl(.fileTransferChunk(FileTransferChunk(
                            transferId: transferId,
                            offset: 0,
                            data: Data(),
                            isLast: true
                        )))
                    } else {
                        while offset < fileSize {
                            guard case .connected = self.connectionState else { break }
                            let data = handle.readData(ofLength: chunkSize)
                            if data.isEmpty { break }
                            let nextOffset = offset + UInt64(data.count)
                            let isLast = nextOffset >= fileSize
                            self.sendControl(.fileTransferChunk(FileTransferChunk(
                                transferId: transferId,
                                offset: offset,
                                data: data,
                                isLast: isLast
                            )))
                            offset = nextOffset
                            chunksSinceYield += 1
                            if chunksSinceYield >= 8 {
                                chunksSinceYield = 0
                                await Task.yield()
                            }
                        }
                    }
                } catch {
                    self.setBanner("读取拖放文件失败：\(error.localizedDescription)", isError: true)
                    self.onStateChanged?()
                }
            }
        }
    }

    // MARK: - Inbound Control Dispatch

    private func handleControlMessage(_ msg: ControlMessage) {
        switch msg.payload {
        case .listWindowsRes(let res):
            availableWindows = res.windows
            restoreWindowsIfPending(from: res.windows)
            onStateChanged?()

        case .listAppsRes(let res):
            availableApps = res.apps
            onStateChanged?()

        case .listMonitorsRes(let res):
            availableMonitors = res.monitors
            onStateChanged?()

        case .launchAppRes(let res):
            if res.started {
                let name = availableApps.first(where: { $0.appId == res.appId })?.name ?? "应用"
                setBanner("已向 Windows 发送启动「\(name)」，正在等待主窗口出现…", isError: false)
                Task { @MainActor [weak self] in
                    try? await Task.sleep(nanoseconds: 2_500_000_000)
                    self?.refreshWindowList()
                }
            } else {
                setBanner("启动失败：\(res.message)", isError: true)
            }
            onStateChanged?()

        case .windowUpdated(let update):
            handleWindowUpdated(update)

        case .windowClosed(let closed):
            ClientLogger.shared.info("WindowClosed 0x\(String(closed.windowId, radix: 16))")
            subscribedRootIds.remove(closed.windowId)
            closeWindowLocally(closed.windowId)
            onStateChanged?()

        case .cursorShape(let shape):
            controllers[shape.windowId]?.applyCursor(shape)

        case .clipboardSync(let sync):
            clipboardMonitor.applyRemote(sync)

        case .fileTransferRes(let res):
            let name = activeTransferNames.removeValue(forKey: res.transferId) ?? "文件"
            if res.success {
                setBanner("文件「\(name)」已保存到 Windows：\(res.savedPath)", isError: false)
            } else {
                setBanner("文件「\(name)」传输失败：\(res.message)", isError: true)
            }
            onStateChanged?()

        case .hostError(let err):
            handleHostError(err)

        default:
            break
        }
    }

    private func handleWindowUpdated(_ update: WindowUpdated) {
        guard let info = update.window else { return }
        let ownerController = info.ownerWindowId != 0 ? controllers[info.ownerWindowId] : nil

        if let existing = controllers[info.windowId] {
            existing.applyUpdate(update, owner: ownerController)
            return
        }

        let wc = RemoteWindowController(
            info: info,
            owner: ownerController,
            syncWindowResize: { [weak self] in self?.settings.syncWindowResize ?? false }
        )
        let procName = info.processName
        wc.cmdMapsToCtrl = { [weak self] in self?.settings.cmdMapsToCtrl ?? true }
        wc.mouseSensitivity = { [weak self] in self?.mouseSensitivity(forProcess: procName) ?? 1.0 }
        wc.suppressInactiveHover = { [weak self] in self?.settings.suppressInactiveHover ?? false }
        wc.showPerformanceHud = { [weak self] in self?.settings.showPerformanceHud ?? false }
        wc.currentRttMs = { [weak self] in self?.rttMs }
        wc.onSendControl = { [weak self] payload in self?.sendControl(payload) }
        wc.onDetachRequested = { [weak self] id in self?.unsubscribeWindow(id) }
        wc.onOwnerMovedOrResized = { [weak self] ownerId in self?.repositionOwnedPopups(for: ownerId) }
        wc.onRequestQuit = { NSApp.terminate(nil) }
        wc.onOpenManager = { [weak self] in self?.onOpenManager?() }
        wc.onToggleHud = { [weak self] in self?.togglePerformanceHud() }
        wc.onFilesDropped = { [weak self] winId, urls in self?.sendDroppedFiles(urls, toWindowId: winId) }

        controllers[info.windowId] = wc
        ClientLogger.shared.info("Opened window 0x\(String(info.windowId, radix: 16)) kind=\(info.kind) \(info.width)x\(info.height) title=\"\(info.title)\"")
        if info.kind == .normal {
            subscribedRootIds.insert(info.windowId)
        }
        wc.present(owner: ownerController)
        wc.updateHudVisibility()
        if info.kind == .normal && isAutoGameLockEnabled(forProcess: info.processName) {
            wc.setPointerLock(true)
        }
        onStateChanged?()
    }

    private func repositionOwnedPopups(for ownerId: UInt64) {
        guard let owner = controllers[ownerId] else { return }
        for (_, child) in controllers where child.info.ownerWindowId == ownerId && child.info.kind == .popup {
            child.repositionRelative(to: owner)
        }
    }

    private func closeWindowLocally(_ windowId: UInt64) {
        let ownedIds = controllers.values
            .filter { $0.info.ownerWindowId == windowId && $0.info.kind == .popup }
            .map(\.windowId)
        for childId in ownedIds {
            controllers.removeValue(forKey: childId)?.forceCloseLocally()
        }
        controllers.removeValue(forKey: windowId)?.forceCloseLocally()
    }

    private func handleHostError(_ err: HostError) {
        ClientLogger.shared.error("HostError window=0x\(String(err.windowId, radix: 16)) code=\(err.code) msg=\(err.message)")
        let prefix: String
        switch err.code {
        case .displayDisconnected:
            prefix = "[显示器断开] "
        case .capturePausedMinimized:
            prefix = "[窗口已最小化] "
        case .accessDeniedElevated:
            prefix = "[管理员权限拦截] "
        case .windowNotFound:
            prefix = "[窗口不可用] "
            subscribedRootIds.remove(err.windowId)
            closeWindowLocally(err.windowId)
        case .captureFailed:
            prefix = "[采集失败] "
        case .encoderError:
            prefix = "[编码错误] "
        case .unspecified:
            prefix = err.message.hasPrefix("已将") ? "" : "[提示] "
        }
        setBanner(prefix + err.message, isError: err.code != .unspecified || !err.message.hasPrefix("已将"))
        onStateChanged?()
    }

    private func setBanner(_ message: String, isError: Bool) {
        lastBannerMessage = message
        lastBannerIsError = isError
    }
}
