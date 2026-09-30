import Foundation
import Sparkle

/// Keeps MouseTail up to date with Sparkle. New releases download in the background, are
/// checked against MouseTail's release signing key, and install themselves (MouseTail
/// relaunches) as soon as nobody is using another computer through this Mac, so nobody is
/// cut off mid-crossing. The feed and the downloads are files on each GitHub release.
@MainActor
final class Updater: NSObject, ObservableObject, SPUUpdaterDelegate {
    static let shared = Updater()

    private var controller: SPUStandardUpdaterController?
    /// Sparkle's "install now" for a downloaded update, held until this Mac is free.
    private var pendingInstall: (() -> Void)?
    private var lastNudge: Date?
    /// When the daemon stopped answering, if it has.
    private var daemonDownSince: Date?

    override init() {
        super.init()
        // Only a built app has a feed; `swift run` during development doesn't.
        if Bundle.main.object(forInfoDictionaryKey: "SUFeedURL") != nil {
            controller = SPUStandardUpdaterController(startingUpdater: true, updaterDelegate: self, userDriverDelegate: nil)
        }
    }

    var available: Bool { controller != nil }

    var automatic: Bool {
        get { controller?.updater.automaticallyDownloadsUpdates ?? false }
        set {
            objectWillChange.send()
            controller?.updater.automaticallyChecksForUpdates = newValue
            controller?.updater.automaticallyDownloadsUpdates = newValue
        }
    }

    func checkForUpdates() {
        controller?.checkForUpdates(nil)
    }

    /// Called when the daemon doesn't answer. Nobody can be using another computer through
    /// this Mac then, and the update may well be the fix, so install once it's clearly down
    /// rather than just restarting.
    func daemonUnavailable() {
        let since = daemonDownSince ?? Date()
        daemonDownSince = since
        if -since.timeIntervalSinceNow > 30, let install = pendingInstall {
            pendingInstall = nil
            install()
        }
    }

    /// Called with each new status from the daemon.
    func statusChanged(_ status: Status?) {
        daemonDownSince = nil
        guard let status, let updater = controller?.updater else { return }
        let inUse = status.controlling != nil || status.controlledBy != nil
        if !inUse, let install = pendingInstall {
            pendingInstall = nil
            install()
            return
        }
        // Another computer already runs a newer release: look now rather than in a few hours,
        // so the two stay compatible.
        let current = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? ""
        if updater.automaticallyChecksForUpdates,
           status.peers.contains(where: { Self.isNewer($0.version, than: current) }),
           lastNudge.map({ -$0.timeIntervalSinceNow > 600 }) ?? true {
            lastNudge = Date()
            updater.checkForUpdatesInBackground()
        }
    }

    nonisolated func updater(
        _ updater: SPUUpdater,
        willInstallUpdateOnQuit item: SUAppcastItem,
        immediateInstallationBlock: @escaping () -> Void
    ) -> Bool {
        MainActor.assumeIsolated { pendingInstall = immediateInstallationBlock }
        return true
    }

    /// Compares "1.2.3"-style versions; anything after a "-" is ignored.
    static func isNewer(_ candidate: String?, than current: String) -> Bool {
        func parts(_ v: String) -> [Int] {
            (v.split(separator: "-").first ?? "").split(separator: ".").map { Int($0) ?? 0 }
        }
        guard let candidate else { return false }
        let (a, b) = (parts(candidate), parts(current))
        for i in 0..<max(a.count, b.count) {
            let (x, y) = (i < a.count ? a[i] : 0, i < b.count ? b[i] : 0)
            if x != y { return x > y }
        }
        return false
    }
}
