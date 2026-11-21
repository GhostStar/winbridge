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
    var codecPreference: CodecPreference
    var cmdMapsToCtrl: Bool
    var syncClipboard: Bool
    var followPopups: Bool
    var syncWindowResize: Bool
    var audioEnabled: Bool
    var audioVolume: Float
    var targetFps: UInt32
    var processCaptureModes: [String: CaptureMode]
    var appCaptureModes: [String: CaptureMode]
    var pinnedAppIds: [String]
}

enum SettingsStore {
    private static let defaults = UserDefaults(suiteName: "com.winbridge.client") ?? UserDefaults.standard

    static func load() -> ClientSettings {
        let host = defaults.string(forKey: "winbridge.host") ?? "192.168.1.100"
        let portInt = defaults.integer(forKey: "winbridge.port")
        let port = (portInt > 0 && portInt <= 65535) ? UInt16(portInt) : 9876
        let token = defaults.string(forKey: "winbridge.token") ?? ""
        let codecRaw = defaults.string(forKey: "winbridge.codec") ?? CodecPreference.auto.rawValue
        let codec = CodecPreference(rawValue: codecRaw) ?? .auto
        let cmdToCtrl = defaults.object(forKey: "winbridge.cmdToCtrl") as? Bool ?? true
        let clipboard = defaults.object(forKey: "winbridge.syncClipboard") as? Bool ?? true
        let popups = defaults.object(forKey: "winbridge.followPopups") as? Bool ?? true
        let syncResize = defaults.object(forKey: "winbridge.syncWindowResize") as? Bool ?? false
        let audioEnabled = defaults.object(forKey: "winbridge.audioEnabled") as? Bool ?? false
        let audioVolume = defaults.object(forKey: "winbridge.audioVolume") as? Float ?? 0.85
        let fpsInt = defaults.integer(forKey: "winbridge.fps")
        let fps = (fpsInt >= 1 && fpsInt <= 240) ? UInt32(fpsInt) : 60
        let rawModes = defaults.dictionary(forKey: "winbridge.processCaptureModes") as? [String: Int] ?? [:]
        var modes: [String: CaptureMode] = [:]
        for (proc, raw) in rawModes {
            if let mode = CaptureMode(rawValue: Int32(raw)), mode != .unspecified {
                modes[proc] = mode
            }
        }
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
            codecPreference: codec,
            cmdMapsToCtrl: cmdToCtrl,
            syncClipboard: clipboard,
            followPopups: popups,
            syncWindowResize: syncResize,
            audioEnabled: audioEnabled,
            audioVolume: max(0, min(1, audioVolume)),
            targetFps: fps,
            processCaptureModes: modes,
            appCaptureModes: appModes,
            pinnedAppIds: pinned
        )
    }

    static func save(_ s: ClientSettings) {
        defaults.set(s.host, forKey: "winbridge.host")
        defaults.set(Int(s.port), forKey: "winbridge.port")
        defaults.set(s.authToken, forKey: "winbridge.token")
        defaults.set(s.codecPreference.rawValue, forKey: "winbridge.codec")
        defaults.set(s.cmdMapsToCtrl, forKey: "winbridge.cmdToCtrl")
        defaults.set(s.syncClipboard, forKey: "winbridge.syncClipboard")
        defaults.set(s.followPopups, forKey: "winbridge.followPopups")
        defaults.set(s.syncWindowResize, forKey: "winbridge.syncWindowResize")
        defaults.set(s.audioEnabled, forKey: "winbridge.audioEnabled")
        defaults.set(s.audioVolume, forKey: "winbridge.audioVolume")
        defaults.set(Int(s.targetFps), forKey: "winbridge.fps")
        defaults.set(s.processCaptureModes.mapValues { Int($0.rawValue) }, forKey: "winbridge.processCaptureModes")
        defaults.set(s.appCaptureModes.mapValues { Int($0.rawValue) }, forKey: "winbridge.appCaptureModes")
        defaults.set(s.pinnedAppIds, forKey: "winbridge.pinnedAppIds")
    }
}
