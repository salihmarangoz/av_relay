// SPDX-License-Identifier: GPL-3.0-or-later
// The microphone as libwebrtc's audio device. libwebrtc starts recording
// when an audio stream starts sending and stops when the last one stops
// (MediaEngine pauses the audio sender while the client doesn't need the
// mic), so the microphone is open exactly while it's wanted, and macOS's
// microphone indicator shows it. We pick the device ourselves: the built-in
// microphone unless the user chose another, never silently whatever the
// system default happens to be. Nothing is ever played out.

import AVFoundation
import AVRelayCore
import CWebRTCSupport
import WebRTC
import os

/// Where the audio comes from.
public enum MicrophoneSource: Sendable, Equatable {
    /// A capture device: by AVCaptureDevice.uniqueID, or nil for the built-in microphone.
    case device(id: String?)
    /// A 440 Hz tone, for tests: no hardware, no permission prompt.
    case testTone
}

final class MicrophoneDevice: NSObject, RTCAudioDevice, @unchecked Sendable {
    static let sampleRate = 48_000.0

    /// Called on the device's queue: the first audio arrived, or capture failed.
    var onCapturing: (() -> Void)?
    var onFailure: ((DeviceState.ErrorCode) -> Void)?

    private let queue = DispatchQueue(label: "av-relay.microphone")
    private let log = Logger(subsystem: "io.github.salihmarangoz.avrelay", category: "microphone")
    private var source: MicrophoneSource
    private var delegate: RTCAudioDeviceDelegate?
    private var recording = false
    private var deliveredSinceStart = false
    private var sampleTime = 0.0
    private var session: AVCaptureSession?
    private var output: AVCaptureAudioDataOutput?
    private var timer: DispatchSourceTimer?
    private var phase = 0.0

    init(source: MicrophoneSource) {
        self.source = source
    }

    /// The name of the device that will be used, for media-state.
    var deviceName: String? {
        switch source {
        case .testTone: return "Test tone"
        case .device(let id): return Self.captureDevice(id: id)?.localizedName
        }
    }

    /// Switches the source; takes effect at the next start (or now, if recording).
    func setSource(_ newSource: MicrophoneSource) {
        queue.async { [self] in
            guard newSource != source else { return }
            source = newSource
            if recording {
                stopCapture()
                startCapture()
            }
        }
    }

    static func captureDevice(id: String?) -> AVCaptureDevice? {
        if let id, let device = AVCaptureDevice(uniqueID: id) { return device }
        let devices = AVCaptureDevice.DiscoverySession(deviceTypes: [.microphone], mediaType: .audio,
                                                       position: .unspecified).devices
        return devices.first { $0.transportType == Int32(bitPattern: kAudioDeviceTransportTypeBuiltIn) }
            ?? AVCaptureDevice.default(for: .audio)
    }

    // MARK: RTCAudioDevice (called by libwebrtc on its threads)

    var deviceInputSampleRate: Double { Self.sampleRate }
    var inputIOBufferDuration: TimeInterval { 0.01 }
    var inputNumberOfChannels: Int { 1 }
    var inputLatency: TimeInterval { 0.01 }
    var deviceOutputSampleRate: Double { Self.sampleRate }
    var outputIOBufferDuration: TimeInterval { 0.01 }
    var outputNumberOfChannels: Int { 1 }
    var outputLatency: TimeInterval { 0 }

    var isInitialized: Bool { queue.sync { delegate != nil } }

    func initialize(with delegate: RTCAudioDeviceDelegate) -> Bool {
        queue.sync { self.delegate = delegate }
        return true
    }

    func terminateDevice() -> Bool {
        queue.sync {
            stopCapture()
            recording = false
            delegate = nil
        }
        return true
    }

    // Playout: AV Relay only sends.
    var isPlayoutInitialized: Bool { true }
    func initializePlayout() -> Bool { true }
    var isPlaying: Bool { false }
    func startPlayout() -> Bool { true }
    func stopPlayout() -> Bool { true }

    var isRecordingInitialized: Bool { true }
    func initializeRecording() -> Bool { true }
    var isRecording: Bool { queue.sync { recording } }

    func startRecording() -> Bool {
        queue.sync {
            guard !recording else { return }
            recording = true
            deliveredSinceStart = false
            startCapture()
        }
        log.info("recording started")
        return true
    }

    func stopRecording() -> Bool {
        queue.sync {
            recording = false
            stopCapture()
        }
        log.info("recording stopped")
        return true
    }

    // MARK: Capture (on `queue`)

    private func startCapture() {
        switch source {
        case .testTone: startTone()
        case .device(let id): startDevice(id: id)
        }
    }

    private func stopCapture() {
        timer?.cancel()
        timer = nil
        output?.setSampleBufferDelegate(nil, queue: nil)
        session?.stopRunning()
        session = nil
        output = nil
    }

    private func startDevice(id: String?) {
        guard let device = Self.captureDevice(id: id) else { return fail(.noDevice) }
        guard AVCaptureDevice.authorizationStatus(for: .audio) == .authorized else { return fail(.permissionDenied) }
        let session = AVCaptureSession()
        let output = AVCaptureAudioDataOutput()
        do {
            let input = try AVCaptureDeviceInput(device: device)
            guard session.canAddInput(input), session.canAddOutput(output) else { return fail(.deviceBusy) }
            session.addInput(input)
            session.addOutput(output)
        } catch {
            log.error("microphone input: \(error.localizedDescription, privacy: .public)")
            return fail(.deviceBusy)
        }
        // AVFoundation converts to what libwebrtc takes: 48 kHz, mono, 16-bit, interleaved.
        output.audioSettings = [
            AVFormatIDKey: kAudioFormatLinearPCM,
            AVSampleRateKey: Self.sampleRate,
            AVNumberOfChannelsKey: 1,
            AVLinearPCMBitDepthKey: 16,
            AVLinearPCMIsFloatKey: false,
            AVLinearPCMIsBigEndianKey: false,
            AVLinearPCMIsNonInterleaved: false,
        ]
        output.setSampleBufferDelegate(self, queue: queue)
        self.session = session
        self.output = output
        session.startRunning()
        log.info("microphone \(device.localizedName, privacy: .public) on")
    }

    private func startTone() {
        let frames = Int(Self.sampleRate / 100)
        var buffer = [Int16](repeating: 0, count: frames)
        let timer = DispatchSource.makeTimerSource(queue: queue)
        timer.schedule(deadline: .now(), repeating: .milliseconds(10), leeway: .milliseconds(1))
        timer.setEventHandler { [self] in
            for i in 0..<frames {
                buffer[i] = Int16(sin(phase) * 3000)
                phase += 2 * .pi * 440 / Self.sampleRate
            }
            phase = phase.truncatingRemainder(dividingBy: 2 * .pi)
            buffer.withUnsafeMutableBufferPointer { deliver($0.baseAddress!, frames: frames, hostTime: mach_absolute_time()) }
        }
        self.timer = timer
        timer.resume()
    }

    private func fail(_ error: DeviceState.ErrorCode) {
        log.error("microphone failed: \(error.rawValue, privacy: .public)")
        onFailure?(error)
    }

    /// Hands 10-ms-or-so chunks to libwebrtc (it rebuffers to 10 ms itself).
    private func deliver(_ samples: UnsafeMutablePointer<Int16>, frames: Int, hostTime: UInt64) {
        guard recording, let delegate else { return }
        if !deliveredSinceStart {
            deliveredSinceStart = true
            onCapturing?()
        }
        var flags = AudioUnitRenderActionFlags()
        var timestamp = AudioTimeStamp()
        timestamp.mFlags = [.hostTimeValid, .sampleTimeValid]
        timestamp.mHostTime = hostTime
        timestamp.mSampleTime = sampleTime
        sampleTime += Double(frames)
        var list = AudioBufferList(mNumberBuffers: 1, mBuffers: AudioBuffer(
            mNumberChannels: 1, mDataByteSize: UInt32(frames * MemoryLayout<Int16>.size), mData: samples))
        _ = delegate.deliverRecordedData(&flags, &timestamp, 1, UInt32(frames), &list, nil, nil)
    }
}

extension MicrophoneDevice: AVCaptureAudioDataOutputSampleBufferDelegate {
    func captureOutput(_ output: AVCaptureOutput, didOutput sampleBuffer: CMSampleBuffer, from connection: AVCaptureConnection) {
        let frames = CMSampleBufferGetNumSamples(sampleBuffer)
        guard frames > 0 else { return }
        var blockBuffer: CMBlockBuffer?
        var list = AudioBufferList()
        let status = CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
            sampleBuffer, bufferListSizeNeededOut: nil, bufferListOut: &list, bufferListSize: MemoryLayout<AudioBufferList>.size,
            blockBufferAllocator: nil, blockBufferMemoryAllocator: nil, flags: 0, blockBufferOut: &blockBuffer)
        guard status == noErr, let data = list.mBuffers.mData else { return }
        let pts = CMSampleBufferGetPresentationTimeStamp(sampleBuffer)
        let hostTime = pts.isValid ? CMClockConvertHostTimeToSystemUnits(pts) : mach_absolute_time()
        deliver(data.assumingMemoryBound(to: Int16.self), frames: frames, hostTime: hostTime)
    }
}
