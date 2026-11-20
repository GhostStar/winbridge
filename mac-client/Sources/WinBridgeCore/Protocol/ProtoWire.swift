import Foundation

enum ProtoError: Error, LocalizedError {
    case truncated
    case invalidVarint
    case invalidWireType(UInt8)
    case invalidUTF8
    case invalidLength(Int)
    case missingPayload

    var errorDescription: String? {
        switch self {
        case .truncated: return "Truncated Protobuf message"
        case .invalidVarint: return "Malformed Protobuf varint"
        case .invalidWireType(let w): return "Unsupported Protobuf wire type \(w)"
        case .invalidUTF8: return "Invalid UTF-8 string in Protobuf field"
        case .invalidLength(let n): return "Invalid Protobuf length \(n)"
        case .missingPayload: return "ControlMessage has empty payload"
        }
    }
}

/// Minimal, strict Proto3 binary writer compatible with `prost` and `protoc`.
struct ProtoWriter {
    private(set) var data = Data()

    mutating func writeVarint(_ value: UInt64) {
        var v = value
        while v >= 0x80 {
            data.append(UInt8((v & 0x7F) | 0x80))
            v >>= 7
        }
        data.append(UInt8(v))
    }

    mutating func writeTag(field: Int, wireType: UInt8) {
        let key = (UInt64(field) << 3) | UInt64(wireType)
        writeVarint(key)
    }

    mutating func writeUInt64(field: Int, _ value: UInt64, forceEmit: Bool = false) {
        guard forceEmit || value != 0 else { return }
        writeTag(field: field, wireType: 0)
        writeVarint(value)
    }

    mutating func writeUInt32(field: Int, _ value: UInt32, forceEmit: Bool = false) {
        guard forceEmit || value != 0 else { return }
        writeTag(field: field, wireType: 0)
        writeVarint(UInt64(value))
    }

    /// Proto3 signed int32 uses 10-byte two's-complement varint for negative numbers.
    mutating func writeInt32(field: Int, _ value: Int32, forceEmit: Bool = false) {
        guard forceEmit || value != 0 else { return }
        writeTag(field: field, wireType: 0)
        writeVarint(UInt64(bitPattern: Int64(value)))
    }

    mutating func writeBool(field: Int, _ value: Bool, forceEmit: Bool = false) {
        guard forceEmit || value else { return }
        writeTag(field: field, wireType: 0)
        data.append(value ? 1 : 0)
    }

    mutating func writeFloat(field: Int, _ value: Float, forceEmit: Bool = false) {
        guard forceEmit || value != 0 else { return }
        writeTag(field: field, wireType: 5)
        var le = value.bitPattern.littleEndian
        withUnsafeBytes(of: &le) { data.append(contentsOf: $0) }
    }

    mutating func writeString(field: Int, _ value: String, forceEmit: Bool = false) {
        guard forceEmit || !value.isEmpty else { return }
        let utf8 = Data(value.utf8)
        writeBytes(field: field, utf8, forceEmit: true)
    }

    mutating func writeBytes(field: Int, _ value: Data, forceEmit: Bool = false) {
        guard forceEmit || !value.isEmpty else { return }
        writeTag(field: field, wireType: 2)
        writeVarint(UInt64(value.count))
        data.append(value)
    }

    mutating func writeMessage(field: Int, _ encoded: Data, forceEmit: Bool = false) {
        guard forceEmit || !encoded.isEmpty else { return }
        writeTag(field: field, wireType: 2)
        writeVarint(UInt64(encoded.count))
        data.append(encoded)
    }

    /// Packed repeated varint (Proto3 default for repeated enums/scalars).
    mutating func writePackedEnums(field: Int, _ values: [Int32]) {
        guard !values.isEmpty else { return }
        var inner = ProtoWriter()
        for v in values {
            inner.writeVarint(UInt64(bitPattern: Int64(v)))
        }
        writeBytes(field: field, inner.data, forceEmit: true)
    }
}

/// Minimal, strict Proto3 binary reader supporting both packed and unpacked repeated fields.
struct ProtoReader {
    private let bytes: [UInt8]
    private var offset: Int

    init(data: Data) {
        self.bytes = [UInt8](data)
        self.offset = 0
    }

    var isAtEnd: Bool { offset >= bytes.count }

    mutating func readVarint() throws -> UInt64 {
        var result: UInt64 = 0
        var shift: UInt64 = 0
        while offset < bytes.count {
            let b = bytes[offset]
            offset += 1
            if shift == 63 && b > 1 {
                throw ProtoError.invalidVarint
            }
            result |= UInt64(b & 0x7F) << shift
            if (b & 0x80) == 0 {
                return result
            }
            shift += 7
            if shift >= 64 {
                throw ProtoError.invalidVarint
            }
        }
        throw ProtoError.truncated
    }

    mutating func readTag() throws -> (field: Int, wireType: UInt8) {
        let key = try readVarint()
        let wireType = UInt8(key & 0x07)
        let field = Int(key >> 3)
        return (field, wireType)
    }

    mutating func readUInt64() throws -> UInt64 {
        try readVarint()
    }

    mutating func readUInt32() throws -> UInt32 {
        UInt32(truncatingIfNeeded: try readVarint())
    }

    mutating func readInt32() throws -> Int32 {
        Int32(truncatingIfNeeded: Int64(bitPattern: try readVarint()))
    }

    mutating func readBool() throws -> Bool {
        try readVarint() != 0
    }

    mutating func readFloat() throws -> Float {
        guard offset + 4 <= bytes.count else { throw ProtoError.truncated }
        let bits = UInt32(bytes[offset])
            | (UInt32(bytes[offset + 1]) << 8)
            | (UInt32(bytes[offset + 2]) << 16)
            | (UInt32(bytes[offset + 3]) << 24)
        offset += 4
        return Float(bitPattern: bits)
    }

    mutating func readBytes() throws -> Data {
        let len64 = try readVarint()
        guard len64 <= UInt64(bytes.count - offset) else {
            throw ProtoError.invalidLength(Int(min(len64, UInt64(Int.max))))
        }
        let len = Int(len64)
        let slice = bytes[offset..<(offset + len)]
        offset += len
        return Data(slice)
    }

    mutating func readString() throws -> String {
        let raw = try readBytes()
        guard let s = String(data: raw, encoding: .utf8) else {
            throw ProtoError.invalidUTF8
        }
        return s
    }

    mutating func readPackedInt32s() throws -> [Int32] {
        let raw = try readBytes()
        var sub = ProtoReader(data: raw)
        var out: [Int32] = []
        while !sub.isAtEnd {
            out.append(try sub.readInt32())
        }
        return out
    }

    mutating func skip(wireType: UInt8) throws {
        switch wireType {
        case 0:
            _ = try readVarint()
        case 1:
            guard offset + 8 <= bytes.count else { throw ProtoError.truncated }
            offset += 8
        case 2:
            _ = try readBytes()
        case 5:
            guard offset + 4 <= bytes.count else { throw ProtoError.truncated }
            offset += 4
        default:
            throw ProtoError.invalidWireType(wireType)
        }
    }
}
