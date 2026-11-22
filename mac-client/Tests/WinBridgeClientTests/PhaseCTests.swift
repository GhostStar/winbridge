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

        // User-cancelled / already-disconnected attempts must NEVER fall back to TCP or failover host
        #expect(!WinBridgeConnection.shouldFallbackToTCP(
            transport: .auto,
            error: ConnectionError.connectionClosed,
            isCancelled: true
        ))
        #expect(!WinBridgeConnection.shouldFailoverToFallbackHost(
            primaryHost: "192.168.1.1",
            fallbackHost: "100.64.0.2",
            error: ConnectionError.connectionClosed,
            isCancelled: true
        ))
    }

    @Test("Disconnected WinBridgeConnection aborts connect() immediately without zombie fallback")
    func disconnectedConnectionAbortsConnectImmediately() async {
        let conn = WinBridgeConnection()
        conn.disconnect(error: nil)
        #expect(conn.cancelledOrDisconnected)

        let config = ConnectionConfig(
            host: "192.168.1.126",
            fallbackHost: "100.64.0.2",
            port: 9876,
            authToken: "tok",
            useTls: true,
            transport: .auto,
            clientName: "TestMac",
            displayScale: 2.0,
            supportedCodecs: [.hevcAnnexB]
        )
        do {
            _ = try await conn.connect(config: config)
            Issue.record("Expected already-disconnected connection to abort connect()")
        } catch let err as ConnectionError {
            if case .connectionClosed = err {
                // Expected
            } else {
                Issue.record("Unexpected ConnectionError: \(err)")
            }
        } catch {
            Issue.record("Unexpected error: \(error)")
        }
    }
}
