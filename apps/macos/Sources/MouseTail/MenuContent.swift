import AppKit
import SwiftUI

/// The menu bar popover: what's connected, where the cursor is, and the few things you might
/// want to change.
struct MenuContent: View {
    @EnvironmentObject var model: AppModel
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            header
            if let status = model.status {
                if !status.canControl {
                    PermissionNotice(detail: status.captureError)
                }
                if status.controlling != nil && status.keyboardBlocked == true {
                    Label("A password field on this Mac is blocking typing. The mouse still works.",
                          systemImage: "keyboard.badge.exclamationmark")
                        .font(.caption)
                        .foregroundStyle(.orange)
                }
                if let shown = status.pairingCode {
                    ShownCodePanel(shown: shown)
                }
                Divider()
                machines(status)
                if let pairing = model.pairing {
                    PairingPanel(pairing: pairing)
                }
                Divider()
                Button {
                    openWindow(id: "arrange")
                    NSApp.activate()
                } label: {
                    Label("Arrange Displays…", systemImage: "rectangle.3.group")
                }
                .buttonStyle(.plain)
                Toggle(soundLabel(status), isOn: Binding(
                    get: { status.settings.audio ?? true },
                    set: { on in Task { await model.setAudio(on) } }
                ))
                Toggle("Share clipboard", isOn: Binding(
                    get: { status.settings.clipboard },
                    set: { on in Task { await model.setClipboard(on) } }
                ))
                Toggle("Open at login", isOn: Binding(
                    get: { model.openAtLogin },
                    set: { model.openAtLogin = $0 }
                ))
            } else {
                Text(model.problem ?? "Starting…")
                    .foregroundStyle(.secondary)
            }
            Divider()
            Button("Quit MouseTail") { NSApp.terminate(nil) }
                .buttonStyle(.plain)
                .keyboardShortcut("q")
        }
        .toggleStyle(.switch)
        .controlSize(.small)
        .padding(14)
        .frame(width: 300)
    }

    private func soundLabel(_ status: Status) -> String {
        let names = status.peers.filter(\.paired).map(\.name)
        return names.count == 1 ? "Play \(names[0])'s sound here" : "Play other computers' sound here"
    }

    private var header: some View {
        HStack(alignment: .firstTextBaseline) {
            Text("MouseTail").font(.headline)
            Spacer()
            Text(summary).font(.caption).foregroundStyle(.secondary)
        }
    }

    private var summary: String {
        guard let s = model.status else { return "" }
        if let peer = s.peerName(s.controlling) { return "Cursor on \(peer)" }
        if let peer = s.peerName(s.controlledBy) { return "Controlled by \(peer)" }
        let connected = s.peers.filter { $0.paired && $0.connected }.count
        return connected == 0 ? "Not connected" : "Ready"
    }

    @ViewBuilder
    private func machines(_ status: Status) -> some View {
        let visible = status.peers.filter { $0.paired || $0.connected }
        if visible.isEmpty {
            Label("Looking for other computers on your network…", systemImage: "magnifyingglass")
                .font(.callout)
                .foregroundStyle(.secondary)
        }
        ForEach(visible) { peer in
            PeerRow(peer: peer, active: status.controlling == peer.id || status.controlledBy == peer.id)
        }
    }
}

struct PeerRow: View {
    @EnvironmentObject var model: AppModel
    let peer: PeerStatus
    let active: Bool

    var body: some View {
        HStack(spacing: 8) {
            Circle()
                .fill(peer.connected ? (peer.paired ? Color.green : Color.orange) : Color.secondary.opacity(0.4))
                .frame(width: 8, height: 8)
            VStack(alignment: .leading, spacing: 1) {
                Text(peer.name).fontWeight(active ? .semibold : .regular)
                Text(detail).font(.caption).foregroundStyle(.secondary)
            }
            Spacer()
            if !peer.paired && peer.connected {
                Button("Pair…") { Task { await model.startPairing(peer) } }
            } else if peer.paired {
                Menu {
                    Button("Forget \(peer.name)", role: .destructive) { Task { await model.unpair(peer) } }
                } label: {
                    Image(systemName: "ellipsis.circle")
                }
                .menuStyle(.borderlessButton)
                .menuIndicator(.hidden)
                .fixedSize()
            }
        }
    }

    private var detail: String {
        if !peer.paired { return peer.connected ? "Found on your network" : "Not paired" }
        guard peer.connected else { return "Offline" }
        if let ms = peer.rttMs { return String(format: "Connected · %.0f ms", ms) }
        return "Connected"
    }
}

struct PairingPanel: View {
    @EnvironmentObject var model: AppModel
    let pairing: Pairing
    @State private var code = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            switch pairing {
            case .awaitingCode(let peer):
                Text("Type the code shown on \(peer.name):").font(.callout)
                HStack {
                    TextField("0000", text: $code)
                        .textFieldStyle(.roundedBorder)
                        .font(.system(.title3, design: .monospaced))
                        .frame(width: 90)
                        .onSubmit(submit)
                        .onChange(of: code) { _, new in
                            code = String(new.filter(\.isNumber).prefix(4))
                            if code.count == 4 { submit() }
                        }
                    Button("Cancel") { model.pairing = nil }
                }
            case .working(let peer):
                HStack { ProgressView().controlSize(.small); Text("Pairing with \(peer.name)…") }
            case .done(let message):
                Label(message, systemImage: "checkmark.circle.fill").foregroundStyle(.green)
                    .task { try? await Task.sleep(for: .seconds(3)); model.pairing = nil }
            case .failed(let message):
                Label(message, systemImage: "exclamationmark.triangle.fill").foregroundStyle(.orange)
                Button("OK") { model.pairing = nil }
            }
        }
        .padding(10)
        .background(RoundedRectangle(cornerRadius: 8).fill(.quaternary))
    }

    private func submit() {
        guard case .awaitingCode(let peer) = pairing, code.count == 4 else { return }
        let entered = code
        Task { await model.submitCode(entered, for: peer) }
    }
}

/// Another computer asked to pair: show the code big, to type over there.
struct ShownCodePanel: View {
    let shown: ShownCode

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Pairing code").font(.caption).foregroundStyle(.secondary)
            Text(shown.code.map { String($0) }.joined(separator: " "))
                .font(.system(size: 30, weight: .semibold, design: .monospaced))
            Text("Type this on \(shown.name ?? "the other computer") to connect it.")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(10)
        .background(RoundedRectangle(cornerRadius: 8).fill(.quaternary))
    }
}

struct PermissionNotice: View {
    let detail: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Label("Allow MouseTail to use your keyboard and mouse", systemImage: "hand.raised.fill")
                .font(.callout.weight(.semibold))
            Text("Turn on MouseTail in Accessibility and Input Monitoring. It starts working as soon as you do.")
                .font(.caption)
                .foregroundStyle(.secondary)
            HStack {
                Button("Accessibility…") { open("Privacy_Accessibility") }
                Button("Input Monitoring…") { open("Privacy_ListenEvent") }
            }
        }
        .padding(10)
        .background(RoundedRectangle(cornerRadius: 8).fill(Color.orange.opacity(0.12)))
    }

    private func open(_ pane: String) {
        if let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?\(pane)") {
            NSWorkspace.shared.open(url)
        }
    }
}
