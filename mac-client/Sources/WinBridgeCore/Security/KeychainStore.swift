import CryptoKit
import Foundation
import Security

/// Native macOS Keychain storage for the WinBridge pre-shared authentication token.
/// Keeps credentials out of plaintext `UserDefaults` plist files.
enum KeychainStore {
    private static let service = "com.winbridge.client.auth"
    static let defaultAccount = "default"

    static func accountName(forProfileId profileId: String) -> String {
        let trimmed = profileId.trimmingCharacters(in: .whitespacesAndNewlines)
        if trimmed.isEmpty || trimmed == defaultAccount {
            return defaultAccount
        }
        return "profile.\(trimmed)"
    }

    static func loadToken(account: String = defaultAccount) -> String? {
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: service,
            kSecAttrAccount: account,
            kSecReturnData: true,
            kSecMatchLimit: kSecMatchLimitOne,
        ]
        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)
        guard status == errSecSuccess, let data = item as? Data else {
            return nil
        }
        return String(data: data, encoding: .utf8)
    }

    @discardableResult
    static func saveToken(_ token: String, account: String = defaultAccount) -> Bool {
        let trimmed = token.trimmingCharacters(in: .whitespacesAndNewlines)
        if trimmed.isEmpty {
            return deleteToken(account: account)
        }
        guard let data = trimmed.data(using: .utf8) else { return false }

        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: service,
            kSecAttrAccount: account,
        ]
        let attrs: [CFString: Any] = [
            kSecValueData: data,
            kSecAttrAccessible: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
        ]
        let updateStatus = SecItemUpdate(query as CFDictionary, attrs as CFDictionary)
        if updateStatus == errSecSuccess {
            return true
        }
        if updateStatus == errSecItemNotFound {
            var addQuery = query
            addQuery[kSecValueData] = data
            addQuery[kSecAttrAccessible] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
            return SecItemAdd(addQuery as CFDictionary, nil) == errSecSuccess
        }
        return false
    }

    @discardableResult
    static func deleteToken(account: String = defaultAccount) -> Bool {
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: service,
            kSecAttrAccount: account,
        ]
        let status = SecItemDelete(query as CFDictionary)
        return status == errSecSuccess || status == errSecItemNotFound
    }
}

/// SHA-256 certificate fingerprint formatting and Trust-On-First-Use (TOFU) verification logic.
enum CertificateFingerprint {
    static func sha256(der: Data) -> String {
        let digest = SHA256.hash(data: der)
        return digest.map { String(format: "%02X", $0) }.joined(separator: ":")
    }

    static func normalize(_ fingerprint: String) -> String {
        fingerprint
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .uppercased()
    }

    static func shortDisplay(_ fingerprint: String) -> String {
        let parts = normalize(fingerprint).split(separator: ":")
        if parts.count >= 8 {
            return parts.prefix(8).joined(separator: ":") + "…"
        }
        return normalize(fingerprint)
    }

    static func hostKey(host: String, port: UInt16) -> String {
        "\(host.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()):\(port)"
    }
}

enum TOFUDecision: Equatable, Sendable {
    case trustFirstUse(fingerprint: String)
    case trustedMatch(fingerprint: String)
    case mismatch(expected: String, actual: String)
}

enum TOFUTrustEvaluator {
    static func evaluate(actualDER: Data, pinnedFingerprint: String?) -> TOFUDecision {
        let actual = CertificateFingerprint.sha256(der: actualDER)
        guard let pinned = pinnedFingerprint?.trimmingCharacters(in: .whitespacesAndNewlines),
              !pinned.isEmpty else {
            return .trustFirstUse(fingerprint: actual)
        }
        let normalizedPinned = CertificateFingerprint.normalize(pinned)
        if normalizedPinned == actual {
            return .trustedMatch(fingerprint: actual)
        }
        return .mismatch(expected: normalizedPinned, actual: actual)
    }
}
