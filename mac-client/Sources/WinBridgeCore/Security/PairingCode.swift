import Foundation

struct ParsedPairingCode: Equatable, Sendable {
    let host: String
    let fallbackHost: String?
    let port: UInt16
    let token: String?
    let fingerprint: String?
    let macAddress: String?
    let name: String?
    let useTls: Bool
}

enum PairingCode {
    /// Parses a `winbridge://host:port?name=...&token=...&fp=...&mac=...&tls=1` URI.
    /// Also tolerates surrounding terminal log text by locating the first `winbridge://` token.
    static func parse(_ raw: String) -> ParsedPairingCode? {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }

        let uriString: String
        if let range = trimmed.range(of: "winbridge://", options: [.caseInsensitive]) {
            let tail = trimmed[range.lowerBound...]
            let tokenEnd = tail.firstIndex(where: { $0.isWhitespace || $0 == "\"" || $0 == "'" || $0 == ">" })
                ?? tail.endIndex
            uriString = String(tail[..<tokenEnd])
        } else {
            return nil
        }

        guard let components = URLComponents(string: uriString),
              components.scheme?.lowercased() == "winbridge",
              let rawHost = components.host?.trimmingCharacters(in: .whitespacesAndNewlines),
              !rawHost.isEmpty else {
            return nil
        }

        let port: UInt16
        if let p = components.port {
            guard p > 0 && p <= 65535 else { return nil }
            port = UInt16(p)
        } else {
            port = 9876
        }

        var fallbackHost: String?
        var token: String?
        var fingerprint: String?
        var macAddress: String?
        var name: String?
        var useTls = true

        for item in components.queryItems ?? [] {
            let key = item.name.lowercased()
            let val = (item.value ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
            guard !val.isEmpty else { continue }
            switch key {
            case "fallback", "backup":
                fallbackHost = val
            case "token":
                token = val
            case "fp", "fingerprint":
                fingerprint = CertificateFingerprint.normalize(val)
            case "mac":
                macAddress = WakeOnLan.normalizeMac(val)
            case "name":
                name = val
            case "tls":
                let lower = val.lowercased()
                useTls = !(lower == "0" || lower == "false" || lower == "no")
            default:
                break
            }
        }

        return ParsedPairingCode(
            host: rawHost,
            fallbackHost: fallbackHost,
            port: port,
            token: token,
            fingerprint: fingerprint,
            macAddress: macAddress,
            name: name,
            useTls: useTls
        )
    }
}
