import Foundation
import Testing
@testable import WinBridgeCore

@Suite("Security & TOFU Certificate Pinning")
struct SecurityTests {
    @Test("SHA-256 fingerprint formats uppercase colon-separated 32 bytes")
    func sha256FingerprintFormat() {
        let fp = CertificateFingerprint.sha256(der: Data("winbridge-test-cert".utf8))
        let octets = fp.split(separator: ":")
        #expect(octets.count == 32)
        #expect(fp.count == 95)
        #expect(octets.allSatisfy { $0.count == 2 && $0.allSatisfy(\.isHexDigit) })

        let short = CertificateFingerprint.shortDisplay(fp)
        #expect(short.hasSuffix("…"))
        #expect(short.split(separator: ":").count == 8)
    }

    @Test("TOFU evaluator handles first-use, case-insensitive match, and MITM mismatch")
    func tofuEvaluatorTransitions() {
        let certA = Data([0x30, 0x82, 0x01, 0x0A, 0xAA, 0xBB])
        let certB = Data([0x30, 0x82, 0x01, 0x0A, 0xCC, 0xDD])
        let fpA = CertificateFingerprint.sha256(der: certA)
        let fpB = CertificateFingerprint.sha256(der: certB)

        // First use trusts and returns fingerprint
        let first = TOFUTrustEvaluator.evaluate(actualDER: certA, pinnedFingerprint: nil)
        #expect(first == .trustFirstUse(fingerprint: fpA))

        // Match succeeds regardless of case / whitespace
        let matched = TOFUTrustEvaluator.evaluate(actualDER: certA, pinnedFingerprint: "  \(fpA.lowercased())\n")
        #expect(matched == .trustedMatch(fingerprint: fpA))

        // Different certificate is rejected with expected vs actual
        let mismatch = TOFUTrustEvaluator.evaluate(actualDER: certB, pinnedFingerprint: fpA)
        #expect(mismatch == .mismatch(expected: fpA, actual: fpB))
    }

    @Test("Host trust key normalizes whitespace and casing")
    func hostKeyNormalization() {
        #expect(CertificateFingerprint.hostKey(host: " 192.168.1.10 ", port: 9876) == "192.168.1.10:9876")
        #expect(CertificateFingerprint.hostKey(host: "My-PC.Local", port: 9876) == "my-pc.local:9876")
    }
}
