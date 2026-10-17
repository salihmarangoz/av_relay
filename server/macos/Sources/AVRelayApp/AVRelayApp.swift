// SPDX-License-Identifier: GPL-3.0-or-later
// AV Relay for macOS: a menu bar app (no Dock icon) that serves this Mac's
// camera and microphone to paired computers.

import AVRelayCore
import AVRelayMedia
import SwiftUI

@main
struct AVRelayApp: App {
    @StateObject private var model = AppModel()

    init() {
        // `open "AV Relay.app" --args --self-test REPORT [WIDTHxHEIGHT]`: the real
        // camera and microphone through the whole media path, then quit (see README).
        let arguments = CommandLine.arguments
        if let index = arguments.firstIndex(of: "--self-test"), index + 1 < arguments.count {
            let path = arguments[index + 1]
            var format = VideoFormat(width: 1280, height: 720, fps: 30)
            if index + 2 < arguments.count {
                let size = arguments[index + 2].split(separator: "x").compactMap { Int($0) }
                if size.count == 2 { format = VideoFormat(width: size[0], height: size[1], fps: 30) }
            }
            Task {
                let report = await SelfTest.run(format: format)
                try? report.text.write(toFile: path, atomically: true, encoding: .utf8)
                exit(report.passed ? 0 : 1)
            }
        }
    }

    var body: some Scene {
        MenuBarExtra {
            MenuContent()
                .environmentObject(model)
        } label: {
            Image(systemName: model.statusSymbol)
        }
        .menuBarExtraStyle(.menu)

        Window("Pair a Device", id: PairingView.windowID) {
            PairingView()
                .environmentObject(model)
        }
        .windowResizability(.contentSize)
        .defaultPosition(.center)

        Settings {
            SettingsView()
                .environmentObject(model)
        }
    }
}

struct MenuContent: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        Text(model.statusLine)
        if let mediaLine = model.mediaLine {
            Text(mediaLine)
        }
        Divider()
        Button("Allow Pairing…") {
            model.openPairing()
            openWindow(id: PairingView.windowID)
            NSApp.activate()
        }
        Menu("Paired Devices") {
            if model.paired.isEmpty {
                Text("None yet")
            }
            ForEach(model.paired) { client in
                Button("Remove \(client.name)…") { confirmRemoval(of: client) }
            }
        }
        Divider()
        Picker("Camera", selection: $model.cameraChoice) {
            Text("Built-in (\(model.defaultCameraName))").tag(String?.none)
            ForEach(model.cameras) { Text($0.name).tag(String?.some($0.id)) }
        }
        Picker("Microphone", selection: $model.microphoneChoice) {
            Text("Built-in (\(model.defaultMicrophoneName))").tag(String?.none)
            ForEach(model.microphones) { Text($0.name).tag(String?.some($0.id)) }
        }
        Divider()
        Toggle("Open at Login", isOn: $model.openAtLogin)
        SettingsLink {
            Text("Settings…")
        }
        .keyboardShortcut(",")
        Button("About AV Relay") {
            NSApp.activate()
            NSApp.orderFrontStandardAboutPanel()
        }
        Divider()
        Button("Quit AV Relay") { NSApp.terminate(nil) }
            .keyboardShortcut("q")
    }

    private func confirmRemoval(of client: PairedClient) {
        NSApp.activate()
        let alert = NSAlert()
        alert.messageText = "Remove \(client.name)?"
        alert.informativeText = "It won't be able to use this Mac's camera and microphone until you pair it again."
        alert.addButton(withTitle: "Remove")
        alert.addButton(withTitle: "Cancel")
        if alert.runModal() == .alertFirstButtonReturn {
            model.remove(client)
        }
    }
}
