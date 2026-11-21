import Foundation

enum CodecPreference: String, CaseIterable {
    case auto = "auto"
    case hevc = "hevc"
    case h264 = "h264"
    case jpeg = "jpeg"
    case raw  = "raw"

    var title: String {
        switch self {
        case .auto: return "Auto (HEVC → H.264)"
        case .hevc: return "HEVC (H.265) 硬件"
        case .h264: return "H.264 硬件"
        case .jpeg: return "JPEG"
        case .raw:  return "RAW BGRA"
        }
    }

    var offeredCodecs: [VideoCodec] {
        switch self {
        case .auto: return [.hevcAnnexB, .h264AnnexB, .jpeg, .rawBgra]
        case .hevc: return [.hevcAnnexB]
        case .h264: return [.h264AnnexB]
        case .jpeg: return [.jpeg]
        case .raw:  return [.rawBgra]
        }
    }
}

enum AudioQualityPreference: String, CaseIterable {
    case opus128 = "opus128"
    case opus64  = "opus64"
    case pcm     = "pcm"

    var title: String {
        switch self {
        case .opus128: return "Opus 128k"
        case .opus64:  return "Opus 64k"
        case .pcm:     return "PCM 无损"
        }
    }

    var audioCodec: AudioCodec {
        switch self {
        case .opus128, .opus64: return .opus
        case .pcm: return .pcmS16Le
        }
    }

    var bitrateKbps: UInt32 {
        switch self {
        case .opus128: return 128
        case .opus64:  return 64
        case .pcm:     return 0
        }
    }
}

enum VideoProfilePreference: String, CaseIterable {
    case auto = "auto"
    case text = "text"
    case game = "game"

    var title: String {
        switch self {
        case .auto: return "自动"
        case .text: return "文字"
        case .game: return "游戏"
        }
    }

    var protoValue: VideoProfile {
        switch self {
        case .auto: return .auto
        case .text: return .text
        case .game: return .game
        }
    }
}

enum TransportPreference: String, CaseIterable, Sendable {
    case auto = "auto"
    case quic = "quic"
    case tcp  = "tcp"

    var title: String {
        switch self {
        case .auto: return "自动 (QUIC 优先)"
        case .quic: return "仅 QUIC (UDP)"
        case .tcp:  return "仅 TCP"
        }
    }
}

enum ActiveTransport: String, Equatable, Sendable {
    case quic = "QUIC"
    case tcp  = "TCP"
}

struct HostProfile: Codable, Equatable, Sendable {
    var id: String
    var name: String
    var host: String
    var port: UInt16
    var useTls: Bool
    var macAddress: String
}

struct ClientSettings {
    var host: String
    var port: UInt16
    var authToken: String
    var useTls: Bool
    var transportPreference: TransportPreference
    var macAddress: String
    var hostProfiles: [HostProfile]
    var selectedHostProfileId: String
    var trustedFingerprints: [String: String]
    var autoReconnect: Bool
    var showPerformanceHud: Bool
    var suppressInactiveHover: Bool
    var codecPreference: CodecPreference
    var cmdMapsToCtrl: Bool
    var syncClipboard: Bool
    var autoPullClipboardFiles: Bool
    var followPopups: Bool
    var syncWindowResize: Bool
    var audioEnabled: Bool
    var audioQuality: AudioQualityPreference
    var audioProcessOnly: Bool
    var audioVolume: Float
    var mouseSensitivity: Float
    var targetFps: UInt32
    var videoProfile: VideoProfilePreference
    var adaptiveBitrate: Bool
    var processVideoProfiles: [String: VideoProfilePreference]
    var processCaptureModes: [String: CaptureMode]
    var processMouseSensitivity: [String: Float]
    var processAutoGameLock: [String: Bool]
    var appCaptureModes: [String: CaptureMode]
    var pinnedAppIds: [String]
}

enum SettingsStore {
    private static let defaults = UserDefaults(suiteName: "com.winbridge.client") ?? UserDefaults.standard
    private static let legacyTokenKey = "winbridge.token"
    private static let hostProfilesKey = "winbridge.hostProfiles"
    private static let selectedProfileKey = "winbridge.selectedHostProfileId"

    static func load() -> ClientSettings {
        var host = defaults.string(forKey: "winbridge.host") ?? "192.168.1.100"
        let portInt = defaults.integer(forKey: "winbridge.port")
        var port = (portInt > 0 && portInt <= 65535) ? UInt16(portInt) : 9876
        var useTls = defaults.object(forKey: "winbridge.useTls") as? Bool ?? true
        var macAddress = defaults.string(forKey: "winbridge.macAddress") ?? ""

        var hostProfiles: [HostProfile] = []
        if let rawData = defaults.data(forKey: hostProfilesKey),
           let decoded = try? JSONDecoder().decode([HostProfile].self, from: rawData),
           !decoded.isEmpty {
            hostProfiles = decoded
        }
        var selectedProfileId = defaults.string(forKey: selectedProfileKey) ?? KeychainStore.defaultAccount
        if hostProfiles.isEmpty {
            let defaultName = host.isEmpty ? "默认主机" : host
            hostProfiles = [
                HostProfile(
                    id: KeychainStore.defaultAccount,
                    name: defaultName,
                    host: host,
                    port: port,
                    useTls: useTls,
                    macAddress: macAddress
                )
            ]
            selectedProfileId = KeychainStore.defaultAccount
        } else if let selected = hostProfiles.first(where: { $0.id == selectedProfileId }) {
            host = selected.host
            port = selected.port
            useTls = selected.useTls
            macAddress = selected.macAddress
        } else {
            let first = hostProfiles[0]
            selectedProfileId = first.id
            host = first.host
            port = first.port
            useTls = first.useTls
            macAddress = first.macAddress
        }

        // Prefer macOS Keychain for auth token; automatically migrate legacy plaintext UserDefaults token once.
        let account = KeychainStore.accountName(forProfileId: selectedProfileId)
        let token: String
        if let keychainToken = KeychainStore.loadToken(account: account) {
            token = keychainToken
            if selectedProfileId == KeychainStore.defaultAccount, defaults.object(forKey: legacyTokenKey) != nil {
                defaults.removeObject(forKey: legacyTokenKey)
            }
        } else if selectedProfileId == KeychainStore.defaultAccount,
                  let legacy = defaults.string(forKey: legacyTokenKey),
                  !legacy.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            let trimmed = legacy.trimmingCharacters(in: .whitespacesAndNewlines)
            token = trimmed
            if KeychainStore.saveToken(trimmed, account: account) {
                defaults.removeObject(forKey: legacyTokenKey)
            }
        } else {
            token = ""
        }

        let trustedFingerprints = defaults.dictionary(forKey: "winbridge.trustedFingerprints") as? [String: String] ?? [:]
        let transportRaw = defaults.string(forKey: "winbridge.transport") ?? TransportPreference.auto.rawValue
        let transportPreference = TransportPreference(rawValue: transportRaw) ?? .auto
        let autoReconnect = defaults.object(forKey: "winbridge.autoReconnect") as? Bool ?? true
        let showPerformanceHud = defaults.object(forKey: "winbridge.showPerformanceHud") as? Bool ?? false
        let suppressInactiveHover = defaults.object(forKey: "winbridge.suppressInactiveHover") as? Bool ?? false

        let codecRaw = defaults.string(forKey: "winbridge.codec") ?? CodecPreference.auto.rawValue
        let codec = CodecPreference(rawValue: codecRaw) ?? .auto
        let cmdToCtrl = defaults.object(forKey: "winbridge.cmdToCtrl") as? Bool ?? true
        let clipboard = defaults.object(forKey: "winbridge.syncClipboard") as? Bool ?? true
        let autoPullClipboardFiles = defaults.object(forKey: "winbridge.autoPullClipboardFiles") as? Bool ?? true
        let popups = defaults.object(forKey: "winbridge.followPopups") as? Bool ?? true
        let syncResize = defaults.object(forKey: "winbridge.syncWindowResize") as? Bool ?? false
        let audioEnabled = defaults.object(forKey: "winbridge.audioEnabled") as? Bool ?? false
        let audioQualityRaw = defaults.string(forKey: "winbridge.audioQuality") ?? AudioQualityPreference.opus128.rawValue
        let audioQuality = AudioQualityPreference(rawValue: audioQualityRaw) ?? .opus128
        let audioProcessOnly = defaults.object(forKey: "winbridge.audioProcessOnly") as? Bool ?? false
        let audioVolume = defaults.object(forKey: "winbridge.audioVolume") as? Float ?? 0.85
        let mouseSensitivity = defaults.object(forKey: "winbridge.mouseSensitivity") as? Float ?? 1.0
        let fpsInt = defaults.integer(forKey: "winbridge.fps")
        let fps = (fpsInt >= 1 && fpsInt <= 240) ? UInt32(fpsInt) : 60
        let profileRaw = defaults.string(forKey: "winbridge.videoProfile") ?? VideoProfilePreference.auto.rawValue
        let videoProfile = VideoProfilePreference(rawValue: profileRaw) ?? .auto
        let adaptiveBitrate = defaults.object(forKey: "winbridge.adaptiveBitrate") as? Bool ?? false
        let rawProcProfiles = defaults.dictionary(forKey: "winbridge.processVideoProfiles") as? [String: String] ?? [:]
        var procProfiles: [String: VideoProfilePreference] = [:]
        for (proc, raw) in rawProcProfiles {
            if let pref = VideoProfilePreference(rawValue: raw) {
                procProfiles[proc] = pref
            }
        }
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
            transportPreference: transportPreference,
            macAddress: macAddress,
            hostProfiles: hostProfiles,
            selectedHostProfileId: selectedProfileId,
            trustedFingerprints: trustedFingerprints,
            autoReconnect: autoReconnect,
            showPerformanceHud: showPerformanceHud,
            suppressInactiveHover: suppressInactiveHover,
            codecPreference: codec,
            cmdMapsToCtrl: cmdToCtrl,
            syncClipboard: clipboard,
            autoPullClipboardFiles: autoPullClipboardFiles,
            followPopups: popups,
            syncWindowResize: syncResize,
            audioEnabled: audioEnabled,
            audioQuality: audioQuality,
            audioProcessOnly: audioProcessOnly,
            audioVolume: max(0, min(1, audioVolume)),
            mouseSensitivity: max(0.1, min(4.0, mouseSensitivity)),
            targetFps: fps,
            videoProfile: videoProfile,
            adaptiveBitrate: adaptiveBitrate,
            processVideoProfiles: procProfiles,
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
        defaults.set(s.macAddress, forKey: "winbridge.macAddress")
        defaults.set(s.selectedHostProfileId, forKey: selectedProfileKey)
        if let encodedProfiles = try? JSONEncoder().encode(s.hostProfiles) {
            defaults.set(encodedProfiles, forKey: hostProfilesKey)
        }
        let account = KeychainStore.accountName(forProfileId: s.selectedHostProfileId)
        if KeychainStore.saveToken(s.authToken, account: account) {
            if s.selectedHostProfileId == KeychainStore.defaultAccount {
                defaults.removeObject(forKey: legacyTokenKey)
            }
        } else if s.selectedHostProfileId == KeychainStore.defaultAccount {
            // Fallback only if Keychain is unavailable (e.g. restricted headless test runner).
            defaults.set(s.authToken, forKey: legacyTokenKey)
        }
        defaults.set(s.useTls, forKey: "winbridge.useTls")
        defaults.set(s.transportPreference.rawValue, forKey: "winbridge.transport")
        defaults.set(s.trustedFingerprints, forKey: "winbridge.trustedFingerprints")
        defaults.set(s.autoReconnect, forKey: "winbridge.autoReconnect")
        defaults.set(s.showPerformanceHud, forKey: "winbridge.showPerformanceHud")
        defaults.set(s.suppressInactiveHover, forKey: "winbridge.suppressInactiveHover")
        defaults.set(s.codecPreference.rawValue, forKey: "winbridge.codec")
        defaults.set(s.cmdMapsToCtrl, forKey: "winbridge.cmdToCtrl")
        defaults.set(s.syncClipboard, forKey: "winbridge.syncClipboard")
        defaults.set(s.autoPullClipboardFiles, forKey: "winbridge.autoPullClipboardFiles")
        defaults.set(s.followPopups, forKey: "winbridge.followPopups")
        defaults.set(s.syncWindowResize, forKey: "winbridge.syncWindowResize")
        defaults.set(s.audioEnabled, forKey: "winbridge.audioEnabled")
        defaults.set(s.audioQuality.rawValue, forKey: "winbridge.audioQuality")
        defaults.set(s.audioProcessOnly, forKey: "winbridge.audioProcessOnly")
        defaults.set(s.audioVolume, forKey: "winbridge.audioVolume")
        defaults.set(s.mouseSensitivity, forKey: "winbridge.mouseSensitivity")
        defaults.set(Int(s.targetFps), forKey: "winbridge.fps")
        defaults.set(s.videoProfile.rawValue, forKey: "winbridge.videoProfile")
        defaults.set(s.adaptiveBitrate, forKey: "winbridge.adaptiveBitrate")
        defaults.set(s.processVideoProfiles.mapValues(\.rawValue), forKey: "winbridge.processVideoProfiles")
        defaults.set(s.processCaptureModes.mapValues { Int($0.rawValue) }, forKey: "winbridge.processCaptureModes")
        defaults.set(s.processMouseSensitivity, forKey: "winbridge.processMouseSensitivity")
        defaults.set(s.processAutoGameLock, forKey: "winbridge.processAutoGameLock")
        defaults.set(s.appCaptureModes.mapValues { Int($0.rawValue) }, forKey: "winbridge.appCaptureModes")
        defaults.set(s.pinnedAppIds, forKey: "winbridge.pinnedAppIds")
    }
}
