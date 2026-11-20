import AVFoundation
import Foundation

/// Bounded stereo Float32 ring buffer with automatic latency-drift trimming.
/// Thread-safe via `NSLock`; safe to push from the Media TCP thread and pull from CoreAudio IO.
final class StereoAudioRingBuffer {
    private let lock = NSLock()
    private var left: [Float]
    private var right: [Float]
    private let capacity: Int
    private let highWaterFrames: Int
    private let trimTargetFrames: Int
    private var readIndex = 0
    private var writeIndex = 0
    private var count = 0
    private var gain: Float = 0.85

    init(sampleRate: Int, maxDurationMs: Int = 220, highWaterMs: Int = 95, trimTargetMs: Int = 28) {
        let cap = max(1024, sampleRate * maxDurationMs / 1000)
        self.capacity = cap
        self.highWaterFrames = min(cap, max(256, sampleRate * highWaterMs / 1000))
        self.trimTargetFrames = min(self.highWaterFrames, max(128, sampleRate * trimTargetMs / 1000))
        self.left = [Float](repeating: 0, count: cap)
        self.right = [Float](repeating: 0, count: cap)
    }

    var bufferedFrames: Int {
        lock.lock()
        defer { lock.unlock() }
        return count
    }

    func setVolume(_ value: Float) {
        lock.lock()
        gain = max(0, min(1, value))
        lock.unlock()
    }

    func clear() {
        lock.lock()
        readIndex = 0
        writeIndex = 0
        count = 0
        lock.unlock()
    }

    /// Appends interleaved signed 16-bit little-endian PCM (1 or 2 channels).
    func pushPcmS16Le(_ data: Data, channels: Int) {
        guard channels >= 1 else { return }
        let frameBytes = channels * 2
        let frameCount = data.count / frameBytes
        guard frameCount > 0 else { return }

        lock.lock()
        defer { lock.unlock() }

        let invScale: Float = 1.0 / 32768.0
        data.withUnsafeBytes { (raw: UnsafeRawBufferPointer) in
            for i in 0..<frameCount {
                let off = i * frameBytes
                let lInt = Int16(bitPattern: UInt16(raw[off]) | (UInt16(raw[off + 1]) << 8))
                let rInt: Int16
                if channels >= 2 {
                    rInt = Int16(bitPattern: UInt16(raw[off + 2]) | (UInt16(raw[off + 3]) << 8))
                } else {
                    rInt = lInt
                }
                left[writeIndex] = Float(lInt) * invScale
                right[writeIndex] = Float(rInt) * invScale
                writeIndex += 1
                if writeIndex == capacity { writeIndex = 0 }
                if count < capacity {
                    count += 1
                } else {
                    readIndex += 1
                    if readIndex == capacity { readIndex = 0 }
                }
            }
        }

        // Prevent latency drift if a burst of packets arrives after jitter.
        if count > highWaterFrames {
            let drop = count - trimTargetFrames
            readIndex = (readIndex + drop) % capacity
            count -= drop
        }
    }

    /// Pulls `frameCount` stereo frames into non-interleaved L/R output buffers, zero-filling underruns.
    func pull(intoLeft outL: UnsafeMutablePointer<Float>, right outR: UnsafeMutablePointer<Float>?, frameCount: Int) {
        lock.lock()
        let vol = gain
        let avail = min(count, frameCount)
        for i in 0..<avail {
            outL[i] = left[readIndex] * vol
            outR?[i] = right[readIndex] * vol
            readIndex += 1
            if readIndex == capacity { readIndex = 0 }
        }
        count -= avail
        lock.unlock()

        if avail < frameCount {
            let rem = frameCount - avail
            outL.advanced(by: avail).initialize(repeating: 0, count: rem)
            outR?.advanced(by: avail).initialize(repeating: 0, count: rem)
        }
    }
}

/// Real-time low-latency macOS audio player backed by `AVAudioEngine` + `AVAudioSourceNode`.
/// Safe to call `enqueue(...)` directly from the Media TCP thread without main-thread hops.
final class AudioPlayer {
    private let stateLock = NSLock()
    private var engine: AVAudioEngine?
    private var sourceNode: AVAudioSourceNode?
    private var ringBuffer = StereoAudioRingBuffer(sampleRate: 48_000)
    private var currentSampleRate: Int = 48_000
    private var enabled: Bool = false
    private var volumeValue: Float = 0.85

    var volume: Float {
        get {
            stateLock.lock()
            defer { stateLock.unlock() }
            return volumeValue
        }
        set {
            let clamped = max(0, min(1, newValue))
            stateLock.lock()
            volumeValue = clamped
            let buf = ringBuffer
            stateLock.unlock()
            buf.setVolume(clamped)
        }
    }

    func setEnabled(_ active: Bool) {
        stateLock.lock()
        enabled = active
        if !active {
            stopEngineLocked()
        }
        stateLock.unlock()
    }

    func stop() {
        setEnabled(false)
    }

    func enqueue(frame: MediaFrame) {
        let sampleRate = Int(frame.header.width)
        let channels = max(1, Int(frame.header.height))
        guard sampleRate >= 8_000, sampleRate <= 65_535 else { return }

        stateLock.lock()
        guard enabled else {
            stateLock.unlock()
            return
        }
        if engine == nil || currentSampleRate != sampleRate {
            startEngineLocked(sampleRate: sampleRate)
        }
        let buf = ringBuffer
        stateLock.unlock()

        buf.pushPcmS16Le(frame.payload, channels: channels)
    }

    private func startEngineLocked(sampleRate: Int) {
        stopEngineLocked()

        let buf = StereoAudioRingBuffer(sampleRate: sampleRate)
        buf.setVolume(volumeValue)
        self.ringBuffer = buf
        self.currentSampleRate = sampleRate

        guard let format = AVAudioFormat(standardFormatWithSampleRate: Double(sampleRate), channels: 2) else {
            return
        }

        let engine = AVAudioEngine()
        let node = AVAudioSourceNode(format: format) { _, _, frameCount, audioBufferList -> OSStatus in
            let abl = UnsafeMutableAudioBufferListPointer(audioBufferList)
            let frames = Int(frameCount)
            if abl.count >= 2,
               let lPtr = abl[0].mData?.assumingMemoryBound(to: Float.self),
               let rPtr = abl[1].mData?.assumingMemoryBound(to: Float.self) {
                buf.pull(intoLeft: lPtr, right: rPtr, frameCount: frames)
            } else if abl.count == 1,
                      let lPtr = abl[0].mData?.assumingMemoryBound(to: Float.self) {
                buf.pull(intoLeft: lPtr, right: nil, frameCount: frames)
            }
            return noErr
        }

        engine.attach(node)
        engine.connect(node, to: engine.mainMixerNode, format: format)
        do {
            try engine.start()
            self.engine = engine
            self.sourceNode = node
        } catch {
            engine.stop()
        }
    }

    private func stopEngineLocked() {
        ringBuffer.clear()
        if let engine {
            engine.stop()
            if let sourceNode {
                engine.detach(sourceNode)
            }
        }
        sourceNode = nil
        engine = nil
    }
}
