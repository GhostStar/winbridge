import Foundation
import Testing
@testable import WinBridgeCore

@Suite("Milestone 2: UX, Discovery, HUD, File Transfer & Gamepad")
struct Milestone2Tests {
    @Test("GamepadState and FileTransfer protobuf messages round-trip cleanly")
    func gamepadAndFileTransferRoundTrip() throws {
        let gp = GamepadState(
            index: 1,
            connected: true,
            buttons: 0x5011,
            leftTrigger: 180,
            rightTrigger: 255,
            thumbLX: -32768,
            thumbLY: 32767,
            thumbRX: 1024,
            thumbRY: -2048
        )
        let decodedGp = try ControlMessage.decode(
            from: ControlMessage(seq: 42, timestampUs: 99, payload: .gamepadState(gp)).encode()
        )
        #expect(decodedGp.payload == .gamepadState(gp))

        let start = FileTransferStart(
            transferId: 7,
            targetWindowId: 0x1234,
            fileName: "screenshot 截图.png",
            totalBytes: 123_456
        )
        let decodedStart = try ControlMessage.decode(
            from: ControlMessage(seq: 43, timestampUs: 100, payload: .fileTransferStart(start)).encode()
        )
        #expect(decodedStart.payload == .fileTransferStart(start))

        let chunk = FileTransferChunk(
            transferId: 7,
            offset: 65_536,
            data: Data([1, 2, 3, 4, 5]),
            isLast: true
        )
        let decodedChunk = try ControlMessage.decode(
            from: ControlMessage(seq: 44, timestampUs: 101, payload: .fileTransferChunk(chunk)).encode()
        )
        #expect(decodedChunk.payload == .fileTransferChunk(chunk))

        let res = FileTransferResult(
            transferId: 7,
            success: true,
            savedPath: #"C:\Users\Demo\Downloads\WinBridge\screenshot 截图.png"#,
            message: ""
        )
        let decodedRes = try ControlMessage.decode(
            from: ControlMessage(seq: 45, timestampUs: 102, payload: .fileTransferRes(res)).encode()
        )
        #expect(decodedRes.payload == .fileTransferRes(res))
    }

    @Test("Bonjour TXT record parsing extracts TLS flag and short fingerprint")
    func bonjourTxtParsing() {
        let txt = NetService.data(fromTXTRecord: [
            "v": Data("1".utf8),
            "tls": Data("1".utf8),
            "fp": Data("AABBCCDD11223344".utf8),
        ])
        let parsed = DiscoveredHost.parseTXTRecord(txt)
        #expect(parsed.tls == true)
        #expect(parsed.shortFingerprint == "AABBCCDD11223344")

        let noTlsTxt = NetService.data(fromTXTRecord: ["tls": Data("0".utf8)])
        let parsedNoTls = DiscoveredHost.parseTXTRecord(noTlsTxt)
        #expect(parsedNoTls.tls == false)
        #expect(parsedNoTls.shortFingerprint == nil)
    }

    @Test("Performance HUD formatter renders concise metrics line")
    func performanceHudFormatting() {
        let line = PerformanceHUDFormatter.format(
            codecName: "HEVC",
            fps: 59.8,
            mbps: 12.34,
            rttMs: 1.4,
            width: 2560,
            height: 1440
        )
        #expect(line == "HEVC · 60 FPS · 12.3 Mbps · RTT 1.4ms · 2560×1440")
    }

    @Test("Gamepad axis and trigger scaling matches XInput ranges")
    func gamepadScalingRanges() {
        #expect(GamepadMonitor.axisToInt16(-1.0) == -32768)
        #expect(GamepadMonitor.axisToInt16(0.0) == 0)
        #expect(GamepadMonitor.axisToInt16(1.0) == 32767)
        #expect(GamepadMonitor.triggerToUInt8(0.0) == 0)
        #expect(GamepadMonitor.triggerToUInt8(1.0) == 255)
    }

    @Test("Auto-reconnect backoff sequence caps cleanly")
    func reconnectBackoffSchedule() {
        #expect(AppState.reconnectDelaySeconds(forAttempt: 1) == 1)
        #expect(AppState.reconnectDelaySeconds(forAttempt: 2) == 2)
        #expect(AppState.reconnectDelaySeconds(forAttempt: 3) == 4)
        #expect(AppState.reconnectDelaySeconds(forAttempt: 4) == 8)
        #expect(AppState.reconnectDelaySeconds(forAttempt: 5) == 15)
        #expect(AppState.reconnectDelaySeconds(forAttempt: 9) == 15)
    }
}
