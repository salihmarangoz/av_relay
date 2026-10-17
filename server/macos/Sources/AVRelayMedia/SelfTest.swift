// SPDX-License-Identifier: GPL-3.0-or-later
// Runs the real camera and microphone through the whole media path, inside
// this process: capture, encode, WebRTC, decode. Only an app bundle may
// open the devices, so this is how the camera and microphone get tested
// (the app's --self-test; see server/macos/README.md).

import AVRelayCore
import Foundation

public enum SelfTest {
    public struct Report: Sendable {
        public var lines: [String] = []
        public var passed = true

        mutating func check(_ ok: Bool, _ what: String) {
            lines.append("\(ok ? "ok  " : "FAIL") \(what)")
            if !ok { passed = false }
        }

        mutating func note(_ what: String) { lines.append("     \(what)") }

        public var text: String { (lines + [passed ? "PASSED" : "FAILED"]).joined(separator: "\n") + "\n" }
    }

    public static func run(camera: CameraSource = .device(id: nil), microphone: MicrophoneSource = .device(id: nil),
                           format: VideoFormat = VideoFormat(width: 1280, height: 720, fps: 30)) async -> Report {
        var report = Report()
        var settings = MediaEngine.Settings()
        settings.camera = camera
        settings.microphone = microphone
        settings.linger = 0.3
        let engine = MediaEngine(settings: settings)
        let client = LoopbackClient(engine: engine)
        defer {
            engine.end(channel: client)
            client.peer.close()
        }
        engine.start(channel: client, hello: Hello(name: "self-test", app: "self-test", video: format))
        report.check(await waitUntil(10) { client.connected }, "WebRTC connected in this process")

        engine.receive(.media(video: true, audio: true), from: client)
        // Generous: the first run waits for the permission prompts.
        let on = await waitUntil(90) { client.lastState().map { $0.video != .starting && $0.audio != .starting && $0.video != .off } == true }
        if case let .mediaState(video, audio)? = client.states.last {
            report.note("camera: \(video.device ?? "-"), \(video.state)\(video.error.map { " (\($0.rawValue))" } ?? "")")
            report.note("microphone: \(audio.device ?? "-"), \(audio.state)\(audio.error.map { " (\($0.rawValue))" } ?? "")")
        }
        report.check(on && client.lastState()?.video == .on && client.lastState()?.audio == .on, "camera and microphone on")

        _ = await waitUntil(10) { client.frames.count > 30 }
        let start = client.frames.count
        let capturedStart = engine.cameraFramesCaptured
        let audioStart = await client.inbound("audio").packets
        try? await Task.sleep(for: .seconds(3))
        let fps = Double(client.frames.count - start) / 3
        let cameraFPS = Double(engine.cameraFramesCaptured - capturedStart) / 3
        report.note(String(format: "camera mode %@; the camera delivered %.1f fps", engine.cameraMode ?? "?", cameraFPS))
        let packets = await client.inbound("audio").packets - audioStart
        let stats = await engine.statistics()
        let source = stats?.statistics.values.first { $0.type == "media-source" && $0.values["kind"] as? String == "audio" }
        let level = (source?.values["audioLevel"] as? NSNumber)?.doubleValue ?? 0
        let outbound = stats?.statistics.values.first { $0.type == "outbound-rtp" && $0.values["kind"] as? String == "video" }
        // In low light a camera lowers its frame rate to expose longer: that's the camera's
        // choice, not a fault here. What must hold is that every captured frame gets through.
        report.check(fps >= cameraFPS * 0.9 - 1, String(format: "video: %.1f fps decoded of %.1f captured", fps, cameraFPS))
        if cameraFPS < Double(format.fps) * 0.8 {
            report.note("the camera runs below \(format.fps) fps; usually low light (it exposes longer)")
        }
        report.check(client.frames.size == CGSize(width: format.width, height: format.height),
                     "video size \(Int(client.frames.size.width))x\(Int(client.frames.size.height)), asked \(format.width)x\(format.height)")
        report.note("encoder: \(outbound?.values["encoderImplementation"] as? String ?? "?"), limited by: \(outbound?.values["qualityLimitationReason"] as? String ?? "?")")
        report.check(packets >= 100, "audio: \(packets) Opus packets in 3 s")
        report.note(String(format: "microphone level: %.3f (0 = silence, 1 = full scale)", level))
        report.check(engine.microphoneIsRecording, "microphone recording while wanted")

        engine.receive(.media(video: false, audio: false), from: client)
        report.check(await waitUntil(5) { client.lastState().map { $0.video == .off && $0.audio == .off } == true }, "both off after the linger")
        report.check(await waitUntil(5) { !engine.microphoneIsRecording }, "microphone closed once off")
        return report
    }
}
