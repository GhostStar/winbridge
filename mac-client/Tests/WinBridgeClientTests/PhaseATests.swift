import Foundation
import Testing
@testable import WinBridgeCore

@Suite("Phase A — WoL, Pairing Code, Capabilities & Per-Window Mute")
struct PhaseATests {
    @Test("WakeOnLan parses MAC formats, rejects invalid MACs, and builds 102-byte magic packet")
    func wakeOnLanMagicPacket() {
        let mac = WakeOnLan.parseMac("aa-bb-cc-01-23-45")
        #expect(mac == [0xAA, 0xBB, 0xCC, 0x01, 0x23, 0x45])
        #expect(WakeOnLan.normalizeMac("aabb.cc01.2345") == "AA:BB:CC:01:23:45")
        #expect(WakeOnLan.normalizeMac("AABBCC012345") == "AA:BB:CC:01:23:45")
        #expect(WakeOnLan.parseMac("00:00:00:00:00:00") == nil)
        #expect(WakeOnLan.parseMac("FF:FF:FF:FF:FF:FF") == nil)
        #expect(WakeOnLan.parseMac("AA:BB:CC") == nil)
        #expect(WakeOnLan.parseMac("ZZ:BB:CC:DD:EE:FF") == nil)

        let packet = WakeOnLan.buildMagicPacket(mac: mac!)
        #expect(packet.count == 102)
        #expect(Array(packet.prefix(6)) == [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF])
        for i in 0..<16 {
            let offset = 6 + i * 6
            #expect(Array(packet[offset..<(offset + 6)]) == [0xAA, 0xBB, 0xCC, 0x01, 0x23, 0x45])
        }

        let targets = WakeOnLan.targetBroadcastAddresses(hostHint: "192.168.50.12")
        #expect(targets.contains("192.168.50.255"))
        #expect(targets.contains("255.255.255.255"))
    }

    @Test("PairingCode parses standard URIs, defaults, and embedded console output")
    func pairingCodeParsing() {
        let raw = """
        Pairing Code (paste into Mac WinBridge):
        winbridge://192.168.1.10:9876?name=Gaming%20PC&token=tok%2B1%2F2&fp=aa%3Abb%3Acc&mac=00%3A11%3A22%3A33%3A44%3A55&tls=1
        """
        let parsed = PairingCode.parse(raw)
        #expect(parsed == ParsedPairingCode(
            host: "192.168.1.10",
            port: 9876,
            token: "tok+1/2",
            fingerprint: "AA:BB:CC",
            macAddress: "00:11:22:33:44:55",
            name: "Gaming PC",
            useTls: true
        ))

        let noTlsDefaultPort = PairingCode.parse("winbridge://desk-pc.local?tls=0")
        #expect(noTlsDefaultPort?.host == "desk-pc.local")
        #expect(noTlsDefaultPort?.port == 9876)
        #expect(noTlsDefaultPort?.useTls == false)

        #expect(PairingCode.parse("https://example.com") == nil)
        #expect(PairingCode.parse("not a pairing uri") == nil)
    }

    @Test("Hello, HelloAck, and SetAudioConfig mutedWindowIds round-trip over protobuf")
    func phaseAProtocolRoundTrip() throws {
        let hello = Hello(
            protocolVersion: 1,
            clientName: "MacBook Pro",
            authToken: "secret",
            displayScale: 2.0,
            supportedCodecs: [.hevcAnnexB, .h264AnnexB],
            appVersion: ProtocolCapabilities.currentAppVersion,
            capabilities: ProtocolCapabilities.clientSupported
        )
        #expect(try Hello.decode(from: hello.encode()) == hello)

        let ack = HelloAck(
            accepted: true,
            rejectReason: "",
            sessionId: Data(repeating: 0x42, count: 16),
            hostName: "DESKTOP-WIN",
            selectedCodec: .hevcAnnexB,
            defaultFps: 60,
            hostVersion: "0.3.0",
            capabilities: ProtocolCapabilities.clientSupported,
            macAddress: "AA:BB:CC:DD:EE:FF"
        )
        #expect(try HelloAck.decode(from: ack.encode()) == ack)

        let audio = SetAudioConfig(
            enabled: true,
            codec: .opus,
            bitrateKbps: 128,
            scope: 1,
            mutedWindowIds: [0x1001, 0x2002, SyntheticWindow.primaryDesktop]
        )
        #expect(try SetAudioConfig.decode(from: audio.encode()) == audio)
    }
}
