import Foundation
import ServiceManagement

enum Pairing: Equatable {
    case awaitingCode(PeerStatus)
    case working(PeerStatus)
    case done(String)
    case failed(String)
}

@MainActor
final class AppModel: ObservableObject {
    @Published var status: Status?
    @Published var layout: LayoutInfo?
    @Published var problem: String?
    @Published var pairing: Pairing?

    let client = DaemonClient()
    let daemon = DaemonProcess()
    private var timer: Timer?

    func start() async {
        await daemon.ensureRunning()
        await refresh()
        timer = Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { [weak self] _ in
            Task { @MainActor in await self?.refresh() }
        }
    }

    func refresh() async {
        do {
            let s = try await client.decode(Status.self, ["cmd": "status"])
            if s != status { status = s }
            problem = nil
            Updater.shared.statusChanged(s)
        } catch {
            status = nil
            problem = error.localizedDescription
            await daemon.ensureRunning()
        }
    }

    func refreshLayout() async {
        if let l = try? await client.decode(LayoutInfo.self, ["cmd": "layout"]), l != layout {
            layout = l
        }
    }

    // MARK: Pairing

    func startPairing(_ peer: PeerStatus) async {
        do {
            _ = try await client.call(["cmd": "pair", "peer": peer.id])
            pairing = .awaitingCode(peer)
        } catch {
            pairing = .failed(error.localizedDescription)
        }
    }

    func submitCode(_ code: String, for peer: PeerStatus) async {
        pairing = .working(peer)
        do {
            _ = try await client.call(["cmd": "pair_code", "peer": peer.id, "code": code], timeout: 20)
            pairing = .done("Paired with \(peer.name).")
        } catch {
            pairing = .failed(error.localizedDescription)
        }
        await refresh()
    }

    func unpair(_ peer: PeerStatus) async {
        _ = try? await client.call(["cmd": "unpair", "peer": peer.id])
        await refresh()
        await refreshLayout()
    }

    // MARK: Layout and settings

    /// Drop a machine at a layout position; returns where it snapped to.
    func place(_ machine: Machine, at p: Point) async -> Point? {
        let reply = try? await client.call(["cmd": "place_at", "peer": machine.id, "x": p.x, "y": p.y])
        await refreshLayout()
        guard let reply,
              let object = try? JSONSerialization.jsonObject(with: reply) as? [String: Any],
              let offset = object["offset"] as? [String: Double],
              let x = offset["x"], let y = offset["y"] else { return nil }
        return Point(x: x, y: y)
    }

    func setClipboard(_ on: Bool) async {
        _ = try? await client.call(["cmd": "set_setting", "key": "clipboard", "value": on])
        await refresh()
    }

    func setAudio(_ on: Bool) async {
        _ = try? await client.call(["cmd": "set_setting", "key": "audio", "value": on])
        await refresh()
    }

    func bringCursorHome() async {
        _ = try? await client.call(["cmd": "release"])
    }

    var openAtLogin: Bool {
        get { SMAppService.mainApp.status == .enabled }
        set {
            do {
                if newValue { try SMAppService.mainApp.register() } else { try SMAppService.mainApp.unregister() }
            } catch {
                problem = "Couldn't change Open at Login: \(error.localizedDescription)"
            }
            objectWillChange.send()
        }
    }
}
