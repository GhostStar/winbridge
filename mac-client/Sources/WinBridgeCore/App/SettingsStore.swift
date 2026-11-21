import Foundation

enum CodecPreference: String, CaseIterable {
    case auto = "auto"
    case h264 = "h264"
    case jpeg = "jpeg"
    case raw  = "raw"

    var title: String {
        switch self {
        case .auto: return "Auto (H.264 → JPEG)"
        case .h264: return "H.264 Hardware"
        case .jpeg: return "JPEG"
        case .raw:  return "RAW BGRA"
        }
    }

    var offeredCodecs: [VideoCodec] {
        switch self {
        case .auto: return [.h264AnnexB, .jpeg, .rawBgra]
        case .h264: return [.h264AnnexB]
        case .jpeg: return [.jpeg]
        case .raw:  return [.rawBgra]
        }
    }
}

struct ClientSettings {
    var host: String
    var port: UInt16
    var authToken: String
    var useTls: Bool
    var trustedFingerprints: [String: String]
    var autoReconnect: Bool
    var showPerformanceHud: Bool
    var suppressInactiveHover: Bool
    var codecPreference: CodecPreference
    var cmdMapsToCtrl: Bool
    var syncClipboard: Bool
    var followPopups: Bool
    var syncWindowResize: Bool
    var audioEnabled: Bool
    var audioVolume: Float
    var mouseSensitivity: Float
    var targetFps: UInt32
    var processCaptureModes: [String: CaptureMode]
    var processMouseSensitivity: [String: Float]
    var processAutoGameLock: [String: Bool]
    var appCaptureModes: [String: CaptureMode]
    var pinnedAppIds: [String]
}

enum SettingsStore {
    private static let defaults = UserDefaults(suiteName: "com.winbridge.client") ?? UserDefaults.standard
    private static let legacyTokenKey = "winbridge.token"

    static func load() -> ClientSettings {
        let host = defaults.string(forKey: "winbridge.host") ?? "192.168.1.100"
        let portInt = defaults.integer(forKey: "winbridge.port")
        let port = (portInt > 0 && portInt <= 65535) ? UInt16(portInt) : 9876

        // Prefer macOS Keychain for auth token; automatically migrate legacy plaintext UserDefaults token once.
        let token: String
        if let keychainToken = KeychainStore.loadToken() {
            token = keychainToken
            if defaults.object(forKey: legacyTokenKey) != nil {
                defaults.removeObject(forKey: legacyTokenKey)
            }
        } else if let legacy = defaults.string(forKey: legacyTokenKey),
                  !legacy.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            let trimmed = legacy.trimmingCharacters(in: .whitespacesAndNewlines)
            token = trimmed
            if KeychainStore.saveToken(trimmed) {
                defaults.removeObject(forKey: legacyTokenKey)
            }
        } else {
            token = ""
        }

        let useTls = defaults.object(forKey: "winbridge.useTls") as? Bool ?? true
        let trustedFingerprints = defaults.dictionary(forKey: "winbridge.trustedFingerprints") as? [String: String] ?? [:]
        let autoReconnect = defaults.object(forKey: "winbridge.autoReconnect") as? Bool ?? true
        let showPerformanceHud = defaults.object(forKey: "winbridge.showPerformanceHud") as? Bool ?? false
        let suppressInactiveHover = defaults.object(forKey: "winbridge.suppressInactiveHover") as? Bool ?? false

        let codecRaw = defaults.string(forKey: "winbridge.codec") ?? CodecPreference.auto.rawValue
        let codec = CodecPreference(rawValue: codecRaw) ?? .auto
        let cmdToCtrl = defaults.object(forKey: "winbridge.cmdToCtrl") as? Bool ?? true
        let clipboard = defaults.object(forKey: "winbridge.syncClipboard") as? Bool ?? true
        let popups = defaults.object(forKey: "winbridge.followPopups") as? Bool ?? true
        let syncResize = defaults.object(forKey: "winbridge.syncWindowResize") as? Bool ?? false
        let audioEnabled = defaults.object(forKey: "winbridge.audioEnabled") as? Bool ?? false
        let audioVolume = defaults.object(forKey: "winbridge.audioVolume") as? Float ?? 0.85
        let mouseSensitivity = defaults.object(forKey: "winbridge.mouseSensitivity") as? Float ?? 1.0
        let fpsInt = defaults.integer(forKey: "winbridge.fps")
        let fps = (fpsInt >= 1 && fpsInt <= 240) ? UInt32(fpsInt) : 60
        let rawModes = defaults.dictionary(forKey: "winbridge.processCaptureModes") as? [String: Int] ?? [:]
        var modes: [String: CaptureMode] = [:]
        for (proc, raw) in rawModes {
            if let mode = CaptureMode(rawValue: Int32(raw)), mode != .unspecified {
                modes[proc] = mode
            }
        }
        let rawSens = defaults.dictionary(forKey: "winbridge.processMouseSensitivity") as? [String: NSNumber] ?? [:]
        var procSens: [String: Float] = [:]
        for (proc, num) in rawSens {
            procSens[proc] = max(0.1, min(4.0, num.floatValue))
        }
        let procAutoLock = defaults.dictionary(forKey: "winbridge.processAutoGameLock") as? [String: Bool] ?? [:]
        let rawAppModes = defaults.dictionary(forKey: "winbridge.appCaptureModes") as? [String: Int] ?? [:]
        var appModes: [String: CaptureMode] = [:]
        for (appId, raw) in rawAppModes {
            if let mode = CaptureMode(rawValue: Int32(raw)), mode != .unspecified {
                appModes[appId] = mode
            }
        }
        let pinned = defaults.stringArray(forKey: "winbridge.pinnedAppIds") ?? []

        return ClientSettings(
            host: host,
            port: port,
            authToken: token,
            useTls: useTls,
            trustedFingerprints: trustedFingerprints,
            autoReconnect: autoReconnect,
            showPerformanceHud: showPerformanceHud,
            suppressInactiveHover: suppressInactiveHover,
            codecPreference: codec,
            cmdMapsToCtrl: cmdToCtrl,
            syncClipboard: clipboard,
            followPopups: popups,
            syncWindowResize: syncResize,
            audioEnabled: audioEnabled,
            audioVolume: max(0, min(1, audioVolume)),
            mouseSensitivity: max(0.1, min(4.0, mouseSensitivity)),
            targetFps: fps,
            processCaptureModes: modes,
            processMouseSensitivity: procSens,
            processAutoGameLock: procAutoLock,
            appCaptureModes: appModes,
            pinnedAppIds: pinned
        )
    }

    static func save(_ s: ClientSettings) {
        defaults.set(s.host, forKey: "winbridge.host")
        defaults.set(Int(s.port), forKey: "winbridge.port")
        if KeychainStore.saveToken(s.authToken) {
            defaults.removeObject(forKey: legacyTokenKey)
        } else {
            // Fallback only if Keychain is unavailable (e.g. restricted headless test runner).
            defaults.set(s.authToken, forKey: legacyTokenKey)
        }
        defaults.set(s.useTls, forKey: "winbridge.useTls")
        defaults.set(s.trustedFingerprints, forKey: "winbridge.trustedFingerprints")
        defaults.set(s.autoReconnect, forKey: "winbridge.autoReconnect")
        defaults.set(s.showPerformanceHud, forKey: "winbridge.showPerformanceHud")
        defaults.set(s.suppressInactiveHover, forKey: "winbridge.suppressInactiveHover")
        defaults.set(s.codecPreference.rawValue, forKey: "winbridge.codec")
        defaults.set(s.cmdMapsToCtrl, forKey: "winbridge.cmdToCtrl")
        defaults.set(s.syncClipboard, forKey: "winbridge.syncClipboard")
        defaults.set(s.followPopups, forKey: "winbridge.followPopups")
        defaults.set(s.syncWindowResize, forKey: "winbridge.syncWindowResize")
        defaults.set(s.audioEnabled, forKey: "winbridge.audioEnabled")
        defaults.set(s.audioVolume, forKey: "winbridge.audioVolume")
        defaults.set(s.mouseSensitivity, forKey: "winbridge.mouseSensitivity")
        defaults.set(Int(s.targetFps), forKey: "winbridge.fps")
        defaults.set(s.processCaptureModes.mapValues { Int($0.rawValue) }, forKey: "winbridge.processCaptureModes")
        defaults.set(s.processMouseSensitivity, forKey: "winbridge.processMouseSensitivity")
        defaults.set(s.processAutoGameLock, forKey: "winbridge.processAutoGameLock")
        defaults.set(s.appCaptureModes.mapValues { Int($0.rawValue) }, forKey: "winbridge.appCaptureModes")
        defaults.set(s.pinnedAppIds, forKey: "winbridge.pinnedAppIds")
    }
}
