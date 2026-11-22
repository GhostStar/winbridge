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

@Test func stereoAudioRingBufferConvertsPcmScalesVolumeAndTrimsDrift() {
    let ring = StereoAudioRingBuffer(sampleRate: 1000, maxDurationMs: 200, highWaterMs: 90, trimTargetMs: 30)
    ring.setVolume(0.5)

    // 2 stereo frames of S16LE: (+16384, -32768) and (+32767, 0)
    var pcm = Data()
    for sample: Int16 in [16384, -32768, 32767, 0] {
        var le = sample.littleEndian
        withUnsafeBytes(of: &le) { pcm.append(contentsOf: $0) }
    }
    ring.pushPcmS16Le(pcm, channels: 2)
    #expect(ring.bufferedFrames == 2)

    var outL = [Float](repeating: -1, count: 4)
    var outR = [Float](repeating: -1, count: 4)
    outL.withUnsafeMutableBufferPointer { lBuf in
        outR.withUnsafeMutableBufferPointer { rBuf in
            ring.pull(intoLeft: lBuf.baseAddress!, right: rBuf.baseAddress!, frameCount: 4)
        }
    }
    // Volume 0.5: +16384/32768 * 0.5 = 0.25; -32768/32768 * 0.5 = -0.5; underrun zero-filled
    #expect(abs(outL[0] - 0.25) < 0.001)
    #expect(abs(outR[0] - (-0.5)) < 0.001)
    #expect(outL[2] == 0 && outR[2] == 0)
    #expect(outL[3] == 0 && outR[3] == 0)

    // Push 1200 frames (> highWaterFrames=256 floor) to confirm drift trimming keeps latency bounded
    let burst = Data(repeating: 0, count: 1200 * 4)
    ring.pushPcmS16Le(burst, channels: 2)
    #expect(ring.bufferedFrames <= 256)
}

@Test func cursorConfinementSnapsFractionalRectsAndNeverSelfWarpsOnTitleBar() {
    // Odd aspect ratio (e.g. 1924x1127 game window) producing fractional Retina/letterbox coordinates.
    let fractionalRect = NSRect(x: 100.3, y: 200.285, width: 962.0, height: 699.43)
    let bounds = CursorConfinementBounds(videoScreenRect: fractionalRect)!

    // Trigger and warp target rects must be strictly integer-aligned.
    #expect(bounds.triggerRect.minX == bounds.triggerRect.minX.rounded())
    #expect(bounds.triggerRect.maxX == bounds.triggerRect.maxX.rounded())
    #expect(bounds.triggerRect.minY == bounds.triggerRect.minY.rounded())
    #expect(bounds.triggerRect.maxY == bounds.triggerRect.maxY.rounded())
    #expect(bounds.warpTargetRect.minY > bounds.triggerRect.minY)
    #expect(bounds.warpTargetRect.maxY < bounds.triggerRect.maxY)

    // Cursor moving onto the top title bar (above maxY) clamps cleanly below triggerRect.maxY.
    let onTitleBar = NSPoint(x: 500.4, y: fractionalRect.maxY + 14.0)
    let warped = bounds.clampedWarpPoint(for: onTitleBar)
    #expect(warped != nil)
    #expect(warped!.x == 500.0)
    #expect(warped!.y == bounds.warpTargetRect.maxY)

    // CRITICAL INVARIANT: the warped destination itself must NEVER trigger another warp
    // (eliminates the infinite edge-freeze loop).
    #expect(bounds.clampedWarpPoint(for: warped!) == nil)

    // Points safely inside triggerRect require zero warps.
    #expect(bounds.clampedWarpPoint(for: NSPoint(x: 500.0, y: bounds.triggerRect.maxY)) == nil)
    #expect(bounds.clampedWarpPoint(for: NSPoint(x: 500.0, y: 500.0)) == nil)
}
