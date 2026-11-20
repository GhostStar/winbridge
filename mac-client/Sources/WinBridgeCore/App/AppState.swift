import AppKit
import Foundation

enum ConnectionState: Equatable {
    case disconnected
    case connecting
    case connected(hostName: String, codec: VideoCodec)
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
            if oldValue.audioEnabled != settings.audioEnabled {
                applyAudioSetting()
            }
        }
    }

    private(set) var connectionState: ConnectionState = .disconnected
    private(set) var rttMs: Double?
    private(set) var availableWindows: [WindowInfo] = []
    private(set) var subscribedRootIds: Set<UInt64> = []
    private(set) var lastBannerMessage: String = ""
    private(set) var lastBannerIsError: Bool = false

    private var connection: WinBridgeConnection?
    private var controllers: [UInt64: RemoteWindowController] = [:]
    private let clipboardMonitor = ClipboardMonitor()
    private let audioPlayer = AudioPlayer()

    var onStateChanged: (() -> Void)?
    var onOpenManager: (() -> Void)?

    init() {
        self.settings = SettingsStore.load()
        clipboardMonitor.isEnabled = settings.syncClipboard
        audioPlayer.volume = settings.audioVolume
        clipboardMonitor.onLocalChange = { [weak self] sync in
            self?.sendControl(.clipboardSync(sync))
        }
    }

    func toggleAudioEnabled() {
        var s = settings
        s.audioEnabled.toggle()
        settings = s
        onStateChanged?()
    }

    private func applyAudioSetting() {
        guard case .connected = connectionState else {
            audioPlayer.setEnabled(false)
            return
        }
        audioPlayer.setEnabled(settings.audioEnabled)
        sendControl(.setAudioConfig(SetAudioConfig(enabled: settings.audioEnabled)))
    }

    // MARK: - Connection Lifecycle

    func connect() {
        guard connectionState == .disconnected else { return }
        let host = settings.host.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !host.isEmpty else {
            setBanner("请输入 Windows 主机内网 IP", isError: true)
            return
        }

        connectionState = .connecting
        setBanner("正在连接 \(host):\(settings.port)…", isError: false)
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
            if frame.header.codec == .audioPcmS16Le {
                // Feed CoreAudio directly from the Media TCP thread — zero main-thread jitter.
                player.enqueue(frame: frame)
                return
            }
            // Dispatch video frame to its per-window decoder queue.
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

        let displayScale = Float(NSScreen.main?.backingScaleFactor ?? 2.0)
        let config = ConnectionConfig(
            host: host,
            port: settings.port,
            authToken: settings.authToken,
            clientName: Host.current().localizedName ?? "macOS",
            displayScale: displayScale,
            supportedCodecs: settings.codecPreference.offeredCodecs
        )

        Task {
            do {
                let ack = try await conn.connect(config: config)
                self.connectionState = .connected(hostName: ack.hostName, codec: ack.selectedCodec)
                self.rttMs = nil
                self.setBanner("已连接到 \(ack.hostName)（\(ack.selectedCodec.displayName)）", isError: false)
                self.clipboardMonitor.start()
                if self.settings.audioEnabled {
                    self.applyAudioSetting()
                }
                self.refreshWindowList()
                self.onStateChanged?()
            } catch {
                conn.disconnect()
                if self.connection === conn {
                    self.connection = nil
                }
                self.connectionState = .disconnected
                self.setBanner("连接失败：\(error.localizedDescription)", isError: true)
                self.onStateChanged?()
            }
        }
    }

    func disconnect() {
        connection?.disconnect(error: nil)
    }

    private func handleDisconnected(error: Error?) {
        clipboardMonitor.stop()
        audioPlayer.stop()
        connection = nil
        connectionState = .disconnected
        rttMs = nil
        availableWindows.removeAll()
        subscribedRootIds.removeAll()

        for (_, controller) in controllers {
            controller.forceCloseLocally()
        }
        controllers.removeAll()

        if let error {
            setBanner("连接已断开：\(error.localizedDescription)", isError: true)
        } else {
            setBanner("已断开连接", isError: false)
        }
        onStateChanged?()
    }

    // MARK: - Window Actions

    func refreshWindowList() {
        guard case .connected = connectionState else { return }
        sendControl(.listWindowsReq(ListWindowsRequest(includeIcons: true)))
    }

    func captureMode(for info: WindowInfo) -> CaptureMode {
        let key = info.processName.lowercased()
        if let saved = settings.processCaptureModes[key] {
            return saved
        }
        // Smart default: cross-process Android emulators (MuMu, LDPlayer, BlueStacks) need Monitor Crop.
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
        // If already subscribed, hot-switch live without closing the macOS window.
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

    func sendControl(_ payload: ControlMessage.Payload) {
        connection?.send(payload)
    }

    // MARK: - Inbound Control Dispatch

    private func handleControlMessage(_ msg: ControlMessage) {
        switch msg.payload {
        case .listWindowsRes(let res):
            availableWindows = res.windows
            onStateChanged?()

        case .windowUpdated(let update):
            handleWindowUpdated(update)

        case .windowClosed(let closed):
            subscribedRootIds.remove(closed.windowId)
            closeWindowLocally(closed.windowId)
            onStateChanged?()

        case .cursorShape(let shape):
            controllers[shape.windowId]?.applyCursor(shape)

        case .clipboardSync(let sync):
            clipboardMonitor.applyRemote(sync)

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
        wc.cmdMapsToCtrl = { [weak self] in self?.settings.cmdMapsToCtrl ?? true }
        wc.onSendControl = { [weak self] payload in self?.sendControl(payload) }
        wc.onDetachRequested = { [weak self] id in self?.unsubscribeWindow(id) }
        wc.onOwnerMovedOrResized = { [weak self] ownerId in self?.repositionOwnedPopups(for: ownerId) }
        wc.onRequestQuit = { NSApp.terminate(nil) }
        wc.onOpenManager = { [weak self] in self?.onOpenManager?() }

        controllers[info.windowId] = wc
        wc.present(owner: ownerController)
        onStateChanged?()
    }

    private func repositionOwnedPopups(for ownerId: UInt64) {
        guard let owner = controllers[ownerId] else { return }
        for (_, child) in controllers where child.info.ownerWindowId == ownerId && child.info.kind == .popup {
            child.repositionRelative(to: owner)
        }
    }

    private func closeWindowLocally(_ windowId: UInt64) {
        // Also close any popups owned by this window.
        let ownedIds = controllers.values
            .filter { $0.info.ownerWindowId == windowId && $0.info.kind == .popup }
            .map(\.windowId)
        for childId in ownedIds {
            controllers.removeValue(forKey: childId)?.forceCloseLocally()
        }
        controllers.removeValue(forKey: windowId)?.forceCloseLocally()
    }

    private func handleHostError(_ err: HostError) {
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

    private func setBanner(_ message: String, isError: Bool) {
        lastBannerMessage = message
        lastBannerIsError = isError
    }
}
