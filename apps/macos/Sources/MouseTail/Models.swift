import Foundation

// Mirrors the JSON from the daemon's control socket (snake_case is converted on decode).

struct Point: Codable, Equatable {
    var x: Double
    var y: Double
}

struct Rect: Codable, Equatable {
    var x: Double
    var y: Double
    var w: Double
    var h: Double

    var right: Double { x + w }
    var bottom: Double { y + h }

    func offset(by p: Point) -> Rect { Rect(x: x + p.x, y: y + p.y, w: w, h: h) }

    func union(_ o: Rect) -> Rect {
        let nx = min(x, o.x), ny = min(y, o.y)
        return Rect(x: nx, y: ny, w: max(right, o.right) - nx, h: max(bottom, o.bottom) - ny)
    }
}

struct Display: Codable, Equatable, Identifiable {
    var id: String
    var name: String
    var rect: Rect
    var scale: Double
    var primary: Bool
}

struct Machine: Codable, Identifiable, Equatable {
    var id: String
    var name: String
    var this: Bool
    var connected: Bool
    /// Paused: stays paired, but the cursor doesn't cross to it.
    var paused: Bool?
    var displays: [Display]
    var offset: Point?

    /// Displays in layout coordinates.
    var placed: [Rect] { displays.map { $0.rect.offset(by: offset ?? Point(x: 0, y: 0)) } }
}

struct LayoutInfo: Codable, Equatable {
    var machines: [Machine]
    /// Edge stretches where the cursor passes between computers: [start, end] pairs.
    var crossings: [[Point]]?

    /// Machines with a place in the arrangement: this Mac, and others once they've been put
    /// somewhere (one that's never connected has nowhere to go yet).
    var shown: [Machine] { machines.filter { $0.this || $0.offset != nil } }
}

struct Settings: Codable, Equatable {
    var clipboard: Bool
    var clipboardMaxBytes: Int
    var audio: Bool?
    var ripple: Bool?
}

struct PeerStatus: Codable, Identifiable, Equatable {
    var id: String
    var name: String
    var paired: Bool
    var connected: Bool
    var address: String?
    var rttMs: Double?
    var platform: String?
    var version: String?
    /// "here" (its sound plays on this Mac) or "there" (this Mac's sound plays on it).
    var sound: String?
    /// Still paired, but nothing crosses until it's resumed.
    var paused: Bool?
}

/// A code this Mac is showing because another computer asked to pair.
struct ShownCode: Codable, Equatable {
    var peer: String
    var name: String?
    var code: String
}

struct Status: Codable, Equatable {
    var id: String
    var name: String
    var port: Int
    var canControl: Bool
    var canBeControlled: Bool
    var captureError: String?
    var keyboardBlocked: Bool?
    var displays: [Display]
    var controlling: String?
    var controlledBy: String?
    var pairingCode: ShownCode?
    var settings: Settings
    var peers: [PeerStatus]

    func peerName(_ id: String?) -> String? {
        guard let id else { return nil }
        return peers.first { $0.id == id }?.name ?? id
    }
}
