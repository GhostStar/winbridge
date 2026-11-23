import Foundation

/// One wall-clock budget for transport + Hello + channel binding. Cancellation must
/// close sockets: cancelling a Swift Task alone does not resume NW continuations.
final class HandshakeDeadline: @unchecked Sendable {
    private let lock = NSLock()
    private let started = DispatchTime.now().uptimeNanoseconds
    private let expires: UInt64
    private let transport: String
    private var stage = "transport"
    private var terminalError: ConnectionError?
    private var completed = false
    private var cancellations: [() -> Void] = []
    private var timer: DispatchSourceTimer?

    init(transport: String, seconds: Double) {
        self.transport = transport
        expires = started + UInt64(max(0, seconds) * 1_000_000_000)
        let timer = DispatchSource.makeTimerSource(queue: .global(qos: .utility))
        self.timer = timer
        timer.schedule(deadline: DispatchTime(uptimeNanoseconds: expires))
        timer.setEventHandler { [weak self] in self?.expire() }
        timer.resume()
    }

    func register(_ cancel: @escaping () -> Void) {
        lock.lock()
        let stopped = terminalError != nil
        if !stopped && !completed { cancellations.append(cancel) }
        lock.unlock()
        if stopped { cancel() }
    }

    func enter(_ next: String) throws {
        expireIfDue()
        lock.lock()
        let error = terminalError
        if error == nil { stage = next }
        lock.unlock()
        if let error { throw error }
        ClientLogger.shared.info("\(transport) handshake stage=\(next) elapsed_ms=\(elapsedMilliseconds)")
    }

    func finish() throws {
        expireIfDue()
        lock.lock()
        let error = terminalError
        if error == nil { completed = true; cancellations.removeAll() }
        lock.unlock()
        stopTimer()
        if let error { throw error }
        ClientLogger.shared.info("\(transport) handshake complete elapsed_ms=\(elapsedMilliseconds)")
    }

    func cancel() { terminate(timeout: false) }

    func failure(_ original: Error) -> Error {
        // Never turn a certificate/auth failure into a retryable timeout.
        if let error = original as? ConnectionError {
            switch error {
            case .certificateMismatch, .certificateMissing, .handshakeRejected, .localNetworkProhibited:
                return error
            default: break
            }
        }
        expireIfDue()
        lock.lock()
        let error = terminalError
        let failedStage = stage
        lock.unlock()
        ClientLogger.shared.info("\(transport) handshake failed stage=\(failedStage) elapsed_ms=\(elapsedMilliseconds)")
        return error ?? original
    }

    func stopTimer() {
        lock.lock()
        let old = timer
        timer = nil
        lock.unlock()
        old?.cancel()
    }

    private var elapsedMilliseconds: UInt64 {
        (DispatchTime.now().uptimeNanoseconds - started) / 1_000_000
    }
    private func expireIfDue() {
        if DispatchTime.now().uptimeNanoseconds >= expires { expire() }
    }
    private func expire() { terminate(timeout: true) }
    private func terminate(timeout: Bool) {
        lock.lock()
        guard !completed, terminalError == nil else { lock.unlock(); return }
        terminalError = timeout ? .handshakeTimeout(transport: transport, stage: stage) : .connectionClosed
        let actions = cancellations
        cancellations.removeAll()
        lock.unlock()
        actions.forEach { $0() }
    }
    deinit { timer?.cancel() }
}

/// In-memory only: repeated reconnects to a failing UDP endpoint use TCP promptly.
/// Explicit QUIC bypasses this cache; a successful QUIC connection clears it.
final class QuicRetryBackoff: @unchecked Sendable {
    static let shared = QuicRetryBackoff()
    private let lock = NSLock()
    private var failures: [String: TimeInterval] = [:]
    private let now: () -> TimeInterval
    init(now: @escaping () -> TimeInterval = { ProcessInfo.processInfo.systemUptime }) { self.now = now }
    func shouldSkip(_ key: String) -> Bool {
        lock.lock(); defer { lock.unlock() }
        return (failures[key] ?? 0) > now()
    }
    func failed(_ key: String) {
        lock.lock(); defer { lock.unlock() }
        let time = now()
        failures = failures.filter { $0.value > time }
        if failures.count >= 64 { failures.removeAll() }
        failures[key] = time + 120
    }
    func succeeded(_ key: String) {
        lock.lock(); defer { lock.unlock() }
        failures.removeValue(forKey: key)
    }
}
