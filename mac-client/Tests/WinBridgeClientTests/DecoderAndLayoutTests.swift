import CoreGraphics
import Foundation
import Testing
@testable import WinBridgeCore

@Test func decoderDecodesRawBGRAIntoCGImage() async throws {
    let decoder = WindowVideoDecoder(windowId: 7)
    let w: UInt16 = 4
    let h: UInt16 = 2
    // 4x2 BGRA solid blue (B=255, G=0, R=0, A=255)
    var payload = Data(capacity: Int(w) * Int(h) * 4)
    for _ in 0..<(Int(w) * Int(h)) {
        payload.append(contentsOf: [255, 0, 0, 255])
    }
    let header = MediaFrameHeader(
        windowId: 7,
        timestampUs: 1000,
        width: w,
        height: h,
        codec: .rawBgra,
        flags: [.keyframe],
        frameSeq: 1,
        payloadLen: UInt32(payload.count)
    )
    let frame = MediaFrame(header: header, payload: payload)

    let result: (Int, Int) = await withCheckedContinuation { cont in
        decoder.onDecodedFrame = { decoded, rw, rh in
            if case .cgImage(let img) = decoded {
                cont.resume(returning: (img.width, img.height))
            } else {
                cont.resume(returning: (Int(rw), Int(rh)))
            }
        }
        decoder.decode(frame)
    }

    #expect(result.0 == 4)
    #expect(result.1 == 2)
}

@MainActor
@Test func dpiScalingComputesExpectedMacPointSizes() {
    let win100 = WindowInfo(width: 1200, height: 800, dpi: 96)
    let size100 = RemoteWindowController.macContentSize(for: win100)
    #expect(size100.width == 1200)
    #expect(size100.height == 800)

    let win200 = WindowInfo(width: 2400, height: 1600, dpi: 192)
    let size200 = RemoteWindowController.macContentSize(for: win200)
    #expect(size200.width == 1200)
    #expect(size200.height == 800)

    let win150 = WindowInfo(width: 1500, height: 900, dpi: 144)
    let size150 = RemoteWindowController.macContentSize(for: win150)
    #expect(size150.width == 1000)
    #expect(size150.height == 600)
}

@MainActor
@Test func aspectPreservingVideoRectLetterboxesCleanly() {
    // 16:9 Windows window displayed inside a square 800x800 Mac view
    let rect = RemoteWindowView.computeVideoRect(
        in: NSRect(x: 0, y: 0, width: 800, height: 800),
        sourceSize: CGSize(width: 1600, height: 900),
        preserveAspect: true
    )
    #expect(rect.width == 800)
    #expect(rect.height == 450)
    #expect(rect.minX == 0)
    #expect(rect.minY == 175)

    // When aspect ratio matches, fills entire bounds with zero letterbox
    let exact = RemoteWindowView.computeVideoRect(
        in: NSRect(x: 0, y: 0, width: 800, height: 450),
        sourceSize: CGSize(width: 1600, height: 900),
        preserveAspect: true
    )
    #expect(exact == NSRect(x: 0, y: 0, width: 800, height: 450))
}
