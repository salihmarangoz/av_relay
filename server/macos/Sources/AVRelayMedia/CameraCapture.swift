// SPDX-License-Identifier: GPL-3.0-or-later
// The camera, feeding libwebrtc's video source. Runs only between start()
// and stop(), so the camera light is on exactly while a client uses it.

import AVFoundation
import AVRelayCore
import CoreVideo
import WebRTC
import os

/// Where the video comes from.
public enum CameraSource: Sendable, Equatable {
    /// A capture device: by AVCaptureDevice.uniqueID, or nil for the built-in camera.
    case device(id: String?)
    /// Moving color bars, for tests: no hardware, no permission prompt.
    case testPattern
}

final class CameraCapture: NSObject, @unchecked Sendable {
    /// Called on the capture queue: the first frame arrived, or capture failed.
    var onCapturing: (() -> Void)?
    var onFailure: ((DeviceState.ErrorCode) -> Void)?

    private let videoSource: RTCVideoSource
    private let capturer: RTCVideoCapturer
    private let queue = DispatchQueue(label: "av-relay.camera")
    private let log = Logger(subsystem: "io.github.salihmarangoz.avrelay", category: "camera")
    private var source: CameraSource
    private var running = false
    private var deliveredSinceStart = false
    private var session: AVCaptureSession?
    private var timer: DispatchSourceTimer?
    private var pool: CVPixelBufferPool?
    private var frameIndex = 0
    private let counter = NSLock()
    private var _captured = 0
    /// Frames the camera has delivered so far (diagnostics).
    var framesCaptured: Int { counter.withLock { _captured } }
    /// The camera mode in use, e.g. "1280x720 at 30 fps max" (diagnostics).
    private(set) var activeMode: String?
    private var observers: [NSObjectProtocol] = []
    private var cadence = CadenceMeter()
    /// Counts starts, so a late notification or watchdog from an earlier
    /// start can't touch the current one.
    private var generation = 0
    /// How long a start may take to deliver its first frame.
    static let startTimeout: TimeInterval = 10

    init(videoSource: RTCVideoSource, source: CameraSource) {
        self.videoSource = videoSource
        capturer = RTCVideoCapturer(delegate: videoSource)
        self.source = source
    }

    /// The user-facing name of the camera a source uses (nil: it isn't there).
    static func deviceName(for source: CameraSource) -> String? {
        switch source {
        case .testPattern: return "Test pattern"
        case .device(let id): return captureDevice(id: id)?.localizedName
        }
    }

    /// The chosen camera, or the built-in one when none was chosen. A chosen
    /// camera that isn't connected is nil: never silently another one.
    static func captureDevice(id: String?) -> AVCaptureDevice? {
        if let id { return AVCaptureDevice(uniqueID: id) }
        let devices = AVCaptureDevice.DiscoverySession(deviceTypes: [.builtInWideAngleCamera, .external, .continuityCamera],
                                                       mediaType: .video, position: .unspecified).devices
        return devices.first { $0.transportType == Int32(bitPattern: kAudioDeviceTransportTypeBuiltIn) }
            ?? AVCaptureDevice.default(for: .video)
    }

    func setSource(_ newSource: CameraSource, format: VideoFormat) {
        queue.async { [self] in
            guard newSource != source else { return }
            source = newSource
            if running {
                stopCapture()
                deliveredSinceStart = false
                startCapture(format: format)
            }
        }
    }

    /// Starts capturing `format` (the camera's closest mode, scaled and
    /// cropped by libwebrtc to exactly this size).
    func start(format: VideoFormat) {
        queue.async { [self] in
            guard !running else { return }
            running = true
            deliveredSinceStart = false
            videoSource.adaptOutputFormat(toWidth: Int32(format.width), height: Int32(format.height), fps: Int32(format.fps))
            startCapture(format: format)
        }
    }

    func stop() {
        queue.async { [self] in
            guard running else { return }
            running = false
            stopCapture()
        }
    }

    /// How regularly frames arrived since the last call (diagnostics).
    func takeCadence() -> Cadence {
        queue.sync { cadence.take() }
    }

    /// Stops now and returns once the camera is closed (before sleep).
    func stopNow() {
        queue.sync {
            running = false
            stopCapture()
        }
    }

    // MARK: On `queue`

    private func startCapture(format: VideoFormat) {
        generation += 1
        let started = generation
        switch source {
        case .testPattern: startPattern(format: format)
        case .device(let id): startDevice(id: id, format: format, generation: started)
        }
        // A camera that never delivers (stuck, taken by another app) is a failure, not "starting" forever.
        queue.asyncAfter(deadline: .now() + Self.startTimeout) { [weak self] in
            guard let self, running, generation == started, !deliveredSinceStart else { return }
            fail(.failed)
        }
    }

    private func stopCapture() {
        timer?.cancel()
        timer = nil
        pool = nil
        observers.forEach(NotificationCenter.default.removeObserver)
        observers = []
        session?.stopRunning()
        session = nil
    }

    private func startDevice(id: String?, format: VideoFormat, generation started: Int) {
        guard let device = Self.captureDevice(id: id) else { return fail(.noDevice) }
        guard AVCaptureDevice.authorizationStatus(for: .video) == .authorized else { return fail(.permissionDenied) }
        let session = AVCaptureSession()
        session.beginConfiguration()
        let output = AVCaptureVideoDataOutput()
        output.videoSettings = [kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange]
        output.alwaysDiscardsLateVideoFrames = true
        output.setSampleBufferDelegate(self, queue: queue)
        do {
            let input = try AVCaptureDeviceInput(device: device)
            guard session.canAddInput(input), session.canAddOutput(output) else {
                session.commitConfiguration()
                return fail(.deviceBusy)
            }
            session.addInput(input)
            session.addOutput(output)
            try configure(device, for: format)
        } catch {
            session.commitConfiguration()
            log.error("camera input: \(error.localizedDescription, privacy: .public)")
            return fail(.deviceBusy)
        }
        session.commitConfiguration()
        observers.append(NotificationCenter.default.addObserver(forName: AVCaptureSession.runtimeErrorNotification,
                                                                object: session, queue: nil) { [weak self] note in
            let error = note.userInfo?[AVCaptureSessionErrorKey] as? Error
            self?.log.error("camera runtime error: \(error?.localizedDescription ?? "?", privacy: .public)")
            self?.failIfCurrent(started, .failed)
        })
        observers.append(NotificationCenter.default.addObserver(forName: AVCaptureDevice.wasDisconnectedNotification,
                                                                object: device, queue: nil) { [weak self] _ in
            self?.failIfCurrent(started, .noDevice)
        })
        self.session = session
        session.startRunning()
        log.info("camera \(device.localizedName, privacy: .public) on")
    }

    /// Picks the smallest mode at least as large as `format` that reaches
    /// its frame rate, preferring the same aspect ratio; else the largest.
    private func configure(_ device: AVCaptureDevice, for format: VideoFormat) throws {
        let wanted = Double(format.width) / Double(format.height)
        let candidates = device.formats.filter { f in
            f.videoSupportedFrameRateRanges.contains { $0.maxFrameRate >= Double(format.fps) - 0.5 }
        }
        func size(_ f: AVCaptureDevice.Format) -> CMVideoDimensions { CMVideoFormatDescriptionGetDimensions(f.formatDescription) }
        let bigEnough = candidates.filter { Int(size($0).width) >= format.width && Int(size($0).height) >= format.height }
        let chosen = bigEnough.min { a, b in
            let (sa, sb) = (size(a), size(b))
            let aspectA = abs(Double(sa.width) / Double(sa.height) - wanted), aspectB = abs(Double(sb.width) / Double(sb.height) - wanted)
            if abs(aspectA - aspectB) > 0.01 { return aspectA < aspectB }
            return Int(sa.width) * Int(sa.height) < Int(sb.width) * Int(sb.height)
        } ?? candidates.max { Int(size($0).width) * Int(size($0).height) < Int(size($1).width) * Int(size($1).height) }
        guard let chosen else { return }
        try device.lockForConfiguration()
        defer { device.unlockForConfiguration() }
        device.activeFormat = chosen
        if let range = chosen.videoSupportedFrameRateRanges.first(where: { $0.maxFrameRate >= Double(format.fps) - 0.5 }) {
            let fps = min(Double(format.fps), range.maxFrameRate)
            device.activeVideoMinFrameDuration = CMTime(value: 1000, timescale: CMTimeScale(fps * 1000))
        }
        let dims = size(chosen)
        let maxFPS = chosen.videoSupportedFrameRateRanges.map(\.maxFrameRate).max() ?? 0
        activeMode = "\(dims.width)x\(dims.height), up to \(Int(maxFPS)) fps"
        log.info("camera mode \(dims.width)x\(dims.height) for \(format.width)x\(format.height)@\(format.fps)")
    }

    private func startPattern(format: VideoFormat) {
        let attributes: [String: Any] = [
            kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
            kCVPixelBufferWidthKey as String: format.width,
            kCVPixelBufferHeightKey as String: format.height,
            kCVPixelBufferIOSurfacePropertiesKey as String: [String: Any](),
        ]
        CVPixelBufferPoolCreate(nil, nil, attributes as CFDictionary, &pool)
        let timer = DispatchSource.makeTimerSource(queue: queue)
        timer.schedule(deadline: .now(), repeating: .nanoseconds(1_000_000_000 / max(format.fps, 1)), leeway: .milliseconds(1))
        timer.setEventHandler { [self] in
            guard let pool else { return }
            var buffer: CVPixelBuffer?
            CVPixelBufferPoolCreatePixelBuffer(nil, pool, &buffer)
            guard let buffer else { return }
            Self.drawBars(buffer, offset: frameIndex * 8)
            frameIndex += 1
            deliver(buffer, timeNs: Int64(Double(mach_absolute_time()) * Self.nanosecondsPerTick))
        }
        self.timer = timer
        timer.resume()
    }

    private static let nanosecondsPerTick: Double = {
        var info = mach_timebase_info_data_t()
        mach_timebase_info(&info)
        return Double(info.numer) / Double(info.denom)
    }()

    /// Vertical bars that move, in NV12 (limited range), so frozen video
    /// shows. One row is drawn and copied down: cheap even at 1080p.
    private static func drawBars(_ buffer: CVPixelBuffer, offset: Int) {
        CVPixelBufferLockBaseAddress(buffer, [])
        defer { CVPixelBufferUnlockBaseAddress(buffer, []) }
        let width = CVPixelBufferGetWidthOfPlane(buffer, 0), height = CVPixelBufferGetHeightOfPlane(buffer, 0)
        let bars: [(UInt8, UInt8, UInt8)] = [(180, 128, 128), (145, 54, 34), (41, 240, 110), (82, 90, 240)]
        var lumaRow = [UInt8](repeating: 0, count: width)
        var chromaRow = [UInt8](repeating: 0, count: width)
        for col in 0..<width {
            let bar = bars[((col + offset) * bars.count / width) % bars.count]
            lumaRow[col] = bar.0
            chromaRow[col] = col % 2 == 0 ? bar.1 : bars[((col - 1 + offset) * bars.count / width) % bars.count].2
        }
        let y = CVPixelBufferGetBaseAddressOfPlane(buffer, 0)!
        let yStride = CVPixelBufferGetBytesPerRowOfPlane(buffer, 0)
        let uv = CVPixelBufferGetBaseAddressOfPlane(buffer, 1)!
        let uvStride = CVPixelBufferGetBytesPerRowOfPlane(buffer, 1)
        lumaRow.withUnsafeBytes { row in
            for r in 0..<height { (y + r * yStride).copyMemory(from: row.baseAddress!, byteCount: width) }
        }
        chromaRow.withUnsafeBytes { row in
            for r in 0..<(height / 2) { (uv + r * uvStride).copyMemory(from: row.baseAddress!, byteCount: width) }
        }
    }

    private func failIfCurrent(_ started: Int, _ error: DeviceState.ErrorCode) {
        queue.async { [self] in
            guard running, generation == started else { return }
            fail(error)
        }
    }

    private func fail(_ error: DeviceState.ErrorCode) {
        log.error("camera failed: \(error.rawValue, privacy: .public)")
        onFailure?(error)
    }

    private func deliver(_ pixelBuffer: CVPixelBuffer, timeNs: Int64) {
        guard running else { return }
        counter.withLock { _captured += 1 }
        cadence.tick()
        if !deliveredSinceStart {
            deliveredSinceStart = true
            onCapturing?()
        }
        let frame = RTCVideoFrame(buffer: RTCCVPixelBuffer(pixelBuffer: pixelBuffer), rotation: ._0, timeStampNs: timeNs)
        videoSource.capturer(capturer, didCapture: frame)
    }
}

extension CameraCapture: AVCaptureVideoDataOutputSampleBufferDelegate {
    func captureOutput(_ output: AVCaptureOutput, didOutput sampleBuffer: CMSampleBuffer, from connection: AVCaptureConnection) {
        guard let pixelBuffer = CMSampleBufferGetImageBuffer(sampleBuffer) else { return }
        let pts = CMSampleBufferGetPresentationTimeStamp(sampleBuffer)
        deliver(pixelBuffer, timeNs: Int64(CMTimeGetSeconds(pts) * 1_000_000_000))
    }
}
