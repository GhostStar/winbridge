import Foundation
import Network
import Testing
@testable import WinBridgeCore

private final class LockedCount: @unchecked Sendable {
    private let lock = NSLock()
    private var count = 0
    func increment() { lock.lock(); count += 1; lock.unlock() }
    var value: Int { lock.lock(); defer { lock.unlock() }; return count }
}

@Suite("Connection handshake deadlines and QUIC interoperability")
struct HandshakeTests {
    @Test("Deadline closes stalled operations and any resource registered after expiry")
    func timeoutClosesResources() async throws {
        let count = LockedCount()
        let deadline = HandshakeDeadline(transport: "QUIC", seconds: 0.03)
        defer { deadline.stopTimer() }
        try deadline.enter("hello-ack")
        deadline.register { count.increment() }
        try await Task.sleep(nanoseconds: 100_000_000)
        deadline.register { count.increment() }
        #expect(count.value == 2)
        do {
            try deadline.finish()
            Issue.record("Expired handshake must not become an established session")
        } catch ConnectionError.handshakeTimeout(let transport, let stage) {
            #expect(transport == "QUIC")
            #expect(stage == "hello-ack")
        }
        deadline.cancel()
        #expect(count.value == 2)
    }

    @Test("A successful handshake disarms the watchdog without closing live channels")
    func successKeepsChannelsAlive() async throws {
        let count = LockedCount()
        let deadline = HandshakeDeadline(transport: "TCP", seconds: 0.03)
        deadline.register { count.increment() }
        try deadline.finish()
        try await Task.sleep(nanoseconds: 100_000_000)
        deadline.cancel()
        #expect(count.value == 0)
    }

    @Test("Timeout cannot disguise certificate and authentication failures")
    func securityErrorsPreserved() throws {
        let deadline = HandshakeDeadline(transport: "QUIC", seconds: 0)
        defer { deadline.stopTimer() }
        for original: ConnectionError in [.certificateMissing, .certificateMismatch(hostKey: "test", expected: "A", actual: "B"), .handshakeRejected("denied")] {
            let result = deadline.failure(original)
            #expect(!WinBridgeConnection.shouldFallbackToTCP(transport: .auto, error: result))
            #expect(!WinBridgeConnection.shouldFailoverToFallbackHost(primaryHost: "a", fallbackHost: "b", error: result))
        }
    }

    @Test("UDP failure cooldown is endpoint-scoped, expires and clears on success")
    func retryBackoff() {
        var time: TimeInterval = 100
        let backoff = QuicRetryBackoff(now: { time })
        #expect(!backoff.shouldSkip("host:1|cert"))
        backoff.failed("host:1|cert")
        #expect(backoff.shouldSkip("host:1|cert"))
        #expect(!backoff.shouldSkip("host:2|cert"))
        #expect(!backoff.shouldSkip("host:1|changed-cert"))
        time += 121
        #expect(!backoff.shouldSkip("host:1|cert"))
        backoff.failed("host:1|cert")
        backoff.succeeded("host:1|cert")
        #expect(!backoff.shouldSkip("host:1|cert"))
    }

    @Test("TCP peer accepting transport but never replying to Hello is bounded")
    func stalledHelloTimesOut() async throws {
        let peer = try SilentTCPPeer()
        let port = try await peer.start()
        defer { peer.stop() }
        let connection = WinBridgeConnection()
        defer { connection.disconnect() }
        let start = Date()
        do {
            _ = try await connection.connect(config: config(port: port, transport: .tcp, tls: false))
            Issue.record("Silent peer must not connect")
        } catch ConnectionError.handshakeTimeout(let transport, let stage) {
            #expect(transport == "TCP")
            #expect(stage == "hello-ack")
        }
        #expect(Date().timeIntervalSince(start) < 10)
    }

    @Test("Task cancellation interrupts a pending Hello read promptly")
    func cancellationClosesPendingRead() async throws {
        let peer = try SilentTCPPeer()
        let port = try await peer.start()
        defer { peer.stop() }
        let connection = WinBridgeConnection()
        defer { connection.disconnect() }
        let task = Task { try await connection.connect(config: config(port: port, transport: .tcp, tls: false)) }
        try await Task.sleep(nanoseconds: 100_000_000)
        let start = Date()
        task.cancel()
        do { _ = try await task.value; Issue.record("Cancelled connect must fail") } catch {}
        #expect(Date().timeIntervalSince(start) < 1)
    }

    @Test("Network.framework Control and Bulk bind to a real Quinn peer", .enabled(if: ProcessInfo.processInfo.environment["WINBRIDGE_QUIC_TEST_PORT"] != nil))
    func quinnInteroperability() async throws {
        let port = try #require(UInt16(ProcessInfo.processInfo.environment["WINBRIDGE_QUIC_TEST_PORT"] ?? ""))
        let connection = WinBridgeConnection()
        defer { connection.disconnect() }
        let frames = LockedCount()
        connection.onMediaFrame = { frame in
            if [11, 12].contains(frame.header.windowId) && frame.payload == Data([7]) { frames.increment() }
        }
        let start = Date()
        let outcome = try await connection.connect(config: config(port: port, transport: .quic, tls: true))
        #expect(outcome.transport == .quic)
        #expect(!outcome.fellBackToTcp)
        #expect(outcome.ack.hostName == "Fixture")
        #expect(Date().timeIntervalSince(start) < 2.5)
        for _ in 0..<100 {
            if frames.value == 2 { break }
            try await Task.sleep(nanoseconds: 10_000_000)
        }
        #expect(frames.value == 2)
    }

    private func config(port: UInt16, transport: TransportPreference, tls: Bool) -> ConnectionConfig {
        ConnectionConfig(host: "127.0.0.1", port: port, authToken: "synthetic-fixture-only", useTls: tls,
                         transport: transport, clientName: "HandshakeTest", displayScale: 1, supportedCodecs: [.jpeg])
    }
}

/// Accepts and consumes bytes but deliberately never sends HelloAck. Loopback only.
private final class SilentTCPPeer: @unchecked Sendable {
    private let listener: NWListener
    private let queue = DispatchQueue(label: "test.silent-peer")
    private var streams: [NWConnection] = [] // confined to queue
    private var startupFinished = false // confined to queue
    init() throws {
        let parameters = NWParameters.tcp
        parameters.requiredLocalEndpoint = .hostPort(host: "127.0.0.1", port: .any)
        listener = try NWListener(using: parameters)
    }
    func start() async throws -> UInt16 {
        try await withCheckedThrowingContinuation { continuation in
            listener.stateUpdateHandler = { state in
                guard !self.startupFinished else { return }
                switch state {
                case .ready:
                    self.startupFinished = true
                    continuation.resume(returning: self.listener.port!.rawValue)
                case .failed(let error):
                    self.startupFinished = true
                    continuation.resume(throwing: error)
                default: break
                }
            }
            listener.newConnectionHandler = { connection in
                self.streams.append(connection)
                connection.start(queue: self.queue)
                connection.receive(minimumIncompleteLength: 1, maximumLength: 4096) { _, _, _, _ in }
            }
            listener.start(queue: queue)
        }
    }
    func stop() {
        queue.sync {
            listener.stateUpdateHandler = nil
            listener.newConnectionHandler = nil
            listener.cancel()
            streams.forEach { $0.cancel() }
            streams.removeAll()
        }
    }
}
