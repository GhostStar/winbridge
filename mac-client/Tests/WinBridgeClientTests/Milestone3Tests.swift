import Foundation
import Testing
@testable import WinBridgeCore

@Suite("Milestone 3: HEVC (H.265) & Opus Audio")
struct Milestone3Tests {
    @Test("AnnexBParser extracts HEVC VPS/SPS/PPS, detects IRAP, and emits 4-byte HVCC slices")
    func parsesHevcAnnexBIntoVpsSpsPpsAndHvcc() {
        // HEVC 2-byte NAL headers: (nalType << 1)
        // VPS=32 (0x40), SPS=33 (0x42), PPS=34 (0x44), AUD=35 (0x46), IDR_W_RADL=19 (0x26)
        let vps = Data([0x40, 0x01, 0x0C, 0x01])
        let sps = Data([0x42, 0x01, 0x01, 0x60, 0x00])
        let pps = Data([0x44, 0x01, 0xC1, 0x73])
        let aud = Data([0x46, 0x01, 0x10])
        let idr = Data([0x26, 0x01, 0xAF, 0xDE, 0xAD])

        var stream = Data()
        for nal in [aud, vps, sps, pps, idr] {
            stream.append(contentsOf: [0x00, 0x00, 0x00, 0x01])
            stream.append(nal)
        }

        let au = AnnexBParser.parseHEVC(stream)
        #expect(au.vps == vps)
        #expect(au.sps == sps)
        #expect(au.pps == pps)
        #expect(au.isIRAP == true)
        #expect(au.hvccPayload == Data([0x00, 0x00, 0x00, 0x05]) + idr)

        // Trailing non-IRAP picture (TRAIL_R = 1 -> 0x02)
        let pSlice = Data([0x02, 0x01, 0x77, 0x88])
        let pAu = AnnexBParser.parseHEVC(Data([0x00, 0x00, 0x01]) + pSlice)
        #expect(pAu.vps == nil)
        #expect(pAu.isIRAP == false)
        #expect(pAu.hvccPayload == Data([0x00, 0x00, 0x00, 0x04]) + pSlice)
    }

    @Test("SetAudioConfig Opus codec & bitrate round-trip and MediaFrameHeader codec 11")
    func audioOpusProtocolRoundTrip() throws {
        let cfg = SetAudioConfig(enabled: true, codec: .opus, bitrateKbps: 128)
        let decoded = try ControlMessage.decode(
            from: ControlMessage(seq: 10, timestampUs: 20, payload: .setAudioConfig(cfg)).encode()
        )
        #expect(decoded.payload == .setAudioConfig(cfg))

        let hdr = MediaFrameHeader(
            windowId: 0,
            timestampUs: 999,
            width: 48_000,
            height: 2,
            codec: .audioOpus,
            flags: .keyframe,
            frameSeq: 5,
            payloadLen: 3
        )
        let roundTrip = try MediaFrameHeader.decode(from: hdr.encode())
        #expect(roundTrip == hdr)
        #expect(roundTrip.codec == .audioOpus)
    }

    @Test("Native AVAudioConverter OpusPacketDecoder decodes RFC 6716 20ms stereo packets into steady-state 960 frames")
    func opusPacketDecoderDecodesValidStereoFrame() {
        let decoder = OpusPacketDecoder(sampleRate: 48_000, channels: 2)
        #expect(decoder != nil)
        // Valid RFC 6716 20ms fullband stereo CELT silence frame (TOC=0xFC: config=31 FB 20ms, stereo=1, code=0)
        let opusPacket = Data([0xFC, 0xFF, 0xFE])

        // First packet may strip Opus priming/pre-skip samples (e.g. 960 - 120 = 840 frames).
        let first = decoder?.decode(opusPacket)
        #expect(first != nil)
        #expect((first?.frameLength ?? 0) > 0)
        #expect(first?.format.channelCount == 2)

        // Steady-state packets decode full 20ms (960 frames @ 48 kHz).
        let second = decoder?.decode(opusPacket)
        #expect(second != nil)
        #expect(second?.frameLength == 960)

        if let second, let ch = second.floatChannelData {
            let ring = StereoAudioRingBuffer(sampleRate: 48_000)
            ring.pushFloat32(left: ch[0], right: ch[1], frameCount: Int(second.frameLength))
            #expect(ring.bufferedFrames == 960)
        }
    }
}
