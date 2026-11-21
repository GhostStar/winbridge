import AVFoundation
import Foundation

/// Captures the Mac microphone as 48 kHz stereo s16le and emits ~20 ms chunks.
final class MicUplink {
    private let engine = AVAudioEngine()
    private var converter: AVAudioConverter?
    private let targetFormat: AVAudioFormat
    private var pending = Data()
    private let lock = NSLock()
    var onChunk: ((Data) -> Void)?
    private(set) var running = false

    init() {
        targetFormat = AVAudioFormat(
            commonFormat: .pcmFormatInt16,
            sampleRate: 48_000,
            channels: 2,
            interleaved: true
        )!
    }

    func requestPermission(_ done: @escaping (Bool) -> Void) {
        AVCaptureDevice.requestAccess(for: .audio, completionHandler: done)
    }

    func start() throws {
        guard !running else { return }
        let input = engine.inputNode
        let hw = input.outputFormat(forBus: 0)
        guard hw.sampleRate > 0, hw.channelCount > 0 else {
            throw NSError(
                domain: "WinBridge",
                code: 1,
                userInfo: [NSLocalizedDescriptionKey: "没有可用的麦克风输入"]
            )
        }
        guard let converter = AVAudioConverter(from: hw, to: targetFormat) else {
            throw NSError(
                domain: "WinBridge",
                code: 2,
                userInfo: [NSLocalizedDescriptionKey: "无法把麦克风转成 48 kHz"]
            )
        }
        self.converter = converter
        input.removeTap(onBus: 0)
        input.installTap(onBus: 0, bufferSize: 960, format: hw) { [weak self] buffer, _ in
            self?.consume(buffer)
        }
        engine.prepare()
        try engine.start()
        running = true
    }

    func stop() {
        if running {
            engine.inputNode.removeTap(onBus: 0)
            engine.stop()
        }
        running = false
        lock.lock()
        pending.removeAll(keepingCapacity: false)
        lock.unlock()
    }

    private func consume(_ buffer: AVAudioPCMBuffer) {
        guard let converter,
              let out = AVAudioPCMBuffer(pcmFormat: targetFormat, frameCapacity: 4096) else { return }
        var error: NSError?
        var consumed = false
        let status = converter.convert(to: out, error: &error) { _, statusOut in
            if consumed {
                statusOut.pointee = .noDataNow
                return nil
            }
            consumed = true
            statusOut.pointee = .haveData
            return buffer
        }
        guard error == nil, status != .error, out.frameLength > 0 else { return }
        let list = UnsafeMutableAudioBufferListPointer(out.mutableAudioBufferList)
        guard let first = list.first, let base = first.mData else { return }
        let bytes = Data(bytes: base, count: Int(first.mDataByteSize))
        lock.lock()
        pending.append(bytes)
        let packet = 960 * 2 * 2
        while pending.count >= packet {
            let chunk = pending.prefix(packet)
            let data = Data(chunk)
            pending.removeFirst(packet)
            lock.unlock()
            onChunk?(data)
            lock.lock()
        }
        lock.unlock()
    }
}
