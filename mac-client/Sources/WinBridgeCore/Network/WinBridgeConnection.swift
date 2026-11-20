import Foundation
import Network

enum ConnectionError: Error, LocalizedError {
    case invalidEndpoint(String, UInt16)
    case connectionClosed
    case handshakeRejected(String)
    case invalidSessionId
    case mediaBindRejected(UInt8)
    case controlFrameTooLarge(Int)
    case unexpectedControlMessage(String)
    case connectTimeout

    var errorDescription: String? {
        switch self {
        case .invalidEndpoint(let h, let p): return "Invalid endpoint \(h):\(p)"
        case .connectionClosed: return "Connection closed by remote host"
        case .handshakeRejected(let r): return "Host rejected connection: \(r)"
        case .invalidSessionId: return "Host returned an invalid session_id"
        case .mediaBindRejected(let code): return "Media channel bind rejected (status \(code))"
        case .controlFrameTooLarge(let n): return "Control frame exceeds 16 MiB (\(n) bytes)"
        case .unexpectedControlMessage(let m): return "Unexpected control message: \(m)"
        case .connectTimeout: return "Timed out connecting to Windows host"
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

private final class BufferedTCPStream {
    let connection: NWConnection
    private let queue: DispatchQueue
    private var buffer = ByteStreamBuffer()

    init(host: String, port: UInt16, label: String) throws {
        guard let nwPort = NWEndpoint.Port(rawValue: port) else {
            throw ConnectionError.invalidEndpoint(host, port)
        }
        let tcpOptions = NWProtocolTCP.Options()
        tcpOptions.noDelay = true
        tcpOptions.enableKeepalive = true
        tcpOptions.keepaliveIdle = 10
        let params = NWParameters(tls: nil, tcp: tcpOptions)
        self.queue = DispatchQueue(label: "winbridge.tcp.\(label)")
        self.connection = NWConnection(host: NWEndpoint.Host(host), port: nwPort, using: params)
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
                finish(.failure(ConnectionError.connectTimeout))
            }
            timer.resume()

            connection.stateUpdateHandler = { state in
                switch state {
                case .ready:
                    timer.cancel()
                    finish(.success(()))
                case .failed(let error):
                    timer.cancel()
                    finish(.failure(error))
                case .cancelled:
                    timer.cancel()
                    finish(.failure(ConnectionError.connectionClosed))
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
    var port: UInt16
    var authToken: String
    var clientName: String
    var displayScale: Float
    var supportedCodecs: [VideoCodec]
}

final class WinBridgeConnection {
    var onControlMessage: ((ControlMessage) -> Void)?
    var onMediaFrame: ((MediaFrame) -> Void)?
    var onDisconnected: ((Error?) -> Void)?
    var onRttUpdated: ((Double) -> Void)?

    private var controlStream: BufferedTCPStream?
    private var mediaStream: BufferedTCPStream?
    private var controlTask: Task<Void, Never>?
    private var mediaTask: Task<Void, Never>?
    private var pingTask: Task<Void, Never>?

    private let seqLock = NSLock()
    private var nextSeq: UInt64 = 1
    private var isDisconnected = false
    private let startTime = DispatchTime.now().uptimeNanoseconds

    private(set) var hostName: String = ""
    private(set) var selectedCodec: VideoCodec = .unspecified
    private(set) var defaultFps: UInt32 = 60

    func currentMicros() -> UInt64 {
        (DispatchTime.now().uptimeNanoseconds &- startTime) / 1_000
    }

    func connect(config: ConnectionConfig) async throws -> HelloAck {
        let control = try BufferedTCPStream(host: config.host, port: config.port, label: "control")
        try await control.connect()
        self.controlStream = control

        // 1. Control preface + Hello
        try await control.send(WireConstants.controlPreface)
        let hello = Hello(
            protocolVersion: 1,
            clientName: config.clientName,
            authToken: config.authToken,
            displayScale: config.displayScale,
            supportedCodecs: config.supportedCodecs
        )
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

        // 3. Connect Media TCP + bind session_id
        let media = try BufferedTCPStream(host: config.host, port: config.port, label: "media")
        do {
            try await media.connect()
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

        self.mediaStream = media
        self.hostName = ack.hostName
        self.selectedCodec = ack.selectedCodec
        self.defaultFps = ack.defaultFps > 0 ? ack.defaultFps : 60

        startLoops(control: control, media: media)
        return ack
    }

    func send(_ payload: ControlMessage.Payload) {
        guard let control = controlStream else { return }
        let framed = makeControlFrame(payload: payload)
        control.sendFireAndForget(framed)
    }

    func disconnect(error: Error? = nil) {
        seqLock.lock()
        if isDisconnected {
            seqLock.unlock()
            return
        }
        isDisconnected = true
        seqLock.unlock()

        pingTask?.cancel()
        controlTask?.cancel()
        mediaTask?.cancel()
        pingTask = nil
        controlTask = nil
        mediaTask = nil

        controlStream?.close()
        mediaStream?.close()
        controlStream = nil
        mediaStream = nil

        let callback = onDisconnected
        onDisconnected = nil
        callback?(error)
    }

    private func startLoops(control: BufferedTCPStream, media: BufferedTCPStream) {
        // Control read loop
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

        // Media read loop
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

        // Keepalive Ping every 5s (win-host idle timeout is 15s)
        pingTask = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 5_000_000_000)
                guard let self, !Task.isCancelled else { break }
                self.send(.ping(Ping(echoTimestampUs: self.currentMicros())))
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

    private func sendControlAsync(stream: BufferedTCPStream, payload: ControlMessage.Payload) async throws {
        try await stream.send(makeControlFrame(payload: payload))
    }

    private func readControlFrame(from stream: BufferedTCPStream) async throws -> ControlMessage {
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
