import Foundation
import os

/// Lightweight append-only file & unified logger writing to `~/Library/Logs/WinBridge/client.log`.
final class ClientLogger: @unchecked Sendable {
    static let shared = ClientLogger()

    private let queue = DispatchQueue(label: "com.winbridge.client.logger", qos: .utility)
    private let osLog = Logger(subsystem: "com.winbridge.client", category: "app")
    private let logURL: URL
    private let dateFormatter: ISO8601DateFormatter

    private init() {
        let base = FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Logs/WinBridge", isDirectory: true)
        try? FileManager.default.createDirectory(at: base, withIntermediateDirectories: true)
        self.logURL = base.appendingPathComponent("client.log")
        let fmt = ISO8601DateFormatter()
        fmt.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        self.dateFormatter = fmt
    }

    func info(_ message: String) {
        write(level: "INFO", message: message)
        osLog.info("\(message, privacy: .public)")
    }

    func error(_ message: String) {
        write(level: "ERROR", message: message)
        osLog.error("\(message, privacy: .public)")
    }

    private func write(level: String, message: String) {
        let line = "[\(dateFormatter.string(from: Date()))] [\(level)] \(message)\n"
        queue.async { [logURL] in
            guard let data = line.data(using: .utf8) else { return }
            if FileManager.default.fileExists(atPath: logURL.path) {
                if let fh = try? FileHandle(forWritingTo: logURL) {
                    defer { try? fh.close() }
                    _ = try? fh.seekToEnd()
                    try? fh.write(contentsOf: data)
                }
            } else {
                try? data.write(to: logURL)
            }
        }
    }
}
