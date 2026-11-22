import Foundation
import Network
import Security

enum ConnectionError: Error, LocalizedError {
    case invalidEndpoint(String, UInt16)
    case connectionClosed
    case handshakeRejected(String)
    case invalidSessionId
    case mediaBindRejected(UInt8)
    case bulkBindRejected(UInt8)
    case quicStreamOpenFailed
    case controlFrameTooLarge(Int)
    case unexpectedControlMessage(String)
    case connectTimeout
    case localNetworkProhibited
    case certificateMissing
    case certificateMismatch(hostKey: String, expected: String, actual: String)

    var errorDescription: String? {
        switch self {
        case .invalidEndpoint(let h, let p): return "无效地址 \(h):\(p)"
        case .connectionClosed: return "远端主机已关闭连接"
        case .handshakeRejected(let r): return "主机拒绝连接：\(r)"
        case .invalidSessionId: return "主机返回了无效的 session_id"
        case .mediaBindRejected(let code): return "媒体通道绑定被拒绝（状态码 \(code)）"
        case .bulkBindRejected(let code): return "文件流绑定被拒绝（状态码 \(code)）"
        case .quicStreamOpenFailed: return "无法在 QUIC 连接上创建独立流"
        case .controlFrameTooLarge(let n): return "控制帧超过 16 MiB（\(n) 字节）"
        case .unexpectedControlMessage(let m): return "收到异常控制消息：\(m)"
        case .connectTimeout: return "连接 Windows 主机超时"
        case .localNetworkProhibited:
            return "macOS 拦截了本地网络访问权限：请在「系统设置 → 隐私与安全性 → 本地网络」中允许 WinBridge（重新打包建议使用固定自签证书）"
        case .certificateMissing:
            return "TLS 握手失败：Windows 主机未提供有效证书"
        case .certificateMismatch(let hostKey, _, let actual):
            return "主机 \(hostKey) 的 TLS 证书指纹已变化（新指纹 \(CertificateFingerprint.shortDisplay(actual))），连接已被拦截以防中间人攻击"
        }
    }
}

/// Contiguous byte buffer with guaranteed 0-based indexing across compactions.
/// Avoids Foundation.Data's `removeFirst(_:)` trap where `startIndex` shifts non-zero.
struct ByteStreamBuffer {
    private var storage: [UInt8] = []
    private var head: Int = 0

    var availableCount: Int { storage.count - head }

    mutating func append(_ chunk: Data) {
        if head == storage.count {
            storage.removeAll(keepingCapacity: true)
            head = 0
        }
        storage.append(contentsOf: chunk)
    }

    mutating func takeExact(_ count: Int) -> Data? {
        guard count >= 0, availableCount >= count else { return nil }
        if count == 0 { return Data() }
        let slice = Data(storage[head..<(head + count)])
        head += count
        if head == storage.count {
            storage.removeAll(keepingCapacity: true)
            head = 0
        } else if head >= 65_536 && head * 2 >= storage.count {
            storage.removeFirst(head)
            head = 0
        }
        return slice
    }
}

private final class TLSVerifyState: @unchecked Sendable {
    var verifiedFingerprint: String?
    var isFirstTrust: Bool = false
    var verificationError: Error?
}

private func configureTOFUVerify(
    securityOptions: sec_protocol_options_t,
    hostKey: String,
    expectedFingerprint: String?,
    verifyState: TLSVerifyState,
    queue: DispatchQueue
) {
    sec_protocol_options_set_verify_block(
        securityOptions,
        { _, secTrust, complete in
            let trust = sec_trust_copy_ref(secTrust).takeRetainedValue()
            guard let chain = SecTrustCopyCertificateChain(trust) as? [SecCertificate],
                  let leaf = chain.first else {
                verifyState.verificationError = ConnectionError.certificateMissing
                complete(false)
                return
            }
            let der = SecCertificateCopyData(leaf) as Data
            switch TOFUTrustEvaluator.evaluate(actualDER: der, pinnedFingerprint: expectedFingerprint) {
            case .trustFirstUse(let fp):
                verifyState.verifiedFingerprint = fp
                verifyState.isFirstTrust = true
                complete(true)
            case .trustedMatch(let fp):
                verifyState.verifiedFingerprint = fp
                verifyState.isFirstTrust = false
                complete(true)
            case .mismatch(let expected, let actual):
                verifyState.verificationError = ConnectionError.certificateMismatch(
                    hostKey: hostKey,
                    expected: expected,
                    actual: actual
                )
                complete(false)
            }
        },
        queue
    )
}

private final class BufferedNWStream: @unchecked Sendable {
    let connection: NWConnection
    private let queue: DispatchQueue
    private var buffer = ByteStreamBuffer()
    private let tlsState = TLSVerifyState()

    var verifiedFingerprint: String? { tlsState.verifiedFingerprint }
    var isFirstTrust: Bool { tlsState.isFirstTrust }

    init(
        host: String,
        port: UInt16,
        label: String,
        useTls: Bool,
        expectedFingerprint: String?
    ) throws {
        guard let nwPort = NWEndpoint.Port(rawValue: port) else {
            throw ConnectionError.invalidEndpoint(host, port)
        }
        let tcpOptions = NWProtocolTCP.Options()
        tcpOptions.noDelay = true
        tcpOptions.enableKeepalive = true
        tcpOptions.keepaliveIdle = 10
        let queue = DispatchQueue(label: "winbridge.tcp.\(label)")
        self.queue = queue

        let params: NWParameters
        if useTls {
            let tlsOptions = NWProtocolTLS.Options()
            sec_protocol_options_set_min_tls_protocol_version(
                tlsOptions.securityProtocolOptions,
                .TLSv12
            )
            let hostKey = CertificateFingerprint.hostKey(host: host, port: port)
            configureTOFUVerify(
                securityOptions: tlsOptions.securityProtocolOptions,
                hostKey: hostKey,
                expectedFingerprint: expectedFingerprint,
                verifyState: self.tlsState,
                queue: queue
            )
            params = NWParameters(tls: tlsOptions, tcp: tcpOptions)
        } else {
            params = NWParameters(tls: nil, tcp: tcpOptions)
        }
        self.connection = NWConnection(host: NWEndpoint.Host(host), port: nwPort, using: params)
    }

    init(connection: NWConnection, queue: DispatchQueue) {
        self.connection = connection
        self.queue = queue
    }

    func connect(timeoutSeconds: Double = 8.0) async throws {
        try await withCheckedThrowingContinuation { (cont: CheckedContinuation<Void, Error>) in
            var resumed = false
            let finish: (Result<Void, Error>) -> Void = { result in
                guard !resumed else { return }
                resumed = true
                self.connection.stateUpdateHandler = nil
                switch result {
                case .success: cont.resume()
                case .failure(let err):
                    self.connection.cancel()
                    cont.resume(throwing: err)
                }
            }

            let timer = DispatchSource.makeTimerSource(queue: queue)
            timer.schedule(deadline: .now() + timeoutSeconds)
            timer.setEventHandler {
                timer.cancel()
                if let tlsErr = self.tlsState.verificationError {
                    finish(.failure(tlsErr))
                } else {
                    finish(.failure(ConnectionError.connectTimeout))
                }
            }
            timer.resume()

            connection.stateUpdateHandler = { [weak self] state in
                guard let self else { return }
                switch state {
                case .ready:
                    timer.cancel()
                    finish(.success(()))
                case .waiting(let error):
                    if self.connection.currentPath?.unsatisfiedReason == .localNetworkDenied
                        || error.localizedDescription.localizedCaseInsensitiveContains("local network") {
                        timer.cancel()
                        finish(.failure(ConnectionError.localNetworkProhibited))
                    } else if let tlsErr = self.tlsState.verificationError {
                        timer.cancel()
                        finish(.failure(tlsErr))
                    }
                case .failed(let error):
                    timer.cancel()
                    if let tlsErr = self.tlsState.verificationError {
                        finish(.failure(tlsErr))
                    } else {
                        finish(.failure(error))
                    }
                case .cancelled:
                    timer.cancel()
                    if let tlsErr = self.tlsState.verificationError {
                        finish(.failure(tlsErr))
                    } else {
                        finish(.failure(ConnectionError.connectionClosed))
                    }
                default:
                    break
                }
            }
            connection.start(queue: queue)
        }
    }

    func send(_ data: Data) async throws {
        try await withCheckedThrowingContinuation { (cont: CheckedContinuation<Void, Error>) in
            connection.send(content: data, completion: .contentProcessed { error in
                if let error {
                    cont.resume(throwing: error)
                } else {
                    cont.resume()
                }
            })
        }
    }

    func sendFireAndForget(_ data: Data) {
        connection.send(content: data, completion: .contentProcessed { _ in })
    }

    func readExact(_ count: Int) async throws -> Data {
        if count == 0 { return Data() }
        while buffer.availableCount < count {
            let chunk = try await receiveChunk()
            buffer.append(chunk)
        }
        guard let slice = buffer.takeExact(count) else {
            throw ConnectionError.connectionClosed
        }
        return slice
    }

    private func receiveChunk() async throws -> Data {
        try await withCheckedThrowingContinuation { (cont: CheckedContinuation<Data, Error>) in
            connection.receive(minimumIncompleteLength: 1, maximumLength: 262_144) { data, _, isComplete, error in
                if let error {
                    cont.resume(throwing: error)
                } else if let data, !data.isEmpty {
                    cont.resume(returning: data)
                } else if isComplete {
                    cont.resume(throwing: ConnectionError.connectionClosed)
                } else {
                    cont.resume(throwing: ConnectionError.connectionClosed)
                }
            }
        }
    }

    func close() {
        connection.cancel()
    }
}

struct ConnectionConfig: Sendable {
    var host: String
    var fallbackHost: String = ""
    var port: UInt16
    var authToken: String
    var useTls: Bool = true
    var transport: TransportPreference = .auto
    var expectedFingerprint: String? = nil
    var fallbackExpectedFingerprint: String? = nil
    var clientName: String
    var displayScale: Float
    var supportedCodecs: [VideoCodec]
}

struct ConnectOutcome: Sendable {
    let ack: HelloAck
    let tlsFingerprint: String?
    let isFirstTrust: Bool
    let transport: ActiveTransport
    let fellBackToTcp: Bool
    let connectedHost: String
    let usedFallbackHost: Bool
}

final class WinBridgeConnection: @unchecked Sendable {
    var onControlMessage: ((ControlMessage) -> Void)?
    var onMediaFrame: ((MediaFrame) -> Void)?
    var onDisconnected: ((Error?) -> Void)?
    var onRttUpdated: ((Double) -> Void)?

    private var controlStream: BufferedNWStream?
    private var mediaStream: BufferedNWStream?
    private var bulkStream: BufferedNWStream?
    private var quicGroup: NWConnectionGroup?
    private var controlTask: Task<Void, Never>?
    private var mediaTask: Task<Void, Never>?
    private var bulkTask: Task<Void, Never>?
    private var pingTask: Task<Void, Never>?

    private let streamTasksLock = NSLock()
    private var quicMediaTasks: [UUID: Task<Void, Never>] = [:]
    private var quicMediaConnections: [UUID: NWConnection] = [:]

    private let seqLock = NSLock()
    private var nextSeq: UInt64 = 1
    private var isDisconnected = false
    private let startTime = DispatchTime.now().uptimeNanoseconds

    private(set) var hostName: String = ""
    private(set) var selectedCodec: VideoCodec = .unspecified
    private(set) var defaultFps: UInt32 = 60
    private(set) var activeTransport: ActiveTransport = .tcp

    var cancelledOrDisconnected: Bool {
        seqLock.lock()
        defer { seqLock.unlock() }
        return isDisconnected
    }

    private func ensureNotDisconnected() throws {
        if cancelledOrDisconnected || Task.isCancelled {
            throw ConnectionError.connectionClosed
        }
    }

    func currentMicros() -> UInt64 {
        (DispatchTime.now().uptimeNanoseconds &- startTime) / 1_000
    }

    static func shouldFallbackToTCP(transport: TransportPreference, error: Error, isCancelled: Bool = false) -> Bool {
        guard !isCancelled, transport == .auto else { return false }
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

    static func shouldFailoverToFallbackHost(
        primaryHost: String,
        fallbackHost: String,
        error: Error,
        isCancelled: Bool = false
    ) -> Bool {
        guard !isCancelled else { return false }
        let fb = fallbackHost.trimmingCharacters(in: .whitespacesAndNewlines)
        let primary = primaryHost.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !fb.isEmpty, fb.caseInsensitiveCompare(primary) != .orderedSame else {
            return false
        }
        if let ce = error as? ConnectionError {
            switch ce {
            case .handshakeRejected, .certificateMismatch, .localNetworkProhibited:
                return false
            default:
                return true
            }
        }
        return true
    }

    func connect(config: ConnectionConfig) async throws -> ConnectOutcome {
        try ensureNotDisconnected()
        let primary = config.host.trimmingCharacters(in: .whitespacesAndNewlines)
        let fallback = config.fallbackHost.trimmingCharacters(in: .whitespacesAndNewlines)
        let hasFallback = !fallback.isEmpty && fallback.caseInsensitiveCompare(primary) != .orderedSame

        do {
            return try await connectHost(
                host: primary,
                expectedFingerprint: config.expectedFingerprint,
                config: config,
                usedFallbackHost: false,
                fastTimeout: hasFallback
            )
        } catch {
            cleanupResourcesWithoutNotifying()
            let cancelled = cancelledOrDisconnected || Task.isCancelled
            if Self.shouldFailoverToFallbackHost(
                primaryHost: primary,
                fallbackHost: fallback,
                error: error,
                isCancelled: cancelled
            ) {
                ClientLogger.shared.info(
                    "Primary host \(primary) unreachable (\(error.localizedDescription)); failing over to fallback host \(fallback)"
                )
                return try await connectHost(
                    host: fallback,
                    expectedFingerprint: config.fallbackExpectedFingerprint ?? config.expectedFingerprint,
                    config: config,
                    usedFallbackHost: true,
                    fastTimeout: false
                )
            }
            throw error
        }
    }

    private func connectHost(
        host: String,
        expectedFingerprint: String?,
        config: ConnectionConfig,
        usedFallbackHost: Bool,
        fastTimeout: Bool
    ) async throws -> ConnectOutcome {
        try ensureNotDisconnected()
        var hostConfig = config
        hostConfig.host = host
        hostConfig.expectedFingerprint = expectedFingerprint
        let tcpTimeout = fastTimeout ? 2.5 : 8.0

        if hostConfig.useTls && hostConfig.transport != .tcp {
            do {
                let quicTimeout: Double
                if hostConfig.transport == .auto {
                    quicTimeout = fastTimeout ? 1.8 : 2.5
                } else {
                    quicTimeout = fastTimeout ? 3.0 : 8.0
                }
                return try await connectQUIC(
                    config: hostConfig,
                    timeoutSeconds: quicTimeout,
                    usedFallbackHost: usedFallbackHost
                )
            } catch {
                cleanupResourcesWithoutNotifying()
                let cancelled = cancelledOrDisconnected || Task.isCancelled
                if Self.shouldFallbackToTCP(
                    transport: hostConfig.transport,
                    error: error,
                    isCancelled: cancelled
                ) {
                    ClientLogger.shared.info(
                        "QUIC unavailable on \(host) (\(error.localizedDescription)); falling back to TCP+TLS"
                    )
                    return try await connectTCP(
                        config: hostConfig,
                        timeoutSeconds: tcpTimeout,
                        fellBackFromQuic: true,
                        usedFallbackHost: usedFallbackHost
                    )
                }
                throw error
            }
        }
        return try await connectTCP(
            config: hostConfig,
            timeoutSeconds: tcpTimeout,
            fellBackFromQuic: false,
            usedFallbackHost: usedFallbackHost
        )
    }

    private func connectQUIC(
        config: ConnectionConfig,
        timeoutSeconds: Double,
        usedFallbackHost: Bool
    ) async throws -> ConnectOutcome {
        guard let nwPort = NWEndpoint.Port(rawValue: config.port) else {
            throw ConnectionError.invalidEndpoint(config.host, config.port)
        }

        let quicQueue = DispatchQueue(label: "winbridge.quic")
        let quicOptions = NWProtocolQUIC.Options(alpn: [WireConstants.quicAlpn])
        quicOptions.direction = .bidirectional
        quicOptions.idleTimeout = 15_000
        quicOptions.initialMaxData = 32 * 1024 * 1024
        quicOptions.initialMaxStreamDataBidirectionalLocal = 8 * 1024 * 1024
        quicOptions.initialMaxStreamDataBidirectionalRemote = 8 * 1024 * 1024
        quicOptions.initialMaxStreamDataUnidirectional = 8 * 1024 * 1024
        quicOptions.initialMaxStreamsBidirectional = 128
        quicOptions.initialMaxStreamsUnidirectional = 128

        let verifyState = TLSVerifyState()
        let hostKey = CertificateFingerprint.hostKey(host: config.host, port: config.port)
        configureTOFUVerify(
            securityOptions: quicOptions.securityProtocolOptions,
            hostKey: hostKey,
            expectedFingerprint: config.expectedFingerprint,
            verifyState: verifyState,
            queue: quicQueue
        )

        let params = NWParameters(quic: quicOptions)
        let endpoint = NWEndpoint.hostPort(host: NWEndpoint.Host(config.host), port: nwPort)
        let group = NWConnectionGroup(with: NWMultiplexGroup(to: endpoint), using: params)
        self.quicGroup = group

        // Accept server-initiated unidirectional Audio / per-window Video streams immediately.
        group.newConnectionHandler = { [weak self] incoming in
            self?.handleIncomingQuicMediaStream(incoming)
        }

        try await startQuicGroup(group, queue: quicQueue, verifyState: verifyState, timeoutSeconds: timeoutSeconds)

        // 1. Open Stream 0 (Control bidirectional stream)
        let streamOptions = NWProtocolQUIC.Options(alpn: [WireConstants.quicAlpn])
        streamOptions.direction = .bidirectional
        guard let ctlConn = NWConnection(from: group, using: streamOptions) else {
            throw ConnectionError.quicStreamOpenFailed
        }
        let control = BufferedNWStream(connection: ctlConn, queue: DispatchQueue(label: "winbridge.quic.control"))
        try await control.connect(timeoutSeconds: 4.0)
        self.controlStream = control

        try await control.send(WireConstants.controlPreface)
        let hello = makeHello(config: config)
        try await sendControlAsync(stream: control, payload: .hello(hello))

        // 2. Read HelloAck
        let reply = try await readControlFrame(from: control)
        guard case .helloAck(let ack) = reply.payload else {
            throw ConnectionError.unexpectedControlMessage("Expected HelloAck")
        }
        guard ack.accepted else {
            throw ConnectionError.handshakeRejected(ack.rejectReason.isEmpty ? "Unknown reason" : ack.rejectReason)
        }
        guard ack.sessionId.count == 16 else {
            throw ConnectionError.invalidSessionId
        }

        // 3. Open Stream 1 (Isolated low-priority Bulk File Transfer bidirectional stream)
        guard let bulkConn = NWConnection(from: group, using: streamOptions) else {
            throw ConnectionError.quicStreamOpenFailed
        }
        let bulk = BufferedNWStream(connection: bulkConn, queue: DispatchQueue(label: "winbridge.quic.bulk"))
        try await bulk.connect(timeoutSeconds: 4.0)
        var bulkBind = Data(capacity: 24)
        bulkBind.append(WireConstants.bulkPreface)
        bulkBind.append(ack.sessionId)
        try await bulk.send(bulkBind)
        let bindAck = try await bulk.readExact(1)
        guard bindAck[0] == 0 else {
            throw ConnectionError.bulkBindRejected(bindAck[0])
        }
        self.bulkStream = bulk

        // Monitor QUIC tunnel failure after connect
        if cancelledOrDisconnected || Task.isCancelled {
            cleanupResourcesWithoutNotifying()
            throw ConnectionError.connectionClosed
        }
        group.stateUpdateHandler = { [weak self] state in
            guard let self else { return }
            switch state {
            case .failed(let err):
                self.disconnect(error: err)
            case .cancelled:
                self.disconnect(error: ConnectionError.connectionClosed)
            default:
                break
            }
        }

        self.hostName = ack.hostName
        self.selectedCodec = ack.selectedCodec
        self.defaultFps = ack.defaultFps > 0 ? ack.defaultFps : 60
        self.activeTransport = .quic

        startControlAndPingLoops(control: control)
        startBulkLoop(bulk: bulk)

        return ConnectOutcome(
            ack: ack,
            tlsFingerprint: verifyState.verifiedFingerprint,
            isFirstTrust: verifyState.isFirstTrust,
            transport: .quic,
            fellBackToTcp: false,
            connectedHost: config.host,
            usedFallbackHost: usedFallbackHost
        )
    }

    private func startQuicGroup(
        _ group: NWConnectionGroup,
        queue: DispatchQueue,
        verifyState: TLSVerifyState,
        timeoutSeconds: Double
    ) async throws {
        try await withCheckedThrowingContinuation { (cont: CheckedContinuation<Void, Error>) in
            var resumed = false
            let finish: (Result<Void, Error>) -> Void = { result in
                guard !resumed else { return }
                resumed = true
                group.stateUpdateHandler = nil
                switch result {
                case .success:
                    cont.resume()
                case .failure(let err):
                    group.cancel()
                    cont.resume(throwing: err)
                }
            }

            let timer = DispatchSource.makeTimerSource(queue: queue)
            timer.schedule(deadline: .now() + timeoutSeconds)
            timer.setEventHandler {
                timer.cancel()
                if let tlsErr = verifyState.verificationError {
                    finish(.failure(tlsErr))
                } else {
                    finish(.failure(ConnectionError.connectTimeout))
                }
            }
            timer.resume()

            group.stateUpdateHandler = { state in
                switch state {
                case .ready:
                    timer.cancel()
                    finish(.success(()))
                case .waiting(let error):
                    if error.localizedDescription.localizedCaseInsensitiveContains("local network") {
                        timer.cancel()
                        finish(.failure(ConnectionError.localNetworkProhibited))
                    } else if let tlsErr = verifyState.verificationError {
                        timer.cancel()
                        finish(.failure(tlsErr))
                    }
                case .failed(let error):
                    timer.cancel()
                    if let tlsErr = verifyState.verificationError {
                        finish(.failure(tlsErr))
                    } else {
                        finish(.failure(error))
                    }
                case .cancelled:
                    timer.cancel()
                    if let tlsErr = verifyState.verificationError {
                        finish(.failure(tlsErr))
                    } else {
                        finish(.failure(ConnectionError.connectionClosed))
                    }
                default:
                    break
                }
            }
            group.start(queue: queue)
        }
    }

    private func handleIncomingQuicMediaStream(_ incoming: NWConnection) {
        let token = UUID()
        let streamQueue = DispatchQueue(label: "winbridge.quic.media.\(token.uuidString.prefix(8))")
        let stream = BufferedNWStream(connection: incoming, queue: streamQueue)

        streamTasksLock.lock()
        quicMediaConnections[token] = incoming
        streamTasksLock.unlock()

        let task = Task { [weak self] in
            defer {
                stream.close()
                self?.removeQuicMediaStream(token: token)
            }
            do {
                try await stream.connect(timeoutSeconds: 5.0)
                let preface = try await stream.readExact(8)
                guard preface == WireConstants.quicAudioPreface || preface == WireConstants.quicVideoPreface else {
                    return
                }
                while !Task.isCancelled {
                    let headerBytes = try await stream.readExact(WireConstants.mediaHeaderSize)
                    let header = try MediaFrameHeader.decode(from: headerBytes)
                    let payload = try await stream.readExact(Int(header.payloadLen))
                    self?.onMediaFrame?(MediaFrame(header: header, payload: payload))
                    if header.flags.contains(.endOfStream) {
                        break
                    }
                }
            } catch {
                // Closing an individual per-window video stream is normal when a window closes;
                // session-wide drops are detected via Control stream / NWConnectionGroup state.
            }
        }

        streamTasksLock.lock()
        quicMediaTasks[token] = task
        streamTasksLock.unlock()
    }

    private func removeQuicMediaStream(token: UUID) {
        streamTasksLock.lock()
        quicMediaTasks.removeValue(forKey: token)
        quicMediaConnections.removeValue(forKey: token)
        streamTasksLock.unlock()
    }

    private func connectTCP(
        config: ConnectionConfig,
        timeoutSeconds: Double,
        fellBackFromQuic: Bool,
        usedFallbackHost: Bool
    ) async throws -> ConnectOutcome {
        let control = try BufferedNWStream(
            host: config.host,
            port: config.port,
            label: "control",
            useTls: config.useTls,
            expectedFingerprint: config.expectedFingerprint
        )
        try await control.connect(timeoutSeconds: timeoutSeconds)
        self.controlStream = control

        // 1. Control preface + Hello
        try await control.send(WireConstants.controlPreface)
        let hello = makeHello(config: config)
        try await sendControlAsync(stream: control, payload: .hello(hello))

        // 2. Read HelloAck
        let reply = try await readControlFrame(from: control)
        guard case .helloAck(let ack) = reply.payload else {
            control.close()
            throw ConnectionError.unexpectedControlMessage("Expected HelloAck")
        }
        guard ack.accepted else {
            control.close()
            throw ConnectionError.handshakeRejected(ack.rejectReason.isEmpty ? "Unknown reason" : ack.rejectReason)
        }
        guard ack.sessionId.count == 16 else {
            control.close()
            throw ConnectionError.invalidSessionId
        }

        // 3. Connect Media TCP + bind session_id (strictly pinned to the Control cert fingerprint)
        let media = try BufferedNWStream(
            host: config.host,
            port: config.port,
            label: "media",
            useTls: config.useTls,
            expectedFingerprint: control.verifiedFingerprint ?? config.expectedFingerprint
        )
        do {
            try await media.connect(timeoutSeconds: timeoutSeconds)
            var bindPacket = Data(capacity: 24)
            bindPacket.append(WireConstants.mediaPreface)
            bindPacket.append(ack.sessionId)
            try await media.send(bindPacket)
            let statusBytes = try await media.readExact(1)
            let status = statusBytes[0]
            guard status == 0 else {
                throw ConnectionError.mediaBindRejected(status)
            }
        } catch {
            media.close()
            control.close()
            throw error
        }

        if cancelledOrDisconnected || Task.isCancelled {
            cleanupResourcesWithoutNotifying()
            throw ConnectionError.connectionClosed
        }
        self.mediaStream = media
        self.hostName = ack.hostName
        self.selectedCodec = ack.selectedCodec
        self.defaultFps = ack.defaultFps > 0 ? ack.defaultFps : 60
        self.activeTransport = .tcp

        startControlAndPingLoops(control: control)
        startTcpMediaLoop(media: media)
        return ConnectOutcome(
            ack: ack,
            tlsFingerprint: control.verifiedFingerprint,
            isFirstTrust: control.isFirstTrust,
            transport: .tcp,
            fellBackToTcp: fellBackFromQuic,
            connectedHost: config.host,
            usedFallbackHost: usedFallbackHost
        )
    }

    private func makeHello(config: ConnectionConfig) -> Hello {
        Hello(
            protocolVersion: 1,
            clientName: config.clientName,
            authToken: config.authToken,
            displayScale: config.displayScale,
            supportedCodecs: config.supportedCodecs,
            appVersion: ProtocolCapabilities.currentAppVersion,
            capabilities: ProtocolCapabilities.clientSupported
        )
    }

    func send(_ payload: ControlMessage.Payload) {
        guard let control = controlStream else { return }
        let framed = makeControlFrame(payload: payload)
        control.sendFireAndForget(framed)
    }

    func sendAsync(_ payload: ControlMessage.Payload) async throws {
        guard let control = controlStream else {
            throw ConnectionError.connectionClosed
        }
        let framed = makeControlFrame(payload: payload)
        try await control.send(framed)
    }

    /// Sends bulk file transfer traffic on the isolated low-priority QUIC bulk stream when available,
    /// falling back to the Control stream on TCP sessions.
    func sendBulkAsync(_ payload: ControlMessage.Payload) async throws {
        if let bulk = bulkStream {
            let framed = makeControlFrame(payload: payload)
            try await bulk.send(framed)
            return
        }
        try await sendAsync(payload)
    }

    private func cleanupResourcesWithoutNotifying() {
        pingTask?.cancel()
        controlTask?.cancel()
        mediaTask?.cancel()
        bulkTask?.cancel()
        pingTask = nil
        controlTask = nil
        mediaTask = nil
        bulkTask = nil

        streamTasksLock.lock()
        let tasks = quicMediaTasks
        let conns = quicMediaConnections
        quicMediaTasks.removeAll()
        quicMediaConnections.removeAll()
        streamTasksLock.unlock()

        for (_, task) in tasks { task.cancel() }
        for (_, conn) in conns { conn.cancel() }

        controlStream?.close()
        mediaStream?.close()
        bulkStream?.close()
        quicGroup?.stateUpdateHandler = nil
        quicGroup?.newConnectionHandler = nil
        quicGroup?.cancel()

        controlStream = nil
        mediaStream = nil
        bulkStream = nil
        quicGroup = nil
    }

    func disconnect(error: Error? = nil) {
        seqLock.lock()
        if isDisconnected {
            seqLock.unlock()
            return
        }
        isDisconnected = true
        seqLock.unlock()

        cleanupResourcesWithoutNotifying()

        let callback = onDisconnected
        onControlMessage = nil
        onMediaFrame = nil
        onRttUpdated = nil
        onDisconnected = nil
        callback?(error)
    }

    private func startControlAndPingLoops(control: BufferedNWStream) {
        controlTask = Task { [weak self] in
            guard let self else { return }
            do {
                while !Task.isCancelled {
                    let msg = try await self.readControlFrame(from: control)
                    switch msg.payload {
                    case .ping(let p):
                        self.send(.pong(Pong(echoTimestampUs: p.echoTimestampUs)))
                    case .pong(let p):
                        let now = self.currentMicros()
                        if now >= p.echoTimestampUs {
                            let rttMs = Double(now - p.echoTimestampUs) / 1000.0
                            self.onRttUpdated?(rttMs)
                        }
                    default:
                        self.onControlMessage?(msg)
                    }
                }
            } catch {
                if !Task.isCancelled {
                    self.disconnect(error: error)
                }
            }
        }

        pingTask = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 5_000_000_000)
                guard let self, !Task.isCancelled else { break }
                self.send(.ping(Ping(echoTimestampUs: self.currentMicros())))
            }
        }
    }

    private func startBulkLoop(bulk: BufferedNWStream) {
        bulkTask = Task { [weak self] in
            guard let self else { return }
            do {
                while !Task.isCancelled {
                    let msg = try await self.readControlFrame(from: bulk)
                    self.onControlMessage?(msg)
                }
            } catch {
                // Bulk stream ending only terminates the bulk reader; Control stream monitors session liveness.
                ClientLogger.shared.info("QUIC bulk stream closed: \(error.localizedDescription)")
            }
        }
    }

    private func startTcpMediaLoop(media: BufferedNWStream) {
        mediaTask = Task { [weak self] in
            guard let self else { return }
            do {
                while !Task.isCancelled {
                    let headerBytes = try await media.readExact(WireConstants.mediaHeaderSize)
                    let header = try MediaFrameHeader.decode(from: headerBytes)
                    let payload = try await media.readExact(Int(header.payloadLen))
                    self.onMediaFrame?(MediaFrame(header: header, payload: payload))
                }
            } catch {
                if !Task.isCancelled {
                    self.disconnect(error: error)
                }
            }
        }
    }

    private func allocateSeq() -> UInt64 {
        seqLock.lock()
        defer { seqLock.unlock() }
        let s = nextSeq
        nextSeq &+= 1
        return s
    }

    private func makeControlFrame(payload: ControlMessage.Payload) -> Data {
        let msg = ControlMessage(seq: allocateSeq(), timestampUs: currentMicros(), payload: payload)
        let encoded = msg.encode()
        var frame = Data(capacity: 4 + encoded.count)
        var beLen = UInt32(encoded.count).bigEndian
        withUnsafeBytes(of: &beLen) { frame.append(contentsOf: $0) }
        frame.append(encoded)
        return frame
    }

    private func sendControlAsync(stream: BufferedNWStream, payload: ControlMessage.Payload) async throws {
        try await stream.send(makeControlFrame(payload: payload))
    }

    private func readControlFrame(from stream: BufferedNWStream) async throws -> ControlMessage {
        let lenBytes = try await stream.readExact(4)
        let len = (Int(lenBytes[0]) << 24)
            | (Int(lenBytes[1]) << 16)
            | (Int(lenBytes[2]) << 8)
            | Int(lenBytes[3])
        guard len > 0 && len <= WireConstants.maxControlPayload else {
            throw ConnectionError.controlFrameTooLarge(len)
        }
        let body = try await stream.readExact(len)
        return try ControlMessage.decode(from: body)
    }
}
