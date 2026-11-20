import Foundation

// ============================================================================
// WinBridge Protocol v0.1 — Swift Models & Proto3 Codec
// Mirrors protocol/control.proto (package winbridge.v1) 1:1.
// ============================================================================

enum VideoCodec: Int32, CaseIterable, Sendable {
    case unspecified = 0
    case h264AnnexB = 1
    case hevcAnnexB = 2
    case jpeg = 3
    case rawBgra = 4

    var displayName: String {
        switch self {
        case .unspecified: return "Unspecified"
        case .h264AnnexB: return "H.264"
        case .hevcAnnexB: return "HEVC"
        case .jpeg: return "JPEG"
        case .rawBgra: return "RAW BGRA"
        }
    }
}

enum CaptureMode: Int32, CaseIterable, Sendable {
    case unspecified = 0
    case window = 1
    case monitorCrop = 2

    var shortTitle: String {
        switch self {
        case .unspecified, .window: return "单窗"
        case .monitorCrop: return "屏幕裁剪"
        }
    }
}

enum WindowKind: Int32, Sendable {
    case unspecified = 0
    case normal = 1
    case dialog = 2
    case popup = 3
}

enum MouseEventKind: Int32, Sendable {
    case unspecified = 0
    case move = 1
    case buttonDown = 2
    case buttonUp = 3
    case wheel = 4
}

enum MouseButton: Int32, Sendable {
    case unspecified = 0
    case left = 1
    case right = 2
    case middle = 3
    case x1 = 4
    case x2 = 5
}

struct KeyModifierMask: OptionSet, Sendable {
    let rawValue: UInt32
    static let lShift = KeyModifierMask(rawValue: 1)
    static let rShift = KeyModifierMask(rawValue: 2)
    static let lCtrl  = KeyModifierMask(rawValue: 4)
    static let rCtrl  = KeyModifierMask(rawValue: 8)
    static let lAlt   = KeyModifierMask(rawValue: 16)
    static let rAlt   = KeyModifierMask(rawValue: 32)
    static let lWin   = KeyModifierMask(rawValue: 64)
    static let rWin   = KeyModifierMask(rawValue: 128)
}

enum WindowActionType: Int32, Sendable {
    case unspecified = 0
    case maximize = 1
    case restore = 2
    case bringToFront = 3
}

enum CursorType: Int32, Sendable {
    case unspecified = 0
    case arrow = 1
    case ibeam = 2
    case hand = 3
    case crosshair = 4
    case wait = 5
    case appStarting = 6
    case sizeNS = 7
    case sizeWE = 8
    case sizeNWSE = 9
    case sizeNESW = 10
    case sizeAll = 11
    case no = 12
    case hidden = 13
    case custom = 14
}

enum HostErrorCode: Int32, Sendable {
    case unspecified = 0
    case windowNotFound = 1
    case captureFailed = 2
    case capturePausedMinimized = 3
    case displayDisconnected = 4
    case encoderError = 5
    case accessDeniedElevated = 6
}

// MARK: - Messages

struct Hello: Equatable, Sendable {
    var protocolVersion: UInt32 = 1
    var clientName: String = ""
    var authToken: String = ""
    var displayScale: Float = 2.0
    var supportedCodecs: [VideoCodec] = []

    func encode() -> Data {
        var w = ProtoWriter()
        w.writeUInt32(field: 1, protocolVersion)
        w.writeString(field: 2, clientName)
        w.writeString(field: 3, authToken)
        w.writeFloat(field: 4, displayScale)
        w.writePackedEnums(field: 5, supportedCodecs.map(\.rawValue))
        return w.data
    }

    static func decode(from data: Data) throws -> Hello {
        var r = ProtoReader(data: data)
        var msg = Hello(protocolVersion: 0, clientName: "", authToken: "", displayScale: 0, supportedCodecs: [])
        while !r.isAtEnd {
            let (field, wire) = try r.readTag()
            switch field {
            case 1: msg.protocolVersion = try r.readUInt32()
            case 2: msg.clientName = try r.readString()
            case 3: msg.authToken = try r.readString()
            case 4: msg.displayScale = try r.readFloat()
            case 5:
                if wire == 2 {
                    let raw = try r.readPackedInt32s()
                    msg.supportedCodecs.append(contentsOf: raw.compactMap(VideoCodec.init(rawValue:)))
                } else {
                    if let c = VideoCodec(rawValue: try r.readInt32()) {
                        msg.supportedCodecs.append(c)
                    }
                }
            default: try r.skip(wireType: wire)
            }
        }
        return msg
    }
}

struct HelloAck: Equatable, Sendable {
    var accepted: Bool = false
    var rejectReason: String = ""
    var sessionId: Data = Data()
    var hostName: String = ""
    var selectedCodec: VideoCodec = .unspecified
    var defaultFps: UInt32 = 60

    func encode() -> Data {
        var w = ProtoWriter()
        w.writeBool(field: 1, accepted)
        w.writeString(field: 2, rejectReason)
        w.writeBytes(field: 3, sessionId)
        w.writeString(field: 4, hostName)
        w.writeInt32(field: 5, selectedCodec.rawValue)
        w.writeUInt32(field: 6, defaultFps)
        return w.data
    }

    static func decode(from data: Data) throws -> HelloAck {
        var r = ProtoReader(data: data)
        var msg = HelloAck(accepted: false, rejectReason: "", sessionId: Data(), hostName: "", selectedCodec: .unspecified, defaultFps: 0)
        while !r.isAtEnd {
            let (field, wire) = try r.readTag()
            switch field {
            case 1: msg.accepted = try r.readBool()
            case 2: msg.rejectReason = try r.readString()
            case 3: msg.sessionId = try r.readBytes()
            case 4: msg.hostName = try r.readString()
            case 5: msg.selectedCodec = VideoCodec(rawValue: try r.readInt32()) ?? .unspecified
            case 6: msg.defaultFps = try r.readUInt32()
            default: try r.skip(wireType: wire)
            }
        }
        return msg
    }
}

struct Ping: Equatable, Sendable {
    var echoTimestampUs: UInt64 = 0

    func encode() -> Data {
        var w = ProtoWriter()
        w.writeUInt64(field: 1, echoTimestampUs)
        return w.data
    }

    static func decode(from data: Data) throws -> Ping {
        var r = ProtoReader(data: data)
        var msg = Ping()
        while !r.isAtEnd {
            let (field, wire) = try r.readTag()
            if field == 1 { msg.echoTimestampUs = try r.readUInt64() } else { try r.skip(wireType: wire) }
        }
        return msg
    }
}

struct Pong: Equatable, Sendable {
    var echoTimestampUs: UInt64 = 0

    func encode() -> Data {
        var w = ProtoWriter()
        w.writeUInt64(field: 1, echoTimestampUs)
        return w.data
    }

    static func decode(from data: Data) throws -> Pong {
        var r = ProtoReader(data: data)
        var msg = Pong()
        while !r.isAtEnd {
            let (field, wire) = try r.readTag()
            if field == 1 { msg.echoTimestampUs = try r.readUInt64() } else { try r.skip(wireType: wire) }
        }
        return msg
    }
}

struct WindowInfo: Equatable, Sendable {
    var windowId: UInt64 = 0
    var ownerWindowId: UInt64 = 0
    var pid: UInt32 = 0
    var processName: String = ""
    var title: String = ""
    var kind: WindowKind = .unspecified
    var x: Int32 = 0
    var y: Int32 = 0
    var width: UInt32 = 0
    var height: UInt32 = 0
    var offsetX: Int32 = 0
    var offsetY: Int32 = 0
    var dpi: UInt32 = 96
    var isMinimized: Bool = false
    var isMaximized: Bool = false
    var isResizable: Bool = false
    var hasAlpha: Bool = false
    var zOrder: UInt32 = 0
    var iconPng: Data = Data()

    func encode() -> Data {
        var w = ProtoWriter()
        w.writeUInt64(field: 1, windowId)
        w.writeUInt64(field: 2, ownerWindowId)
        w.writeUInt32(field: 3, pid)
        w.writeString(field: 4, processName)
        w.writeString(field: 5, title)
        w.writeInt32(field: 6, kind.rawValue)
        w.writeInt32(field: 7, x)
        w.writeInt32(field: 8, y)
        w.writeUInt32(field: 9, width)
        w.writeUInt32(field: 10, height)
        w.writeInt32(field: 11, offsetX)
        w.writeInt32(field: 12, offsetY)
        w.writeUInt32(field: 13, dpi)
        w.writeBool(field: 14, isMinimized)
        w.writeBool(field: 15, isMaximized)
        w.writeBool(field: 16, isResizable)
        w.writeBool(field: 17, hasAlpha)
        w.writeUInt32(field: 18, zOrder)
        w.writeBytes(field: 19, iconPng)
        return w.data
    }

    static func decode(from data: Data) throws -> WindowInfo {
        var r = ProtoReader(data: data)
        var m = WindowInfo(dpi: 0)
        while !r.isAtEnd {
            let (field, wire) = try r.readTag()
            switch field {
            case 1: m.windowId = try r.readUInt64()
            case 2: m.ownerWindowId = try r.readUInt64()
            case 3: m.pid = try r.readUInt32()
            case 4: m.processName = try r.readString()
            case 5: m.title = try r.readString()
            case 6: m.kind = WindowKind(rawValue: try r.readInt32()) ?? .unspecified
            case 7: m.x = try r.readInt32()
            case 8: m.y = try r.readInt32()
            case 9: m.width = try r.readUInt32()
            case 10: m.height = try r.readUInt32()
            case 11: m.offsetX = try r.readInt32()
            case 12: m.offsetY = try r.readInt32()
            case 13: m.dpi = try r.readUInt32()
            case 14: m.isMinimized = try r.readBool()
            case 15: m.isMaximized = try r.readBool()
            case 16: m.isResizable = try r.readBool()
            case 17: m.hasAlpha = try r.readBool()
            case 18: m.zOrder = try r.readUInt32()
            case 19: m.iconPng = try r.readBytes()
            default: try r.skip(wireType: wire)
            }
        }
        return m
    }
}

struct ListWindowsRequest: Equatable, Sendable {
    var includeIcons: Bool = true

    func encode() -> Data {
        var w = ProtoWriter()
        w.writeBool(field: 1, includeIcons)
        return w.data
    }

    static func decode(from data: Data) throws -> ListWindowsRequest {
        var r = ProtoReader(data: data)
        var msg = ListWindowsRequest(includeIcons: false)
        while !r.isAtEnd {
            let (field, wire) = try r.readTag()
            if field == 1 { msg.includeIcons = try r.readBool() } else { try r.skip(wireType: wire) }
        }
        return msg
    }
}

struct ListWindowsResponse: Equatable, Sendable {
    var windows: [WindowInfo] = []

    func encode() -> Data {
        var w = ProtoWriter()
        for win in windows {
            w.writeMessage(field: 1, win.encode(), forceEmit: true)
        }
        return w.data
    }

    static func decode(from data: Data) throws -> ListWindowsResponse {
        var r = ProtoReader(data: data)
        var msg = ListWindowsResponse()
        while !r.isAtEnd {
            let (field, wire) = try r.readTag()
            if field == 1 {
                msg.windows.append(try WindowInfo.decode(from: try r.readBytes()))
            } else {
                try r.skip(wireType: wire)
            }
        }
        return msg
    }
}

struct SubscribeWindow: Equatable, Sendable {
    var windowId: UInt64 = 0
    var targetFps: UInt32 = 0
    var maxBitrateKbps: UInt32 = 0
    var followPopups: Bool = true
    var captureMode: CaptureMode = .unspecified

    func encode() -> Data {
        var w = ProtoWriter()
        w.writeUInt64(field: 1, windowId)
        w.writeUInt32(field: 2, targetFps)
        w.writeUInt32(field: 3, maxBitrateKbps)
        w.writeBool(field: 4, followPopups)
        w.writeInt32(field: 5, captureMode.rawValue)
        return w.data
    }

    static func decode(from data: Data) throws -> SubscribeWindow {
        var r = ProtoReader(data: data)
        var m = SubscribeWindow(followPopups: false, captureMode: .unspecified)
        while !r.isAtEnd {
            let (field, wire) = try r.readTag()
            switch field {
            case 1: m.windowId = try r.readUInt64()
            case 2: m.targetFps = try r.readUInt32()
            case 3: m.maxBitrateKbps = try r.readUInt32()
            case 4: m.followPopups = try r.readBool()
            case 5: m.captureMode = CaptureMode(rawValue: try r.readInt32()) ?? .unspecified
            default: try r.skip(wireType: wire)
            }
        }
        return m
    }
}

struct UnsubscribeWindow: Equatable, Sendable {
    var windowId: UInt64 = 0

    func encode() -> Data {
        var w = ProtoWriter()
        w.writeUInt64(field: 1, windowId)
        return w.data
    }

    static func decode(from data: Data) throws -> UnsubscribeWindow {
        var r = ProtoReader(data: data)
        var m = UnsubscribeWindow()
        while !r.isAtEnd {
            let (field, wire) = try r.readTag()
            if field == 1 { m.windowId = try r.readUInt64() } else { try r.skip(wireType: wire) }
        }
        return m
    }
}

struct WindowUpdated: Equatable, Sendable {
    var window: WindowInfo?
    var ackResizeSeq: UInt32 = 0

    func encode() -> Data {
        var w = ProtoWriter()
        if let window {
            w.writeMessage(field: 1, window.encode(), forceEmit: true)
        }
        w.writeUInt32(field: 2, ackResizeSeq)
        return w.data
    }

    static func decode(from data: Data) throws -> WindowUpdated {
        var r = ProtoReader(data: data)
        var m = WindowUpdated()
        while !r.isAtEnd {
            let (field, wire) = try r.readTag()
            switch field {
            case 1: m.window = try WindowInfo.decode(from: try r.readBytes())
            case 2: m.ackResizeSeq = try r.readUInt32()
            default: try r.skip(wireType: wire)
            }
        }
        return m
    }
}

struct WindowClosed: Equatable, Sendable {
    var windowId: UInt64 = 0

    func encode() -> Data {
        var w = ProtoWriter()
        w.writeUInt64(field: 1, windowId)
        return w.data
    }

    static func decode(from data: Data) throws -> WindowClosed {
        var r = ProtoReader(data: data)
        var m = WindowClosed()
        while !r.isAtEnd {
            let (field, wire) = try r.readTag()
            if field == 1 { m.windowId = try r.readUInt64() } else { try r.skip(wireType: wire) }
        }
        return m
    }
}

struct CursorShape: Equatable, Sendable {
    var windowId: UInt64 = 0
    var cursorType: CursorType = .unspecified
    var customRgba: Data = Data()
    var width: UInt32 = 0
    var height: UInt32 = 0
    var hotspotX: UInt32 = 0
    var hotspotY: UInt32 = 0

    func encode() -> Data {
        var w = ProtoWriter()
        w.writeUInt64(field: 1, windowId)
        w.writeInt32(field: 2, cursorType.rawValue)
        w.writeBytes(field: 3, customRgba)
        w.writeUInt32(field: 4, width)
        w.writeUInt32(field: 5, height)
        w.writeUInt32(field: 6, hotspotX)
        w.writeUInt32(field: 7, hotspotY)
        return w.data
    }

    static func decode(from data: Data) throws -> CursorShape {
        var r = ProtoReader(data: data)
        var m = CursorShape()
        while !r.isAtEnd {
            let (field, wire) = try r.readTag()
            switch field {
            case 1: m.windowId = try r.readUInt64()
            case 2: m.cursorType = CursorType(rawValue: try r.readInt32()) ?? .unspecified
            case 3: m.customRgba = try r.readBytes()
            case 4: m.width = try r.readUInt32()
            case 5: m.height = try r.readUInt32()
            case 6: m.hotspotX = try r.readUInt32()
            case 7: m.hotspotY = try r.readUInt32()
            default: try r.skip(wireType: wire)
            }
        }
        return m
    }
}

struct ResizeWindow: Equatable, Sendable {
    var windowId: UInt64 = 0
    var width: UInt32 = 0
    var height: UInt32 = 0
    var resizeSeq: UInt32 = 0

    func encode() -> Data {
        var w = ProtoWriter()
        w.writeUInt64(field: 1, windowId)
        w.writeUInt32(field: 2, width)
        w.writeUInt32(field: 3, height)
        w.writeUInt32(field: 4, resizeSeq)
        return w.data
    }

    static func decode(from data: Data) throws -> ResizeWindow {
        var r = ProtoReader(data: data)
        var m = ResizeWindow()
        while !r.isAtEnd {
            let (field, wire) = try r.readTag()
            switch field {
            case 1: m.windowId = try r.readUInt64()
            case 2: m.width = try r.readUInt32()
            case 3: m.height = try r.readUInt32()
            case 4: m.resizeSeq = try r.readUInt32()
            default: try r.skip(wireType: wire)
            }
        }
        return m
    }
}

struct FocusWindow: Equatable, Sendable {
    var windowId: UInt64 = 0
    var focused: Bool = false

    func encode() -> Data {
        var w = ProtoWriter()
        w.writeUInt64(field: 1, windowId)
        w.writeBool(field: 2, focused)
        return w.data
    }

    static func decode(from data: Data) throws -> FocusWindow {
        var r = ProtoReader(data: data)
        var m = FocusWindow()
        while !r.isAtEnd {
            let (field, wire) = try r.readTag()
            switch field {
            case 1: m.windowId = try r.readUInt64()
            case 2: m.focused = try r.readBool()
            default: try r.skip(wireType: wire)
            }
        }
        return m
    }
}

struct CloseWindow: Equatable, Sendable {
    var windowId: UInt64 = 0

    func encode() -> Data {
        var w = ProtoWriter()
        w.writeUInt64(field: 1, windowId)
        return w.data
    }

    static func decode(from data: Data) throws -> CloseWindow {
        var r = ProtoReader(data: data)
        var m = CloseWindow()
        while !r.isAtEnd {
            let (field, wire) = try r.readTag()
            if field == 1 { m.windowId = try r.readUInt64() } else { try r.skip(wireType: wire) }
        }
        return m
    }
}

struct WindowAction: Equatable, Sendable {
    var windowId: UInt64 = 0
    var action: WindowActionType = .unspecified

    func encode() -> Data {
        var w = ProtoWriter()
        w.writeUInt64(field: 1, windowId)
        w.writeInt32(field: 2, action.rawValue)
        return w.data
    }

    static func decode(from data: Data) throws -> WindowAction {
        var r = ProtoReader(data: data)
        var m = WindowAction()
        while !r.isAtEnd {
            let (field, wire) = try r.readTag()
            switch field {
            case 1: m.windowId = try r.readUInt64()
            case 2: m.action = WindowActionType(rawValue: try r.readInt32()) ?? .unspecified
            default: try r.skip(wireType: wire)
            }
        }
        return m
    }
}

struct RequestKeyFrame: Equatable, Sendable {
    var windowId: UInt64 = 0

    func encode() -> Data {
        var w = ProtoWriter()
        w.writeUInt64(field: 1, windowId)
        return w.data
    }

    static func decode(from data: Data) throws -> RequestKeyFrame {
        var r = ProtoReader(data: data)
        var m = RequestKeyFrame()
        while !r.isAtEnd {
            let (field, wire) = try r.readTag()
            if field == 1 { m.windowId = try r.readUInt64() } else { try r.skip(wireType: wire) }
        }
        return m
    }
}

struct MouseEvent: Equatable, Sendable {
    var windowId: UInt64 = 0
    var kind: MouseEventKind = .unspecified
    var normX: Float = 0
    var normY: Float = 0
    var button: MouseButton = .unspecified
    var wheelDeltaX: Float = 0
    var wheelDeltaY: Float = 0
    var modifiers: UInt32 = 0

    func encode() -> Data {
        var w = ProtoWriter()
        w.writeUInt64(field: 1, windowId)
        w.writeInt32(field: 2, kind.rawValue)
        w.writeFloat(field: 3, normX)
        w.writeFloat(field: 4, normY)
        w.writeInt32(field: 5, button.rawValue)
        w.writeFloat(field: 6, wheelDeltaX)
        w.writeFloat(field: 7, wheelDeltaY)
        w.writeUInt32(field: 8, modifiers)
        return w.data
    }

    static func decode(from data: Data) throws -> MouseEvent {
        var r = ProtoReader(data: data)
        var m = MouseEvent()
        while !r.isAtEnd {
            let (field, wire) = try r.readTag()
            switch field {
            case 1: m.windowId = try r.readUInt64()
            case 2: m.kind = MouseEventKind(rawValue: try r.readInt32()) ?? .unspecified
            case 3: m.normX = try r.readFloat()
            case 4: m.normY = try r.readFloat()
            case 5: m.button = MouseButton(rawValue: try r.readInt32()) ?? .unspecified
            case 6: m.wheelDeltaX = try r.readFloat()
            case 7: m.wheelDeltaY = try r.readFloat()
            case 8: m.modifiers = try r.readUInt32()
            default: try r.skip(wireType: wire)
            }
        }
        return m
    }
}

struct KeyEvent: Equatable, Sendable {
    var windowId: UInt64 = 0
    var isDown: Bool = false
    var isRepeat: Bool = false
    var hidUsage: UInt32 = 0
    var modifiers: UInt32 = 0

    func encode() -> Data {
        var w = ProtoWriter()
        w.writeUInt64(field: 1, windowId)
        w.writeBool(field: 2, isDown)
        w.writeBool(field: 3, isRepeat)
        w.writeUInt32(field: 4, hidUsage)
        w.writeUInt32(field: 5, modifiers)
        return w.data
    }

    static func decode(from data: Data) throws -> KeyEvent {
        var r = ProtoReader(data: data)
        var m = KeyEvent()
        while !r.isAtEnd {
            let (field, wire) = try r.readTag()
            switch field {
            case 1: m.windowId = try r.readUInt64()
            case 2: m.isDown = try r.readBool()
            case 3: m.isRepeat = try r.readBool()
            case 4: m.hidUsage = try r.readUInt32()
            case 5: m.modifiers = try r.readUInt32()
            default: try r.skip(wireType: wire)
            }
        }
        return m
    }
}

struct TextInput: Equatable, Sendable {
    var windowId: UInt64 = 0
    var text: String = ""

    func encode() -> Data {
        var w = ProtoWriter()
        w.writeUInt64(field: 1, windowId)
        w.writeString(field: 2, text)
        return w.data
    }

    static func decode(from data: Data) throws -> TextInput {
        var r = ProtoReader(data: data)
        var m = TextInput()
        while !r.isAtEnd {
            let (field, wire) = try r.readTag()
            switch field {
            case 1: m.windowId = try r.readUInt64()
            case 2: m.text = try r.readString()
            default: try r.skip(wireType: wire)
            }
        }
        return m
    }
}

struct ClipboardSync: Equatable, Sendable {
    enum Content: Equatable, Sendable {
        case textUtf8(String)
        case imagePng(Data)
    }

    var changeId: UInt64 = 0
    var content: Content?

    func encode() -> Data {
        var w = ProtoWriter()
        w.writeUInt64(field: 1, changeId)
        if let content {
            switch content {
            case .textUtf8(let s): w.writeString(field: 2, s, forceEmit: true)
            case .imagePng(let d): w.writeBytes(field: 3, d, forceEmit: true)
            }
        }
        return w.data
    }

    static func decode(from data: Data) throws -> ClipboardSync {
        var r = ProtoReader(data: data)
        var m = ClipboardSync()
        while !r.isAtEnd {
            let (field, wire) = try r.readTag()
            switch field {
            case 1: m.changeId = try r.readUInt64()
            case 2: m.content = .textUtf8(try r.readString())
            case 3: m.content = .imagePng(try r.readBytes())
            default: try r.skip(wireType: wire)
            }
        }
        return m
    }
}

struct HostError: Equatable, Sendable {
    var windowId: UInt64 = 0
    var code: HostErrorCode = .unspecified
    var message: String = ""

    func encode() -> Data {
        var w = ProtoWriter()
        w.writeUInt64(field: 1, windowId)
        w.writeInt32(field: 2, code.rawValue)
        w.writeString(field: 3, message)
        return w.data
    }

    static func decode(from data: Data) throws -> HostError {
        var r = ProtoReader(data: data)
        var m = HostError()
        while !r.isAtEnd {
            let (field, wire) = try r.readTag()
            switch field {
            case 1: m.windowId = try r.readUInt64()
            case 2: m.code = HostErrorCode(rawValue: try r.readInt32()) ?? .unspecified
            case 3: m.message = try r.readString()
            default: try r.skip(wireType: wire)
            }
        }
        return m
    }
}

// MARK: - Top-level Envelope

struct ControlMessage: Equatable, Sendable {
    enum Payload: Equatable, Sendable {
        case hello(Hello)                               // 10
        case helloAck(HelloAck)                         // 11
        case ping(Ping)                                 // 12
        case pong(Pong)                                 // 13
        case listWindowsReq(ListWindowsRequest)         // 20
        case listWindowsRes(ListWindowsResponse)        // 21
        case subscribeWindow(SubscribeWindow)           // 22
        case unsubscribeWindow(UnsubscribeWindow)       // 23
        case windowUpdated(WindowUpdated)               // 30
        case windowClosed(WindowClosed)                 // 31
        case cursorShape(CursorShape)                   // 32
        case resizeWindow(ResizeWindow)                 // 40
        case focusWindow(FocusWindow)                   // 41
        case closeWindow(CloseWindow)                   // 42
        case windowAction(WindowAction)                 // 43
        case requestKeyFrame(RequestKeyFrame)           // 44
        case mouseEvent(MouseEvent)                     // 50
        case keyEvent(KeyEvent)                         // 51
        case textInput(TextInput)                       // 52
        case clipboardSync(ClipboardSync)               // 60
        case hostError(HostError)                       // 70
    }

    var seq: UInt64 = 0
    var timestampUs: UInt64 = 0
    var payload: Payload

    func encode() -> Data {
        var w = ProtoWriter()
        w.writeUInt64(field: 1, seq)
        w.writeUInt64(field: 2, timestampUs)
        switch payload {
        case .hello(let m): w.writeMessage(field: 10, m.encode(), forceEmit: true)
        case .helloAck(let m): w.writeMessage(field: 11, m.encode(), forceEmit: true)
        case .ping(let m): w.writeMessage(field: 12, m.encode(), forceEmit: true)
        case .pong(let m): w.writeMessage(field: 13, m.encode(), forceEmit: true)
        case .listWindowsReq(let m): w.writeMessage(field: 20, m.encode(), forceEmit: true)
        case .listWindowsRes(let m): w.writeMessage(field: 21, m.encode(), forceEmit: true)
        case .subscribeWindow(let m): w.writeMessage(field: 22, m.encode(), forceEmit: true)
        case .unsubscribeWindow(let m): w.writeMessage(field: 23, m.encode(), forceEmit: true)
        case .windowUpdated(let m): w.writeMessage(field: 30, m.encode(), forceEmit: true)
        case .windowClosed(let m): w.writeMessage(field: 31, m.encode(), forceEmit: true)
        case .cursorShape(let m): w.writeMessage(field: 32, m.encode(), forceEmit: true)
        case .resizeWindow(let m): w.writeMessage(field: 40, m.encode(), forceEmit: true)
        case .focusWindow(let m): w.writeMessage(field: 41, m.encode(), forceEmit: true)
        case .closeWindow(let m): w.writeMessage(field: 42, m.encode(), forceEmit: true)
        case .windowAction(let m): w.writeMessage(field: 43, m.encode(), forceEmit: true)
        case .requestKeyFrame(let m): w.writeMessage(field: 44, m.encode(), forceEmit: true)
        case .mouseEvent(let m): w.writeMessage(field: 50, m.encode(), forceEmit: true)
        case .keyEvent(let m): w.writeMessage(field: 51, m.encode(), forceEmit: true)
        case .textInput(let m): w.writeMessage(field: 52, m.encode(), forceEmit: true)
        case .clipboardSync(let m): w.writeMessage(field: 60, m.encode(), forceEmit: true)
        case .hostError(let m): w.writeMessage(field: 70, m.encode(), forceEmit: true)
        }
        return w.data
    }

    static func decode(from data: Data) throws -> ControlMessage {
        var r = ProtoReader(data: data)
        var seq: UInt64 = 0
        var ts: UInt64 = 0
        var payload: Payload?
        while !r.isAtEnd {
            let (field, wire) = try r.readTag()
            switch field {
            case 1: seq = try r.readUInt64()
            case 2: ts = try r.readUInt64()
            case 10: payload = .hello(try Hello.decode(from: try r.readBytes()))
            case 11: payload = .helloAck(try HelloAck.decode(from: try r.readBytes()))
            case 12: payload = .ping(try Ping.decode(from: try r.readBytes()))
            case 13: payload = .pong(try Pong.decode(from: try r.readBytes()))
            case 20: payload = .listWindowsReq(try ListWindowsRequest.decode(from: try r.readBytes()))
            case 21: payload = .listWindowsRes(try ListWindowsResponse.decode(from: try r.readBytes()))
            case 22: payload = .subscribeWindow(try SubscribeWindow.decode(from: try r.readBytes()))
            case 23: payload = .unsubscribeWindow(try UnsubscribeWindow.decode(from: try r.readBytes()))
            case 30: payload = .windowUpdated(try WindowUpdated.decode(from: try r.readBytes()))
            case 31: payload = .windowClosed(try WindowClosed.decode(from: try r.readBytes()))
            case 32: payload = .cursorShape(try CursorShape.decode(from: try r.readBytes()))
            case 40: payload = .resizeWindow(try ResizeWindow.decode(from: try r.readBytes()))
            case 41: payload = .focusWindow(try FocusWindow.decode(from: try r.readBytes()))
            case 42: payload = .closeWindow(try CloseWindow.decode(from: try r.readBytes()))
            case 43: payload = .windowAction(try WindowAction.decode(from: try r.readBytes()))
            case 44: payload = .requestKeyFrame(try RequestKeyFrame.decode(from: try r.readBytes()))
            case 50: payload = .mouseEvent(try MouseEvent.decode(from: try r.readBytes()))
            case 51: payload = .keyEvent(try KeyEvent.decode(from: try r.readBytes()))
            case 52: payload = .textInput(try TextInput.decode(from: try r.readBytes()))
            case 60: payload = .clipboardSync(try ClipboardSync.decode(from: try r.readBytes()))
            case 70: payload = .hostError(try HostError.decode(from: try r.readBytes()))
            default: try r.skip(wireType: wire)
            }
        }
        guard let payload else { throw ProtoError.missingPayload }
        return ControlMessage(seq: seq, timestampUs: ts, payload: payload)
    }
}
