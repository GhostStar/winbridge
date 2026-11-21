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

private struct IncomingPull {
    let finalURL: URL
    let partialURL: URL
    let handle: FileHandle
    let expected: UInt64
    var received: UInt64
    var lastProgressToastAt: Date
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
            if oldValue.audioEnabled != settings.audioEnabled
                || oldValue.audioQuality != settings.audioQuality
                || oldValue.audioProcessOnly != settings.audioProcessOnly {
                applyAudioSetting()
            }
            if oldValue.videoProfile != settings.videoProfile || oldValue.adaptiveBitrate != settings.adaptiveBitrate {
                pushVideoConfig(windowId: 0)
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
    private var activeTransferTargetWindow: [UInt64: UInt64] = [:]
    private var incomingPulls: [UInt64: IncomingPull] = [:]
    var onHostFilesListed: ((ListHostFilesResponse) -> Void)?
    private var lastAdaptivePush = Date.distantPast

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

    func setVideoProfile(_ profile: VideoProfilePreference) {
        var s = settings
        s.videoProfile = profile
        settings = s
        let adapt = s.adaptiveBitrate ? "，自适应码率开" : "，自适应码率关"
        setBanner("画质已设为\(profile.title)\(adapt)", isError: false)
        onStateChanged?()
    }

    func setVideoProfileForActiveProcess(_ profile: VideoProfilePreference) {
        guard let name = activeRemoteProcessName(), !name.isEmpty else {
            setBanner("请先选中一个正在投射的窗口，再按进程记住画质", isError: true)
            onStateChanged?()
            return
        }
        let key = name.lowercased()
        var s = settings
        s.processVideoProfiles[key] = profile
        settings = s
        if let wc = NSApp.keyWindow?.windowController as? RemoteWindowController {
            pushVideoConfig(windowId: wc.windowId)
        }
        setBanner("已记住：\(name) 使用\(profile.title)画质", isError: false)
        onStateChanged?()
    }

    func toggleAdaptiveBitrate() {
        var s = settings
        s.adaptiveBitrate.toggle()
        settings = s
        setBanner(s.adaptiveBitrate ? "已开启自适应码率（队列积压或 RTT 升高时降码率）" : "已关闭自适应码率", isError: false)
        onStateChanged?()
    }

    func videoProfile(for info: WindowInfo) -> VideoProfilePreference {
        let key = info.processName.lowercased()
        if !key.isEmpty, let saved = settings.processVideoProfiles[key] {
            return saved
        }
        return settings.videoProfile
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
            bitrateKbps: settings.audioQuality.bitrateKbps,
            scope: settings.audioProcessOnly ? 1 : 0
        )))
    }

    func toggleAudioProcessOnly() {
        var s = settings
        s.audioProcessOnly.toggle()
        settings = s
        setBanner(
            s.audioProcessOnly
                ? "只采集已投射窗口的进程声音。若系统不支持进程回环，会自动退回整机声音。"
                : "已改回采集整机系统声音",
            isError: false
        )
        onStateChanged?()
    }

    func pushVideoConfig(windowId: UInt64, rttOnly: Bool = false) {
        guard case .connected = connectionState else { return }
        let profile: VideoProfile
        if rttOnly {
            profile = .unspecified
        } else if windowId != 0, let info = videoInfo(for: windowId) {
            profile = videoProfile(for: info).protoValue
        } else {
            profile = settings.videoProfile.protoValue
        }
        let rtt = UInt32(min(10_000, max(0, (rttMs ?? 0).rounded())))
        sendControl(.setVideoConfig(SetVideoConfig(
            windowId: windowId,
            profile: profile,
            targetFps: 0,
            maxBitrateKbps: 0,
            adaptive: settings.adaptiveBitrate,
            rttMs: rtt
        )))
    }

    private func pushAdaptiveRttIfNeeded() {
        guard settings.adaptiveBitrate, case .connected = connectionState else { return }
        let now = Date()
        guard now.timeIntervalSince(lastAdaptivePush) >= 2 else { return }
        lastAdaptivePush = now
        pushVideoConfig(windowId: 0, rttOnly: true)
    }

    private func videoInfo(for windowId: UInt64) -> WindowInfo? {
        if let info = controllers[windowId]?.info {
            return info
        }
        return availableWindows.first { $0.windowId == windowId }
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
                self?.pushAdaptiveRttIfNeeded()
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
                self.pushVideoConfig(windowId: 0)
                self.refreshWindowList()
                self.refreshAppList()
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
        subscribedRootIds.removeAll()
        activeTransferNames.removeAll()
        activeTransferTargetWindow.removeAll()
        for (_, pull) in incomingPulls {
            try? pull.handle.close()
            try? FileManager.default.removeItem(at: pull.partialURL)
        }
        incomingPulls.removeAll()

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

    nonisolated static func formatByteCount(_ bytes: UInt64) -> String {
        if bytes < 1024 {
            return "\(bytes) B"
        } else if bytes < 1024 * 1024 {
            return String(format: "%.1f KB", Double(bytes) / 1024.0)
        } else if bytes < 1024 * 1024 * 1024 {
            return String(format: "%.1f MB", Double(bytes) / (1024.0 * 1024.0))
        }
        return String(format: "%.2f GB", Double(bytes) / (1024.0 * 1024.0 * 1024.0))
    }

    private func showTransferToast(
        _ text: String,
        style: TransferToastStyle,
        windowId: UInt64? = nil,
        autoHideSeconds: TimeInterval? = nil
    ) {
        if let wid = windowId, let wc = controllers[wid] {
            if wc.info.kind == .popup, wc.info.ownerWindowId != 0, let owner = controllers[wc.info.ownerWindowId] {
                owner.showToast(text, style: style, autoHideSeconds: autoHideSeconds)
                return
            }
            wc.showToast(text, style: style, autoHideSeconds: autoHideSeconds)
            return
        }
        if let keyWC = NSApp.keyWindow?.windowController as? RemoteWindowController {
            keyWC.showToast(text, style: style, autoHideSeconds: autoHideSeconds)
            return
        }
        if let fallback = controllers.values.first(where: { $0.info.kind != .popup }) {
            fallback.showToast(text, style: style, autoHideSeconds: autoHideSeconds)
        }
    }

    func sendDroppedFiles(_ urls: [URL], toWindowId windowId: UInt64) {
        guard case .connected = connectionState, let conn = connection else { return }
        let maxBytes: UInt64 = 512 * 1024 * 1024
        let chunkSize = 128 * 1024

        Task { @MainActor [weak self] in
            guard let self else { return }
            let totalFiles = urls.count
            for (idx, rawURL) in urls.enumerated() {
                guard case .connected = self.connectionState else { break }
                let url = rawURL.standardizedFileURL.resolvingSymlinksInPath()
                let name = url.lastPathComponent
                let batchPrefix = totalFiles > 1 ? "[\(idx + 1)/\(totalFiles)] " : ""
                var startedTransferId: UInt64?
                do {
                    let values = try url.resourceValues(forKeys: [.fileSizeKey, .isDirectoryKey])
                    if values.isDirectory == true {
                        let msg = "\(batchPrefix)已跳过文件夹「\(name)」"
                        ClientLogger.shared.info("FileTransfer skip directory: \(url.path)")
                        self.setBanner(msg, isError: true)
                        self.showTransferToast(msg, style: .error, windowId: windowId, autoHideSeconds: 4.0)
                        self.onStateChanged?()
                        continue
                    }
                    let fileSize = UInt64(max(0, values.fileSize ?? 0))
                    guard fileSize <= maxBytes else {
                        let msg = "\(batchPrefix)文件「\(name)」超过 512 MiB 上限，已跳过"
                        ClientLogger.shared.error("FileTransfer rejected oversized file \(name) (\(fileSize) bytes)")
                        self.setBanner(msg, isError: true)
                        self.showTransferToast(msg, style: .error, windowId: windowId, autoHideSeconds: 5.0)
                        self.onStateChanged?()
                        continue
                    }
                    let transferId = self.nextTransferId
                    self.nextTransferId &+= 1
                    startedTransferId = transferId
                    self.activeTransferNames[transferId] = name
                    self.activeTransferTargetWindow[transferId] = windowId

                    let totalStr = Self.formatByteCount(fileSize)
                    let startMsg = "⬆ \(batchPrefix)正在发送「\(name)」(0% · 0 B / \(totalStr))…"
                    ClientLogger.shared.info(
                        "FileTransfer start id=\(transferId) name=\"\(name)\" bytes=\(fileSize) window=0x\(String(windowId, radix: 16))"
                    )
                    self.setBanner(startMsg, isError: false)
                    self.showTransferToast(startMsg, style: .progress, windowId: windowId, autoHideSeconds: nil)
                    self.onStateChanged?()

                    try await conn.sendAsync(.fileTransferStart(FileTransferStart(
                        transferId: transferId,
                        targetWindowId: windowId,
                        fileName: name,
                        totalBytes: fileSize
                    )))

                    let handle = try FileHandle(forReadingFrom: url)
                    defer { try? handle.close() }
                    var offset: UInt64 = 0
                    var lastProgressUI = Date.distantPast
                    if fileSize == 0 {
                        try await conn.sendAsync(.fileTransferChunk(FileTransferChunk(
                            transferId: transferId,
                            offset: 0,
                            data: Data(),
                            isLast: true
                        )))
                    } else {
                        while offset < fileSize {
                            guard case .connected = self.connectionState else { break }
                            guard let data = try handle.read(upToCount: chunkSize), !data.isEmpty else {
                                break
                            }
                            let nextOffset = offset + UInt64(data.count)
                            let isLast = nextOffset >= fileSize
                            try await conn.sendAsync(.fileTransferChunk(FileTransferChunk(
                                transferId: transferId,
                                offset: offset,
                                data: data,
                                isLast: isLast
                            )))
                            offset = nextOffset
                            let now = Date()
                            if isLast || now.timeIntervalSince(lastProgressUI) >= 0.12 {
                                lastProgressUI = now
                                let pct = min(100, Int((Double(offset) * 100.0) / Double(max(1, fileSize))))
                                let sentStr = Self.formatByteCount(offset)
                                let progressMsg = isLast
                                    ? "⬆ \(batchPrefix)「\(name)」已发送 100% (\(totalStr))，等待 Windows 保存…"
                                    : "⬆ \(batchPrefix)正在发送「\(name)」· \(pct)% (\(sentStr) / \(totalStr))"
                                self.setBanner(progressMsg, isError: false)
                                self.showTransferToast(progressMsg, style: .progress, windowId: windowId, autoHideSeconds: nil)
                                self.onStateChanged?()
                            }
                        }
                    }
                    ClientLogger.shared.info("FileTransfer sent all chunks id=\(transferId) bytes=\(offset)")
                } catch {
                    if let tid = startedTransferId {
                        self.activeTransferNames.removeValue(forKey: tid)
                        self.activeTransferTargetWindow.removeValue(forKey: tid)
                    }
                    let msg = "发送「\(name)」失败：\(error.localizedDescription)"
                    ClientLogger.shared.error("FileTransfer send error name=\"\(name)\": \(error.localizedDescription)")
                    self.setBanner(msg, isError: true)
                    self.showTransferToast("✕ \(msg)", style: .error, windowId: windowId, autoHideSeconds: 5.5)
                    self.onStateChanged?()
                }
            }
        }
    }

    func requestHostFileList(path: String) {
        guard case .connected = connectionState else {
            setBanner("请先连接 Windows 主机", isError: true)
            onStateChanged?()
            return
        }
        sendControl(.listHostFilesReq(ListHostFilesRequest(path: path)))
    }

    private static let maxIncomingPullBytes: UInt64 = 512 * 1024 * 1024

    nonisolated static func sanitizeIncomingFileName(_ raw: String) -> String {
        let last = raw.split(whereSeparator: { $0 == "/" || $0 == "\\" }).last.map(String.init) ?? ""
        let cleaned = last
            .unicodeScalars
            .filter { !CharacterSet.controlCharacters.contains($0) }
            .map { Character($0) }
        var name = String(cleaned).trimmingCharacters(in: .whitespacesAndNewlines)
        if name.isEmpty || name == "." || name == ".." {
            return "winbridge-file.bin"
        }
        if name.count > 180 {
            name = String(name.prefix(180))
        }
        return name
    }

    func pullHostFile(path: String) {
        guard case .connected = connectionState else {
            setBanner("请先连接 Windows 主机", isError: true)
            onStateChanged?()
            return
        }
        let requestId = nextTransferId
        nextTransferId &+= 1
        let label: String
        if path == "clipboard:" {
            label = "剪贴板文件"
        } else {
            let tail = path.split(whereSeparator: { $0 == "/" || $0 == "\\" }).last.map(String.init) ?? ""
            label = tail.isEmpty ? "文件" : tail
        }
        setBanner("正在从 Windows 取回「\(label)」…", isError: false)
        onStateChanged?()
        sendControl(.filePullReq(FilePullRequest(requestId: requestId, path: path)))
    }

    private func macDropDirectory() -> URL {
        let base = FileManager.default.urls(for: .downloadsDirectory, in: .userDomainMask).first
            ?? URL(fileURLWithPath: NSTemporaryDirectory())
        let dir = base.appendingPathComponent("WinBridge", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }

    private func beginIncomingPull(_ start: FileTransferStart) {
        let name = Self.sanitizeIncomingFileName(start.fileName)
        guard start.totalBytes <= Self.maxIncomingPullBytes else {
            setBanner("取回「\(name)」失败：文件超过 512 MiB 上限", isError: true)
            onStateChanged?()
            return
        }
        if let existing = incomingPulls.removeValue(forKey: start.transferId) {
            try? existing.handle.close()
            try? FileManager.default.removeItem(at: existing.partialURL)
        }
        let dir = macDropDirectory()
        var dest = dir.appendingPathComponent(name)
        if FileManager.default.fileExists(atPath: dest.path) {
            let stem = dest.deletingPathExtension().lastPathComponent
            let ext = dest.pathExtension
            for n in 1..<10_000 {
                let candidateName = ext.isEmpty ? "\(stem) (\(n))" : "\(stem) (\(n)).\(ext)"
                let candidate = dir.appendingPathComponent(candidateName)
                if !FileManager.default.fileExists(atPath: candidate.path) {
                    dest = candidate
                    break
                }
            }
        }
        let partial = dir.appendingPathComponent(".\(dest.lastPathComponent).\(start.transferId).winbridge.part")
        guard FileManager.default.createFile(atPath: partial.path, contents: nil),
              let handle = try? FileHandle(forWritingTo: partial) else {
            setBanner("无法写入 \(partial.path)", isError: true)
            onStateChanged?()
            return
        }
        incomingPulls[start.transferId] = IncomingPull(
            finalURL: dest,
            partialURL: partial,
            handle: handle,
            expected: start.totalBytes,
            received: 0,
            lastProgressToastAt: .distantPast
        )
        let totalStr = Self.formatByteCount(start.totalBytes)
        ClientLogger.shared.info("FilePull start id=\(start.transferId) name=\"\(dest.lastPathComponent)\" bytes=\(start.totalBytes)")
        let msg = "⬇ 正在从 Windows 取回「\(dest.lastPathComponent)」(0% · 0 B / \(totalStr))…"
        setBanner(msg, isError: false)
        showTransferToast(msg, style: .progress, autoHideSeconds: nil)
        onStateChanged?()
    }

    private func appendIncomingChunk(_ chunk: FileTransferChunk) {
        guard var pull = incomingPulls[chunk.transferId] else { return }
        if chunk.offset != pull.received {
            finishIncoming(chunk.transferId, success: false, message: "分块偏移不连续（预期 \(pull.received)，实际 \(chunk.offset)）")
            return
        }
        let nextTotal = pull.received + UInt64(chunk.data.count)
        if nextTotal > pull.expected || nextTotal > Self.maxIncomingPullBytes {
            finishIncoming(chunk.transferId, success: false, message: "收到超限分块数据")
            return
        }
        if !chunk.data.isEmpty {
            do {
                try pull.handle.write(contentsOf: chunk.data)
            } catch {
                finishIncoming(chunk.transferId, success: false, message: "写入磁盘失败：\(error.localizedDescription)")
                return
            }
            pull.received = nextTotal
        }
        let now = Date()
        if chunk.isLast || now.timeIntervalSince(pull.lastProgressToastAt) >= 0.15 {
            pull.lastProgressToastAt = now
            let pct = pull.expected == 0 ? 100 : min(100, Int((Double(pull.received) * 100.0) / Double(pull.expected)))
            let msg = "⬇ 正在取回「\(pull.finalURL.lastPathComponent)」· \(pct)% (\(Self.formatByteCount(pull.received)) / \(Self.formatByteCount(pull.expected)))"
            setBanner(msg, isError: false)
            showTransferToast(msg, style: .progress, autoHideSeconds: nil)
        }
        incomingPulls[chunk.transferId] = pull
    }

    private func finishIncoming(_ id: UInt64, success: Bool, message: String) {
        guard let pull = incomingPulls.removeValue(forKey: id) else { return }
        try? pull.handle.close()
        let name = pull.finalURL.lastPathComponent
        let complete = success && pull.received == pull.expected
        if complete {
            do {
                if FileManager.default.fileExists(atPath: pull.finalURL.path) {
                    try FileManager.default.removeItem(at: pull.finalURL)
                }
                try FileManager.default.moveItem(at: pull.partialURL, to: pull.finalURL)
                let msg = "✓ 文件「\(name)」已取回到 Mac：\(pull.finalURL.path)"
                ClientLogger.shared.info("FilePull done id=\(id) path=\"\(pull.finalURL.path)\" bytes=\(pull.received)")
                setBanner(msg, isError: false)
                showTransferToast(msg, style: .success, autoHideSeconds: 5.0)
            } catch {
                try? FileManager.default.removeItem(at: pull.partialURL)
                let msg = "保存「\(name)」失败：\(error.localizedDescription)"
                ClientLogger.shared.error("FilePull save failed id=\(id): \(error.localizedDescription)")
                setBanner(msg, isError: true)
                showTransferToast("✕ \(msg)", style: .error, autoHideSeconds: 5.5)
            }
        } else {
            try? FileManager.default.removeItem(at: pull.partialURL)
            let why = message.isEmpty ? "传输不完整" : message
            let msg = "取回「\(name)」失败：\(why)"
            ClientLogger.shared.error("FilePull failed id=\(id) name=\"\(name)\": \(why)")
            setBanner(msg, isError: true)
            showTransferToast("✕ \(msg)", style: .error, autoHideSeconds: 5.5)
        }
        onStateChanged?()
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

        case .listHostFilesRes(let res):
            onHostFilesListed?(res)
            if !res.error.isEmpty {
                setBanner("列出 Windows 文件失败：\(res.error)", isError: true)
                onStateChanged?()
            }

        case .fileTransferStart(let start):
            if start.direction == FileTransferStart.directionToClient {
                beginIncomingPull(start)
            }

        case .fileTransferChunk(let chunk):
            if incomingPulls[chunk.transferId] != nil {
                appendIncomingChunk(chunk)
            }

        case .fileTransferRes(let res):
            if incomingPulls[res.transferId] != nil {
                finishIncoming(res.transferId, success: res.success, message: res.message)
                break
            }
            let targetWin = activeTransferTargetWindow.removeValue(forKey: res.transferId)
            if let name = activeTransferNames.removeValue(forKey: res.transferId) {
                if res.success {
                    let bannerMsg = "✓ 文件「\(name)」已保存到 Windows：\(res.savedPath)（已写入 Windows 剪贴板，按 ⌘V 可直接粘贴）"
                    let toastMsg = "✓ 已发送到 Windows：\(res.savedPath)\n已自动复制到 Windows 剪贴板，按 ⌘V 可直接粘贴"
                    ClientLogger.shared.info("FileTransfer done id=\(res.transferId) saved=\"\(res.savedPath)\"")
                    setBanner(bannerMsg, isError: false)
                    showTransferToast(toastMsg, style: .success, windowId: targetWin, autoHideSeconds: 5.5)
                } else {
                    let msg = "文件「\(name)」传输失败：\(res.message)"
                    ClientLogger.shared.error("FileTransfer failed id=\(res.transferId) name=\"\(name)\": \(res.message)")
                    setBanner(msg, isError: true)
                    showTransferToast("✕ \(msg)", style: .error, windowId: targetWin, autoHideSeconds: 5.5)
                }
                onStateChanged?()
            } else if !res.success {
                let msg = "文件传输失败：\(res.message)"
                ClientLogger.shared.error("FileTransfer error id=\(res.transferId): \(res.message)")
                setBanner(msg, isError: true)
                showTransferToast("✕ \(msg)", style: .error, windowId: targetWin, autoHideSeconds: 5.5)
                onStateChanged?()
            }

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
            let procKey = info.processName.lowercased()
            if !procKey.isEmpty, settings.processVideoProfiles[procKey] != nil {
                pushVideoConfig(windowId: info.windowId)
            }
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
            prefix = "[提示] "
        }
        setBanner(prefix + err.message, isError: true)
        onStateChanged?()
    }

    func setBanner(_ message: String, isError: Bool) {
        lastBannerMessage = message
        lastBannerIsError = isError
    }
}
