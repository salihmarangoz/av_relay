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

    /// Off is as if AV Relay were closed: no listening, no Bonjour, the
    /// camera and microphone closed; only the menu stays. Remembered.
    @Published var enabled: Bool {
        didSet {
            guard enabled != oldValue else { return }
            defaults.set(enabled, forKey: Keys.enabled)
            serially { [weak self] in
                guard let self else { return }
                if self.enabled { await self.turnOn() } else { await self.turnOff() }
            }
        }
    }

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
            guard openAtLogin != Self.loginItemOn else { return }
            do {
                if openAtLogin { try SMAppService.mainApp.register() } else { try SMAppService.mainApp.unregister() }
            } catch {
                problem = "Couldn't change Open at Login: \(error.localizedDescription)"
                // System Settings › General › Login Items can always change it.
                SMAppService.openSystemSettingsLoginItems()
            }
            if openAtLogin != Self.loginItemOn { openAtLogin = Self.loginItemOn }
        }
    }

    /// On, or waiting for the user's approval in System Settings (which still counts as asked for).
    static var loginItemOn: Bool {
        [.enabled, .requiresApproval].contains(SMAppService.mainApp.status)
    }

    private(set) var port: UInt16
    let deviceID: String?
    private let defaults = UserDefaults.standard
    private let store: IdentityStore?
    private let engine: MediaEngine
    private var server: RelayServer?
    private var eventsTask: Task<Void, Never>?
    private var deviceObservers: [NSObjectProtocol] = []
    private var systemObservers: [NSObjectProtocol] = []
    private var sleepWatcher: SleepWatcher?
    /// Starting and stopping the server, one change after the other.
    private var transition: Task<Void, Never>?

    enum Keys {
        static let camera = "camera", microphone = "microphone", port = "port", enabled = "enabled"
        /// Set once Open at Login was turned on at first launch; after that, the user's toggle decides.
        static let loginItemDefaulted = "loginItemDefaulted"
    }

    static let defaultPort: UInt16 = 48100
    static var version: String { Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "dev" }
    /// This computer's name as the user set it, shown to receivers.
    static var computerName: String { Host.current().localizedName ?? "Mac" }

    init() {
        let camera = defaults.string(forKey: Keys.camera)
        let microphone = defaults.string(forKey: Keys.microphone)
        cameraChoice = camera
        microphoneChoice = microphone
        openAtLogin = Self.loginItemOn
        enabled = defaults.object(forKey: Keys.enabled) as? Bool ?? true
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
        enableOpenAtLoginOnFirstLaunch()
        refreshDevices()
        for name in [AVCaptureDevice.wasConnectedNotification, AVCaptureDevice.wasDisconnectedNotification] {
            deviceObservers.append(NotificationCenter.default.addObserver(forName: name, object: nil, queue: .main) { [weak self] _ in
                Task { @MainActor in self?.refreshDevices() }
            })
        }
        observeSleepAndQuit()
        if enabled {
            serially { [weak self] in await self?.startServer() }
        }
    }

    /// Before the Mac sleeps: end the session (bye "sleep"), refuse new ones
    /// until the wake, and close the camera and microphone; only then let
    /// the sleep go on. So capture can't resume on wake for a client that's
    /// long gone; the client reconnects after the wake. On quit, say bye so
    /// the client knows at once.
    private func observeSleepAndQuit() {
        sleepWatcher = SleepWatcher(willSleep: { [weak self] proceed in
            Task { @MainActor in
                await self?.server?.suspend()
                await self?.engine.stopAll()
                proceed()
            }
        }, didWake: { [weak self] in
            Task { @MainActor in
                guard let self, self.enabled else { return }
                self.engine.resume()
                await self.server?.resume()
            }
        })
        systemObservers.append(NotificationCenter.default.addObserver(
            forName: NSApplication.willTerminateNotification, object: nil, queue: .main) { [weak self] _ in
            guard let server = MainActor.assumeIsolated({ self?.server }) else { return }
            let done = DispatchSemaphore(value: 0)
            Task.detached {
                await server.stop()
                done.signal()
            }
            _ = done.wait(timeout: .now() + 1)
        })
    }

    /// AV Relay should run whenever the Mac is on, so Open at Login starts
    /// out on. Only the first launch does this: if the user turns it off,
    /// it stays off. (Not for the self-test, which only runs and quits.)
    /// Only for a copy in Applications: a login item for the copy in the
    /// disk image (or a quarantined, translocated copy) would break once
    /// the user installs it, and would use up the one-time default.
    private func enableOpenAtLoginOnFirstLaunch() {
        guard !defaults.bool(forKey: Keys.loginItemDefaulted), !CommandLine.arguments.contains("--self-test"),
              Self.isInApplications else { return }
        defaults.set(true, forKey: Keys.loginItemDefaulted)
        if !Self.loginItemOn {
            openAtLogin = true
        }
    }

    static var isInApplications: Bool {
        let path = Bundle.main.bundleURL.resolvingSymlinksInPath().path
        let home = FileManager.default.homeDirectoryForCurrentUser.path
        return path.hasPrefix("/Applications/") || path.hasPrefix(home + "/Applications/")
    }

    // MARK: Server

    private func startServer() async {
        guard let store else { return }
        var configuration = RelayServer.Configuration(name: Self.computerName, app: "AV Relay \(Self.version) macOS")
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
        serially { [weak self] in
            guard let self, self.server != nil else { return }
            await self.stopServer()
            await self.startServer()
        }
    }

    /// Runs `change` once the changes before it have finished, so a quick
    /// off-on or a port change can't interleave with another.
    private func serially(_ change: @escaping @MainActor () async -> Void) {
        let previous = transition
        transition = Task {
            await previous?.value
            await change()
        }
    }

    private func stopServer() async {
        eventsTask?.cancel()
        eventsTask = nil
        let stopping = server
        server = nil
        await stopping?.stop()
        listeningPort = nil
        connected = nil
        // The old server's code is gone with it.
        pairingCode = nil
        pairingExpires = nil
        pairingResult = nil
    }

    /// Switched off: the receiver gets bye "quit", as when the app quits.
    private func turnOff() async {
        guard !enabled else { return }
        await stopServer()
        await engine.stopAll()
        problem = nil
    }

    private func turnOn() async {
        guard enabled, server == nil else { return }
        engine.resume()
        await startServer()
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
                if let server {
                    try await server.removeClient(publicKey: client.publicKey)
                } else {
                    _ = try store?.remove(publicKey: client.publicKey)
                }
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
        if !enabled { return "Off: receivers can't connect" }
        if let problem { return problem }
        if let connected { return "Connected to \(connected.name)" }
        if paired.isEmpty { return "Not paired yet" }
        return listeningPort == nil ? "Starting…" : "Waiting for a paired device"
    }

    var mediaLine: String? {
        guard connected != nil else { return nil }
        return "Camera \(Self.describe(media.video)) · Microphone \(Self.describe(media.audio))"
    }

    enum IconState: String { case off, on, live, disabled }

    var iconState: IconState {
        if !enabled { return .disabled }
        if media.video.state == .on || media.audio.state == .on { return .live }
        return connected != nil ? .on : .off
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
