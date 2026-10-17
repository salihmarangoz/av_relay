// SPDX-License-Identifier: GPL-3.0-or-later
// The pairing window: the 6-digit code, how long it's valid, and the
// address to type on the other computer.

import AVRelayCore
import SwiftUI

struct PairingView: View {
    static let windowID = "pairing"
    @EnvironmentObject private var model: AppModel

    var body: some View {
        VStack(spacing: 18) {
            switch model.pairingResult {
            case .paired(let name)?:
                result(symbol: "checkmark.circle.fill", color: .green, title: "Paired with \(name)",
                       detail: "It can now use this Mac's camera and microphone whenever an app on it needs them.")
            case .failed(let reason)?:
                result(symbol: "xmark.octagon.fill", color: .red, title: "Pairing failed",
                       detail: reason == "wrong code" ? "The code didn't match. Each code works once." : "\(reason.prefix(1).uppercased())\(reason.dropFirst()).")
                Button("New Code") { model.openPairing() }
                    .keyboardShortcut(.defaultAction)
            case nil:
                if let code = model.pairingCode, let expires = model.pairingExpires {
                    codeView(code, expires: expires)
                } else {
                    Text("The code has expired.")
                        .foregroundStyle(.secondary)
                    Button("New Code") { model.openPairing() }
                        .keyboardShortcut(.defaultAction)
                }
            }
        }
        .padding(28)
        .frame(width: 420)
        .onAppear {
            if model.pairingCode == nil, model.pairingResult == nil { model.openPairing() }
        }
        .onDisappear { model.closePairing() }
    }

    private func codeView(_ code: PairingCode, expires: Date) -> some View {
        VStack(spacing: 18) {
            Text("On the other computer, choose **Pair device…** in AV Relay and enter:")
                .multilineTextAlignment(.center)
            addresses
            Text(code.display)
                .font(.system(size: 52, weight: .semibold, design: .monospaced))
                .textSelection(.enabled)
                .accessibilityLabel("Code \(code.digits.map(String.init).joined(separator: " "))")
            TimelineView(.periodic(from: .now, by: 1)) { context in
                let left = max(0, Int(expires.timeIntervalSince(context.date).rounded(.up)))
                Text(left > 0 ? "Valid for \(left) s, for one attempt" : "Expired")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .monospacedDigit()
            }
        }
    }

    private var addresses: some View {
        let list = NetworkAddresses.current()
        let port = model.listeningPort ?? model.port
        return VStack(spacing: 4) {
            if list.isEmpty {
                Text("This Mac has no network address. Connect to Wi-Fi first.")
                    .foregroundStyle(.red)
            }
            ForEach(list, id: \.address) { entry in
                HStack(spacing: 6) {
                    Text("\(entry.address)").font(.title3.monospaced()).textSelection(.enabled)
                    Text("port \(String(port))").font(.title3.monospaced()).textSelection(.enabled)
                    if list.count > 1 {
                        Text(entry.label).font(.caption).foregroundStyle(.secondary)
                    }
                }
            }
        }
    }

    private func result(symbol: String, color: Color, title: String, detail: String) -> some View {
        VStack(spacing: 10) {
            Image(systemName: symbol)
                .font(.system(size: 44))
                .foregroundStyle(color)
            Text(title).font(.title2.weight(.semibold))
            Text(detail)
                .multilineTextAlignment(.center)
                .foregroundStyle(.secondary)
        }
    }
}
