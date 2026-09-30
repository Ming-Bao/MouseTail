import AppKit
import SwiftUI

@main
struct MouseTailApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    @StateObject private var model = AppModel.shared

    var body: some Scene {
        MenuBarExtra {
            MenuContent().environmentObject(model)
        } label: {
            MenuBarIcon(active: model.status?.controlling != nil || model.status?.controlledBy != nil)
        }
        .menuBarExtraStyle(.window)

        Window("Arrange Displays", id: "arrange") {
            ArrangeView().environmentObject(model)
        }
        .windowResizability(.contentSize)
        .defaultSize(width: 620, height: 460)
        .defaultPosition(.center)
    }
}

/// The logo as a silhouette: the trail is faint normally and lights up while the cursor is on
/// another computer.
struct MenuBarIcon: View {
    let active: Bool

    var body: some View {
        if let image = NSImage(named: active ? "MenuBarIconActive" : "MenuBarIcon") {
            Image(nsImage: {
                image.isTemplate = true
                return image
            }())
        } else {
            // Running outside the app bundle (development).
            Image(systemName: active ? "computermouse.fill" : "computermouse")
        }
    }
}

extension AppModel {
    static let shared = AppModel()
}

final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationDidFinishLaunching(_ notification: Notification) {
        MainActor.assumeIsolated {
            if let i = CommandLine.arguments.firstIndex(of: "--render"),
               CommandLine.arguments.indices.contains(i + 1) {
                let dir = URL(fileURLWithPath: CommandLine.arguments[i + 1])
                Task { await Snapshots.render(to: dir); exit(0) }
                return
            }
            Task { await AppModel.shared.start() }
        }
    }

    func applicationWillTerminate(_ notification: Notification) {
        MainActor.assumeIsolated {
            AppModel.shared.daemon.stop()
        }
    }
}
