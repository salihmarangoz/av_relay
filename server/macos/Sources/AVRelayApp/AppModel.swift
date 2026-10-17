// SPDX-License-Identifier: GPL-3.0-or-later
// The app's state: the control server, the media engine, the user's
// choices, and what the menu and windows show.

import AVFoundation
import AVRelayCore
import AVRelayMedia
import ServiceManagement
import SwiftUI

@MainActor
final class AppModel: ObservableObject {
    enum PairingResult: Equatable {
        case paired(String)
        case failed(String)
    }

    @Published private(set) var listeningPort: UInt16?
    @Published private(set) var problem: String?
    @Published private(set) var pairingCode: PairingCode?
    @Published private(set) var pairingExpires: Date?
    @Published private(set) var pairingResult: PairingResult?
    @Published private(set) var paired: [PairedClient] = []
    @Published private(set) var connected: PairedClient?
    @Published private(set) var media = MediaEngine.Status.idle
    @Published private(set) var cameras: [CaptureDeviceInfo] = []
    @Published private(set) var microphones: [CaptureDeviceInfo] = []

    /// nil: the built-in one.
    @Published var cameraChoice: String? {
        didSet {
            defaults.set(cameraChoice, forKey: Keys.camera)
            engine.setCamera(.device(id: cameraChoice))
        }
    }

    @Published var microphoneChoice: String? {
        didSet {
            defaults.set(microphoneChoice, forKey: Keys.microphone)
            engine.setMicrophone(.device(id: microphoneChoice))
        }
    }

    @Published var openAtLogin: Bool {
        didSet {
            guard openAtLogin != (SMAppService.mainApp.status == .enabled) else { return }
            do {
                if openAtLogin { try SMAppService.mainApp.register() } else { try SMAppService.mainApp.unregister() }
            } catch {
                problem = "Couldn't change Open at Login: \(error.localizedDescription)"
                openAtLogin = SMAppService.mainApp.status == .enabled
            }
        }
    }

    private(set) var port: UInt16
    let deviceID: String?
    private let defaults = UserDefaults.standard
    private let store: IdentityStore?
    private let engine: MediaEngine
    private var server: RelayServer?
    private var eventsTask: Task<Void, Never>?
    private var deviceObservers: [NSObjectProtocol] = []

    enum Keys {
        static let camera = "camera", microphone = "microphone", port = "port"
    }

    static let defaultPort: UInt16 = 48100
    static var version: String { Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "dev" }
    static var macName: String { Host.current().localizedName ?? "Mac" }

    init() {
        let camera = defaults.string(forKey: Keys.camera)
        let microphone = defaults.string(forKey: Keys.microphone)
        cameraChoice = camera
        microphoneChoice = microphone
        openAtLogin = SMAppService.mainApp.status == .enabled
        let savedPort = defaults.integer(forKey: Keys.port)
        port = (1...65535).contains(savedPort) ? UInt16(savedPort) : Self.defaultPort

        var settings = MediaEngine.Settings()
        settings.camera = .device(id: camera)
        settings.microphone = .device(id: microphone)
        engine = MediaEngine(settings: settings)

        var opened: IdentityStore?
        do {
            opened = try IdentityStore()
        } catch {
            problem = "Can't open AV Relay's key in \(IdentityStore.defaultDirectory.path): \(error.localizedDescription)"
        }
        store = opened
        deviceID = opened?.deviceID
        paired = opened?.pairedClients ?? []

        engine.onStatus = { [weak self] status in
            Task { @MainActor in self?.media = status }
        }
        refreshDevices()
        for name in [AVCaptureDevice.wasConnectedNotification, AVCaptureDevice.wasDisconnectedNotification] {
            deviceObservers.append(NotificationCenter.default.addObserver(forName: name, object: nil, queue: .main) { [weak self] _ in
                Task { @MainActor in self?.refreshDevices() }
            })
        }
        Task { await startServer() }
    }

    // MARK: Server

    private func startServer() async {
        guard let store else { return }
        var configuration = RelayServer.Configuration(name: Self.macName, app: "AV Relay \(Self.version) macOS")
        configuration.port = port
        let server = RelayServer(configuration: configuration, store: store)
        await server.setHandler(engine)
        self.server = server
        eventsTask = Task { [weak self] in
            for await event in server.events {
                self?.handle(event)
            }
        }
        do {
            listeningPort = try await server.start()
            problem = nil
        } catch {
            listeningPort = nil
            problem = "Can't listen on TCP port \(port): \(error.localizedDescription). Choose another port in Settings."
        }
    }

    private func handle(_ event: RelayServer.Event) {
        switch event {
        case .listening(let port):
            listeningPort = port
        case .listenerFailed(let message):
            problem = "The server stopped: \(message)"
        case let .pairingWindowOpened(code, expires):
            pairingCode = code
            pairingExpires = expires
            pairingResult = nil
        case .pairingAttemptStarted:
            break
        case .pairingWindowClosed:
            pairingCode = nil
            pairingExpires = nil
        case .paired(let client):
            pairingResult = .paired(client.name)
            paired = store?.pairedClients ?? []
        case .pairingFailed(let reason):
            pairingResult = .failed(reason)
        case .sessionStarted(let client):
            connected = client
        case .sessionEnded(let client, _):
            if connected?.publicKey == client.publicKey { connected = nil }
        case .clientRemoved:
            paired = store?.pairedClients ?? []
        }
    }

    /// Changes the port: the server restarts on it (a connected client reconnects).
    func setPort(_ newPort: UInt16) {
        guard newPort != port, newPort > 0 else { return }
        port = newPort
        defaults.set(Int(newPort), forKey: Keys.port)
        Task {
            eventsTask?.cancel()
            await server?.stop()
            server = nil
            connected = nil
            await startServer()
        }
    }

    // MARK: Pairing

    func openPairing() {
        Task { await server?.openPairingWindow() }
    }

    func closePairing() {
        Task { await server?.closePairingWindow() }
    }

    func remove(_ client: PairedClient) {
        Task {
            do {
                try await server?.removeClient(publicKey: client.publicKey)
            } catch {
                problem = "Couldn't remove \(client.name): \(error.localizedDescription)"
            }
            paired = store?.pairedClients ?? []
        }
    }

    // MARK: Devices

    func refreshDevices() {
        cameras = CaptureDevices.cameras()
        microphones = CaptureDevices.microphones()
    }

    var defaultCameraName: String { CaptureDevices.defaultCamera()?.name ?? "None" }
    var defaultMicrophoneName: String { CaptureDevices.defaultMicrophone()?.name ?? "None" }

    // MARK: Menu text

    var statusLine: String {
        if let problem { return problem }
        if let connected { return "Connected to \(connected.name)" }
        if paired.isEmpty { return "Not paired yet" }
        return listeningPort == nil ? "Starting…" : "Waiting for a paired device"
    }

    var mediaLine: String? {
        guard connected != nil else { return nil }
        return "Camera \(Self.describe(media.video)) · Microphone \(Self.describe(media.audio))"
    }

    var statusSymbol: String {
        if problem != nil { return "exclamationmark.triangle" }
        if media.video.state == .on || media.audio.state == .on { return "web.camera.fill" }
        return connected != nil ? "web.camera" : "video.slash"
    }

    static func describe(_ state: DeviceState) -> String {
        switch state.state {
        case .off: "off"
        case .starting: "starting"
        case .on: "on"
        case .error:
            switch state.error {
            case .permissionDenied?: "not allowed (System Settings › Privacy & Security)"
            case .noDevice?: "not found"
            case .deviceBusy?: "busy"
            default: "failed"
            }
        }
    }
}
