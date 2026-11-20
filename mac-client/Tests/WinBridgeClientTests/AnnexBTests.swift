import Foundation
import Testing
@testable import WinBridgeCore

@Test func parsesWinHostAnnexBKeyframeIntoSpsPpsAndAvcc() {
    let stream = Data([
        0, 0, 0, 1, 0x67, 0x42, 0x00, 0x1E,
        0, 0, 0, 1, 0x68, 0xCE, 0x3C, 0x80,
        0, 0, 0, 1, 0x65, 0x88, 0x84, 0x01, 0x02,
    ])
    let au = AnnexBParser.parseH264(stream)
    #expect(au.isIDR)
    #expect(au.sps == Data([0x67, 0x42, 0x00, 0x1E]))
    #expect(au.pps == Data([0x68, 0xCE, 0x3C, 0x80]))
    #expect(au.avccPayload == Data([0, 0, 0, 5, 0x65, 0x88, 0x84, 0x01, 0x02]))
}

@Test func parsesThreeByteStartCodesAndSkipsAud() {
    let stream = Data([
        0, 0, 1, 0x09, 0x10,
        0, 0, 1, 0x41, 0x9A, 0x22,
    ])
    let au = AnnexBParser.parseH264(stream)
    #expect(!au.isIDR)
    #expect(au.sps == nil)
    #expect(au.pps == nil)
    #expect(au.avccPayload == Data([0, 0, 0, 3, 0x41, 0x9A, 0x22]))
}
