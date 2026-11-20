import Foundation

/// Parses H.264 / HEVC Annex-B byte streams (3-byte or 4-byte start codes)
/// into individual NAL units and converts slices to 4-byte length-prefixed AVCC/HVCC format
/// required by Apple VideoToolbox.
enum AnnexBParser {
    struct H264AccessUnit {
        var sps: Data?
        var pps: Data?
        var avccPayload: Data
        var isIDR: Bool
    }

    /// Splits an Annex-B stream into raw NAL units (excluding start codes).
    static func splitNALUnits(_ data: Data) -> [Data] {
        return data.withUnsafeBytes { (raw: UnsafeRawBufferPointer) -> [Data] in
            guard let base = raw.bindMemory(to: UInt8.self).baseAddress else { return [] }
            let count = raw.count
            var units: [Data] = []
            var i = 0

            func findStartCode(from start: Int) -> (pos: Int, len: Int)? {
                var idx = start
                let limit = count - 2
                while idx < limit {
                    if base[idx] == 0 && base[idx + 1] == 0 {
                        if base[idx + 2] == 1 {
                            return (idx, 3)
                        } else if idx + 3 < count && base[idx + 2] == 0 && base[idx + 3] == 1 {
                            return (idx, 4)
                        }
                    }
                    idx += 1
                }
                return nil
            }

            while let sc = findStartCode(from: i) {
                let nalStart = sc.pos + sc.len
                if let next = findStartCode(from: nalStart) {
                    if next.pos > nalStart {
                        units.append(Data(bytes: base.advanced(by: nalStart), count: next.pos - nalStart))
                    }
                    i = next.pos
                } else {
                    if count > nalStart {
                        units.append(Data(bytes: base.advanced(by: nalStart), count: count - nalStart))
                    }
                    break
                }
            }
            return units
        }
    }

    static func parseH264(_ annexB: Data) -> H264AccessUnit {
        let nals = splitNALUnits(annexB)
        var sps: Data?
        var pps: Data?
        var isIDR = false
        var avcc = Data(capacity: annexB.count + 16)

        for nal in nals where !nal.isEmpty {
            let nalType = nal[0] & 0x1F
            switch nalType {
            case 7:
                sps = nal
            case 8:
                pps = nal
            case 9:
                // Access Unit Delimiter — not needed by VideoToolbox.
                continue
            case 5:
                isIDR = true
                appendLengthPrefixed(&avcc, nal: nal)
            default:
                // Slice (1), SEI (6), etc.
                appendLengthPrefixed(&avcc, nal: nal)
            }
        }
        return H264AccessUnit(sps: sps, pps: pps, avccPayload: avcc, isIDR: isIDR)
    }

    private static func appendLengthPrefixed(_ out: inout Data, nal: Data) {
        var beLen = UInt32(nal.count).bigEndian
        withUnsafeBytes(of: &beLen) { out.append(contentsOf: $0) }
        out.append(nal)
    }
}
