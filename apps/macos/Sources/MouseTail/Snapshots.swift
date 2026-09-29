import AppKit
import SwiftUI

/// `MouseTail --render <dir>` draws the menu and arrangement views, filled with live data from
/// the running daemon, to PNG files. For checking the UI without clicking through it.
@MainActor
enum Snapshots {
    static func render(to dir: URL) async {
        let model = AppModel.shared
        await model.refresh()
        await model.refreshLayout()
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)

        save(MenuContent().environmentObject(model), "menu", to: dir)
        save(ArrangeView().environmentObject(model).frame(width: 760, height: 560), "arrange", to: dir)

        if let peer = model.status?.peers.first {
            model.pairing = .awaitingCode(PeerStatus(
                id: peer.id, name: peer.name, paired: false, connected: true,
                address: nil, rttMs: nil, platform: nil))
            save(MenuContent().environmentObject(model), "menu-pairing", to: dir)
            model.pairing = nil
        }
    }

    private static func save<V: View>(_ view: V, _ name: String, to dir: URL) {
        let renderer = ImageRenderer(content: view.background(Color(nsColor: .windowBackgroundColor)))
        renderer.scale = 2
        guard let image = renderer.nsImage,
              let tiff = image.tiffRepresentation,
              let png = NSBitmapImageRep(data: tiff)?.representation(using: .png, properties: [:]) else {
            print("couldn't render \(name)")
            return
        }
        let url = dir.appendingPathComponent("\(name).png")
        try? png.write(to: url)
        print("wrote \(url.path)")
    }
}
