// SPDX-License-Identifier: GPL-3.0-or-later
// Settings: the port, and what identifies this Mac.

import SwiftUI

struct SettingsView: View {
    @EnvironmentObject private var model: AppModel
    @State private var portText = ""

    var body: some View {
        Form {
            Section {
                LabeledContent("Port (TCP)") {
                    HStack {
                        TextField("", text: $portText)
                            .frame(width: 80)
                            .onSubmit(apply)
                        Button("Apply", action: apply)
                            .disabled(UInt16(portText) == nil || UInt16(portText) == model.port)
                    }
                }
                Text("Paired computers find this Mac again on their own after a change; new pairings use the new port.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Section {
                LabeledContent("This Mac's device ID", value: model.deviceID ?? "–")
                LabeledContent("Version", value: AppModel.version)
            }
        }
        .formStyle(.grouped)
        .frame(width: 440)
        .onAppear { portText = String(model.port) }
    }

    private func apply() {
        guard let port = UInt16(portText), port > 0 else { return }
        model.setPort(port)
    }
}
