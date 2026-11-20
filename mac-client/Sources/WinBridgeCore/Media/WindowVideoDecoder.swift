import CoreGraphics
import CoreMedia
import CoreVideo
import Foundation
import ImageIO
import VideoToolbox

enum DecodedImage {
    /// Zero-copy GPU-backed pixel buffer (from VideoToolbox). Retaining the CVPixelBuffer
    /// keeps its IOSurface pinned while displayed in CALayer.contents.
    case pixelBuffer(CVPixelBuffer)
    case cgImage(CGImage)

    var layerContents: Any? {
        switch self {
        case .pixelBuffer(let pb):
            if let surface = CVPixelBufferGetIOSurface(pb)?.takeUnretainedValue() {
                return surface
            }
            var cg: CGImage?
            VTCreateCGImageFromCVPixelBuffer(pb, options: nil, imageOut: &cg)
            return cg
        case .cgImage(let img):
            return img
        }
    }
}

/// Per-window decoder running on its own serial queue.
/// Supports H.264 Annex-B (VideoToolbox hardware), JPEG, and RAW_BGRA.
final class WindowVideoDecoder {
    let windowId: UInt64
    private let queue: DispatchQueue
    private var formatDesc: CMVideoFormatDescription?
    private var session: VTDecompressionSession?
    private var currentSPS: Data?
    private var currentPPS: Data?
    private var waitingForKeyframe = true

    var onDecodedFrame: ((DecodedImage, UInt16, UInt16) -> Void)?
    var onRequestKeyframe: (() -> Void)?

    init(windowId: UInt64) {
        self.windowId = windowId
        self.queue = DispatchQueue(label: String(format: "winbridge.decode.%llx", windowId), qos: .userInteractive)
    }

    deinit {
        if let session {
            VTDecompressionSessionInvalidate(session)
        }
    }

    func decode(_ frame: MediaFrame) {
        if frame.header.flags.contains(.endOfStream) || frame.payload.isEmpty {
            return
        }
        queue.async { [weak self] in
            self?.decodeSync(frame)
        }
    }

    func invalidate() {
        queue.async { [weak self] in
            self?.tearDownSession()
        }
    }

    private func tearDownSession() {
        if let session {
            VTDecompressionSessionInvalidate(session)
            self.session = nil
        }
        formatDesc = nil
        currentSPS = nil
        currentPPS = nil
        waitingForKeyframe = true
    }

    private func decodeSync(_ frame: MediaFrame) {
        let w = frame.header.width
        let h = frame.header.height
        switch frame.header.codec {
        case .h264AnnexB:
            decodeH264(frame.payload, width: w, height: h, flags: frame.header.flags)
        case .jpeg:
            if let img = decodeJPEG(frame.payload) {
                onDecodedFrame?(.cgImage(img), w, h)
            }
        case .rawBgra:
            if let img = decodeRawBGRA(frame.payload, width: Int(w), height: Int(h)) {
                onDecodedFrame?(.cgImage(img), w, h)
            }
        default:
            break
        }
    }

    // MARK: - H.264 VideoToolbox

    private func decodeH264(_ payload: Data, width: UInt16, height: UInt16, flags: MediaFrameFlags) {
        let au = AnnexBParser.parseH264(payload)

        if let sps = au.sps, let pps = au.pps {
            if sps != currentSPS || pps != currentPPS || session == nil {
                if !configureH264Session(sps: sps, pps: pps) {
                    waitingForKeyframe = true
                    onRequestKeyframe?()
                    return
                }
            }
        }

        if waitingForKeyframe {
            guard au.isIDR || flags.contains(.keyframe), session != nil else {
                return
            }
            waitingForKeyframe = false
        }

        guard let session, let formatDesc, !au.avccPayload.isEmpty else {
            return
        }

        var blockBuffer: CMBlockBuffer?
        let len = au.avccPayload.count
        let statusAlloc = CMBlockBufferCreateWithMemoryBlock(
            allocator: kCFAllocatorDefault,
            memoryBlock: nil,
            blockLength: len,
            blockAllocator: kCFAllocatorDefault,
            customBlockSource: nil,
            offsetToData: 0,
            dataLength: len,
            flags: 0,
            blockBufferOut: &blockBuffer
        )
        guard statusAlloc == kCMBlockBufferNoErr, let blockBuffer else {
            return
        }

        let statusReplace = au.avccPayload.withUnsafeBytes { raw -> OSStatus in
            guard let base = raw.baseAddress else { return -1 }
            return CMBlockBufferReplaceDataBytes(
                with: base,
                blockBuffer: blockBuffer,
                offsetIntoDestination: 0,
                dataLength: len
            )
        }
        guard statusReplace == kCMBlockBufferNoErr else { return }

        var sampleBuffer: CMSampleBuffer?
        var sampleSize = len
        let statusSample = CMSampleBufferCreateReady(
            allocator: kCFAllocatorDefault,
            dataBuffer: blockBuffer,
            formatDescription: formatDesc,
            sampleCount: 1,
            sampleTimingEntryCount: 0,
            sampleTimingArray: nil,
            sampleSizeEntryCount: 1,
            sampleSizeArray: &sampleSize,
            sampleBufferOut: &sampleBuffer
        )
        guard statusSample == noErr, let sampleBuffer else { return }

        if let attachments = CMSampleBufferGetSampleAttachmentsArray(sampleBuffer, createIfNecessary: true) {
            let dict = unsafeBitCast(CFArrayGetValueAtIndex(attachments, 0), to: CFMutableDictionary.self)
            CFDictionarySetValue(
                dict,
                Unmanaged.passUnretained(kCMSampleAttachmentKey_DisplayImmediately).toOpaque(),
                Unmanaged.passUnretained(kCFBooleanTrue).toOpaque()
            )
        }

        // Baseline / 0 B-frames: synchronous decode finishes immediately with zero reordering delay.
        var decodeFailed = false
        let decodeStatus = VTDecompressionSessionDecodeFrame(
            session,
            sampleBuffer: sampleBuffer,
            flags: [],
            infoFlagsOut: nil
        ) { [weak self] status, _, imageBuffer, _, _ in
            guard let self else { return }
            if status != noErr {
                decodeFailed = true
                return
            }
            if let pixelBuffer = imageBuffer {
                self.onDecodedFrame?(.pixelBuffer(pixelBuffer), width, height)
            }
        }

        if decodeStatus != noErr || decodeFailed {
            waitingForKeyframe = true
            onRequestKeyframe?()
        }
    }

    private func configureH264Session(sps: Data, pps: Data) -> Bool {
        if let old = session {
            VTDecompressionSessionInvalidate(old)
            session = nil
        }
        formatDesc = nil

        var newFormatDesc: CMVideoFormatDescription?
        let statusDesc: OSStatus = sps.withUnsafeBytes { spsRaw in
            pps.withUnsafeBytes { ppsRaw in
                guard let spsPtr = spsRaw.bindMemory(to: UInt8.self).baseAddress,
                      let ppsPtr = ppsRaw.bindMemory(to: UInt8.self).baseAddress else {
                    return -1
                }
                let pointers: [UnsafePointer<UInt8>] = [spsPtr, ppsPtr]
                let sizes: [Int] = [sps.count, pps.count]
                return CMVideoFormatDescriptionCreateFromH264ParameterSets(
                    allocator: kCFAllocatorDefault,
                    parameterSetCount: 2,
                    parameterSetPointers: pointers,
                    parameterSetSizes: sizes,
                    nalUnitHeaderLength: 4,
                    formatDescriptionOut: &newFormatDesc
                )
            }
        }
        guard statusDesc == noErr, let newFormatDesc else {
            return false
        }

        let decoderSpec: [CFString: Any] = [
            kVTVideoDecoderSpecification_EnableHardwareAcceleratedVideoDecoder: true,
        ]
        let imageAttrs: [CFString: Any] = [
            kCVPixelBufferPixelFormatTypeKey: kCVPixelFormatType_32BGRA,
            kCVPixelBufferIOSurfacePropertiesKey: [:] as [CFString: Any],
            kCVPixelBufferMetalCompatibilityKey: true,
        ]

        var newSession: VTDecompressionSession?
        let statusSession = VTDecompressionSessionCreate(
            allocator: kCFAllocatorDefault,
            formatDescription: newFormatDesc,
            decoderSpecification: decoderSpec as CFDictionary,
            imageBufferAttributes: imageAttrs as CFDictionary,
            outputCallback: nil,
            decompressionSessionOut: &newSession
        )
        guard statusSession == noErr, let newSession else {
            return false
        }

        VTSessionSetProperty(newSession, key: kVTDecompressionPropertyKey_RealTime, value: kCFBooleanTrue)
        self.formatDesc = newFormatDesc
        self.session = newSession
        self.currentSPS = sps
        self.currentPPS = pps
        return true
    }

    // MARK: - JPEG & RAW_BGRA

    private func decodeJPEG(_ data: Data) -> CGImage? {
        guard let source = CGImageSourceCreateWithData(data as CFData, nil) else { return nil }
        return CGImageSourceCreateImageAtIndex(source, 0, nil)
    }

    private func decodeRawBGRA(_ data: Data, width: Int, height: Int) -> CGImage? {
        guard width > 0, height > 0 else { return nil }
        let bytesPerRow = width * 4
        guard data.count >= bytesPerRow * height else { return nil }
        guard let provider = CGDataProvider(data: data as CFData) else { return nil }
        let bitmapInfo = CGBitmapInfo(rawValue: CGBitmapInfo.byteOrder32Little.rawValue | CGImageAlphaInfo.premultipliedFirst.rawValue)
        return CGImage(
            width: width,
            height: height,
            bitsPerComponent: 8,
            bitsPerPixel: 32,
            bytesPerRow: bytesPerRow,
            space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: bitmapInfo,
            provider: provider,
            decode: nil,
            shouldInterpolate: true,
            intent: .defaultIntent
        )
    }
}
