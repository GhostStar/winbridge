import Foundation
import Testing
@testable import WinBridgeCore

@Suite("Phase C — QUIC Multi-Stream Wire Constants & Automatic Fallback Policy")
struct PhaseCTests {
    @Test("QUIC stream prefaces, ALPN, and capability bit match host specification")
    func quicWireConstantsAndCapabilities() {
        #expect(WireConstants.quicAlpn == "winbridge-v1")
        #expect(WireConstants.controlPreface == Data("WBRGCTL\u{01}".utf8))
        #expect(WireConstants.mediaPreface == Data("WBRGMED\u{01}".utf8))
        #expect(WireConstants.bulkPreface == Data("WBRGFLS\u{01}".utf8))
        #expect(WireConstants.quicAudioPreface == Data("WBRGAUD\u{01}".utf8))
        #expect(WireConstants.quicVideoPreface == Data("WBRGVID\u{01}".utf8))
        #expect((ProtocolCapabilities.clientSupported & ProtocolCapabilities.quic) != 0)
    }

    @Test("Auto transport falls back to TCP on UDP timeout/failure but never bypasses auth or TLS security errors")
    func quicToTcpFallbackPolicy() {
        // Recoverable UDP/QUIC errors fall back in .auto mode
        #expect(WinBridgeConnection.shouldFallbackToTCP(transport: .auto, error: ConnectionError.connectTimeout))
        #expect(WinBridgeConnection.shouldFallbackToTCP(transport: .auto, error: ConnectionError.connectionClosed))
        #expect(WinBridgeConnection.shouldFallbackToTCP(transport: .auto, error: ConnectionError.quicStreamOpenFailed))
        #expect(WinBridgeConnection.shouldFallbackToTCP(transport: .auto, error: NSError(domain: "udp", code: 61)))

        // Hard security / auth / permission errors must NOT silently fall back
        #expect(!WinBridgeConnection.shouldFallbackToTCP(
            transport: .auto,
            error: ConnectionError.handshakeRejected("authentication failed")
        ))
        #expect(!WinBridgeConnection.shouldFallbackToTCP(
            transport: .auto,
            error: ConnectionError.certificateMismatch(hostKey: "192.168.1.1:9876", expected: "AA", actual: "BB")
        ))
        #expect(!WinBridgeConnection.shouldFallbackToTCP(
            transport: .auto,
            error: ConnectionError.localNetworkProhibited
        ))
        #expect(!WinBridgeConnection.shouldFallbackToTCP(
            transport: .auto,
            error: ConnectionError.invalidEndpoint("bad", 0)
        ))

        // Explicit .quic mode never silently falls back
        #expect(!WinBridgeConnection.shouldFallbackToTCP(transport: .quic, error: ConnectionError.connectTimeout))
        #expect(!WinBridgeConnection.shouldFallbackToTCP(transport: .tcp, error: ConnectionError.connectTimeout))
    }
}
