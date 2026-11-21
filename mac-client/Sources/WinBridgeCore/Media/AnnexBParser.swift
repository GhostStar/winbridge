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

    struct HEVCAccessUnit {
        var vps: Data?
        var sps: Data?
        var pps: Data?
        var hvccPayload: Data
        var isIRAP: Bool
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
                appendLengthPrefixed(&avcc, nal: nal)
            }
        }
        return H264AccessUnit(sps: sps, pps: pps, avccPayload: avcc, isIDR: isIDR)
    }

    static func parseHEVC(_ annexB: Data) -> HEVCAccessUnit {
        let nals = splitNALUnits(annexB)
        var vps: Data?
        var sps: Data?
        var pps: Data?
        var isIRAP = false
        var hvcc = Data(capacity: annexB.count + 16)

        for nal in nals where nal.count >= 2 {
            let nalType = (nal[0] >> 1) & 0x3F
            switch nalType {
            case 32:
                vps = nal
            case 33:
                sps = nal
            case 34:
                pps = nal
            case 35:
                // Access Unit Delimiter — not needed by VideoToolbox.
                continue
            case 19, 20, 21:
                // IDR_W_RADL (19), IDR_N_LP (20), CRA_NUT (21)
                isIRAP = true
                appendLengthPrefixed(&hvcc, nal: nal)
            default:
                appendLengthPrefixed(&hvcc, nal: nal)
            }
        }
        return HEVCAccessUnit(vps: vps, sps: sps, pps: pps, hvccPayload: hvcc, isIRAP: isIRAP)
    }

    private static func appendLengthPrefixed(_ out: inout Data, nal: Data) {
        var beLen = UInt32(nal.count).bigEndian
        withUnsafeBytes(of: &beLen) { out.append(contentsOf: $0) }
        out.append(nal)
    }
}
