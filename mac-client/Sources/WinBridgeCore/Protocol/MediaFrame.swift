import Foundation

enum WireConstants {
    static let controlPreface   = Data([0x57, 0x42, 0x52, 0x47, 0x43, 0x54, 0x4C, 0x01]) // "WBRGCTL\x01"
    static let mediaPreface     = Data([0x57, 0x42, 0x52, 0x47, 0x4D, 0x45, 0x44, 0x01]) // "WBRGMED\x01"
    static let bulkPreface      = Data([0x57, 0x42, 0x52, 0x47, 0x46, 0x4C, 0x53, 0x01]) // "WBRGFLS\x01"
    static let quicAudioPreface = Data([0x57, 0x42, 0x52, 0x47, 0x41, 0x55, 0x44, 0x01]) // "WBRGAUD\x01"
    static let quicVideoPreface = Data([0x57, 0x42, 0x52, 0x47, 0x56, 0x49, 0x44, 0x01]) // "WBRGVID\x01"
    static let quicAlpn = "winbridge-v1"
    static let mediaFrameMagic: UInt32 = 0x5742_4D46 // "WBMF"
    static let mediaHeaderSize = 32
    static let maxControlPayload = 16 * 1024 * 1024
    static let maxMediaPayload = 64 * 1024 * 1024
}

struct MediaFrameFlags: OptionSet, Equatable, Sendable {
    let rawValue: UInt8
    static let keyframe   = MediaFrameFlags(rawValue: 0x01)
    static let configOnly = MediaFrameFlags(rawValue: 0x02)
    static let endOfStream = MediaFrameFlags(rawValue: 0x04)
}

enum MediaFrameError: Error, LocalizedError {
    case invalidHeaderLength(Int)
    case badMagic(UInt32)
    case payloadTooLarge(UInt32)

    var errorDescription: String? {
        switch self {
        case .invalidHeaderLength(let n): return "Expected 32-byte media frame header, got \(n)"
        case .badMagic(let m): return String(format: "Bad media frame magic: 0x%08X", m)
        case .payloadTooLarge(let n): return "Media frame payload exceeds 64 MiB: \(n)"
        }
    }
}

struct MediaFrameHeader: Equatable, Sendable {
    var windowId: UInt64
    var timestampUs: UInt64
    var width: UInt16
    var height: UInt16
    var codec: VideoCodec
    var flags: MediaFrameFlags
    var frameSeq: UInt16
    var payloadLen: UInt32

    static func decode(from data: Data) throws -> MediaFrameHeader {
        guard data.count == WireConstants.mediaHeaderSize else {
            throw MediaFrameError.invalidHeaderLength(data.count)
        }
        return try data.withUnsafeBytes { (raw: UnsafeRawBufferPointer) in
            func u16(_ offset: Int) -> UInt16 {
                UInt16(raw[offset]) << 8 | UInt16(raw[offset + 1])
            }
            func u32(_ offset: Int) -> UInt32 {
                (UInt32(raw[offset]) << 24)
                    | (UInt32(raw[offset + 1]) << 16)
                    | (UInt32(raw[offset + 2]) << 8)
                    | UInt32(raw[offset + 3])
            }
            func u64(_ offset: Int) -> UInt64 {
                (UInt64(u32(offset)) << 32) | UInt64(u32(offset + 4))
            }

            let magic = u32(0)
            guard magic == WireConstants.mediaFrameMagic else {
                throw MediaFrameError.badMagic(magic)
            }
            let payloadLen = u32(28)
            guard payloadLen <= UInt32(WireConstants.maxMediaPayload) else {
                throw MediaFrameError.payloadTooLarge(payloadLen)
            }
            return MediaFrameHeader(
                windowId: u64(4),
                timestampUs: u64(12),
                width: u16(20),
                height: u16(22),
                codec: VideoCodec(rawValue: Int32(raw[24])) ?? .unspecified,
                flags: MediaFrameFlags(rawValue: raw[25]),
                frameSeq: u16(26),
                payloadLen: payloadLen
            )
        }
    }

    func encode() -> Data {
        var out = Data(capacity: WireConstants.mediaHeaderSize)
        func appendBE<T: FixedWidthInteger>(_ v: T) {
            var be = v.bigEndian
            withUnsafeBytes(of: &be) { out.append(contentsOf: $0) }
        }
        appendBE(WireConstants.mediaFrameMagic)
        appendBE(windowId)
        appendBE(timestampUs)
        appendBE(width)
        appendBE(height)
        out.append(UInt8(truncatingIfNeeded: codec.rawValue))
        out.append(flags.rawValue)
        appendBE(frameSeq)
        appendBE(payloadLen)
        return out
    }
}

struct MediaFrame: Sendable {
    let header: MediaFrameHeader
    let payload: Data
}
