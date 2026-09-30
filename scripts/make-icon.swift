// Builds assets/logo.svg from the Affinity artwork, then renders it into the app's icon, the menu bar icon (also used by the
// Omarchy bar plugin and the website) and the web images.
//   swift scripts/make-icon.swift
import AppKit

/// Draws the image into a px × px canvas, inset by `inset` (a fraction of px) on every side.
func render(_ file: String, _ px: Int, height: Int? = nil, inset: Double = 0) -> Data {
    let pw = px, ph = height ?? px
    guard let image = NSImage(contentsOf: URL(fileURLWithPath: file)) else { fatalError("can't read \(file)") }
    let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: pw, pixelsHigh: ph, bitsPerSample: 8,
                               samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                               colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
    NSGraphicsContext.current?.imageInterpolation = .high
    let margin = Double(pw) * inset
    image.draw(in: NSRect(x: margin, y: margin, width: Double(pw) - 2 * margin, height: Double(ph) - 2 * margin))
    NSGraphicsContext.restoreGraphicsState()
    return rep.representation(using: .png, properties: [:])!
}

/// The SVG with its viewBox shrunk to the drawn artwork (plus a little room), and the
/// artwork's width ÷ height.
func trimmed(_ file: String) -> (String, Double) {
    let svg = try! String(contentsOfFile: file, encoding: .utf8)
    let match = svg.firstMatch(of: try! Regex(#"viewBox="([^"]*)""#))!
    let box = match.output[1].substring!.split(separator: " ").map { Double($0)! }
    let (w, h) = (Int(box[2]), Int(box[3]))
    let bitmap = NSBitmapImageRep(data: render(file, w, height: h))!
    var (minX, minY, maxX, maxY) = (w, h, 0, 0)
    for y in 0..<h {
        for x in 0..<w where bitmap.colorAt(x: x, y: y)!.alphaComponent > 0.02 {
            (minX, minY, maxX, maxY) = (min(minX, x), min(minY, y), max(maxX, x), max(maxY, y))
        }
    }
    let pad = Double(max(maxX - minX, maxY - minY)) * 0.02
    let (bw, bh) = (Double(maxX - minX) + 2 * pad, Double(maxY - minY) + 2 * pad)
    let viewBox = String(format: "%.1f %.1f %.1f %.1f", box[0] + Double(minX) - pad, box[1] + Double(minY) - pad, bw, bh)
    return (svg.replacingOccurrences(of: String(match.output[0].substring!), with: "viewBox=\"\(viewBox)\""), bw / bh)
}

/// MouseTail's logo: the artwork from Affinity (assets/logo-art.svg, kept exactly as exported)
/// on the dark tile, placed as in the original PNG. Written to assets/logo.svg.
func composeLogo() -> String {
    let art = try! String(contentsOfFile: "assets/logo-art.svg", encoding: .utf8)
    let open = art.range(of: "<svg")!
    let openEnd = art[open.upperBound...].range(of: ">")!
    let root = art[open.lowerBound..<openEnd.upperBound]
    let inner = art[openEnd.upperBound..<art.range(of: "</svg>", options: .backwards)!.lowerBound]
    let style = root.firstMatch(of: try! Regex(#"style="([^"]*)""#)).map { String($0.output[1].substring!) } ?? ""
    return """
    <svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" \
    xmlns:affinity="https://www.affinity.studio/" viewBox="0 0 1024 1024">
    <!-- Made by scripts/make-icon.swift from assets/logo-art.svg: edit that, not this. -->
    <defs><linearGradient id="mousetail-tile" x1="0" y1="0" x2="0" y2="1">\
    <stop offset="0" stop-color="#2b2c2b"/><stop offset="0.5" stop-color="#232424"/>\
    <stop offset="1" stop-color="#0f1112"/></linearGradient></defs>
    <path d="\(tilePath())" fill="url(#mousetail-tile)"/>
    <g transform="translate(-38.9 -58.6) scale(0.9093)" style="\(style)">\(inner)</g>
    </svg>

    """
}

/// The tile: a 1024 square whose corners are superellipse curves (n = 2.5) reaching 320 in
/// from each edge, the smooth corner shape of the original logo.
func tilePath() -> String {
    let (size, reach, n) = (1024.0, 320.0, 2.5)
    var points: [(Double, Double)] = []
    // Corners clockwise from top-left; each sweeps from its vertical edge to its horizontal one.
    for (cx, cy, sx, sy) in [(reach, reach, -1.0, -1.0), (size - reach, reach, 1.0, -1.0),
                             (size - reach, size - reach, 1.0, 1.0), (reach, size - reach, -1.0, 1.0)] {
        for i in 0...24 {
            var t = Double(i) / 24 * Double.pi / 2
            if sx * sy < 0 { t = Double.pi / 2 - t }  // keep going clockwise
            let dx = pow(cos(t), 2 / n) * reach, dy = pow(sin(t), 2 / n) * reach
            points.append((cx + sx * dx, cy + sy * dy))
        }
    }
    return "M" + points.map { String(format: "%.1f %.1f", $0.0, $0.1) }.joined(separator: "L") + "Z"
}

let resources = URL(fileURLWithPath: "apps/macos/Resources")
try! FileManager.default.createDirectory(at: resources, withIntermediateDirectories: true)

// The logo, and its 1024 px PNG for anything that wants a bitmap.
try! composeLogo().write(toFile: "assets/logo.svg", atomically: true, encoding: .utf8)
try! render("assets/logo.svg", 1024).write(to: URL(fileURLWithPath: "assets/logo.png"))

// App icon. The logo fills its canvas; macOS icons sit on an 824-in-1024 grid.
let iconset = URL(fileURLWithPath: NSTemporaryDirectory()).appendingPathComponent("AppIcon.iconset")
try? FileManager.default.removeItem(at: iconset)
try! FileManager.default.createDirectory(at: iconset, withIntermediateDirectories: true)
for size in [16, 32, 128, 256, 512] {
    try! render("assets/logo.svg", size, inset: 100 / 1024).write(to: iconset.appendingPathComponent("icon_\(size)x\(size).png"))
    try! render("assets/logo.svg", size * 2, inset: 100 / 1024).write(to: iconset.appendingPathComponent("icon_\(size)x\(size)@2x.png"))
}
let task = Process()
task.executableURL = URL(fileURLWithPath: "/usr/bin/iconutil")
task.arguments = ["-c", "icns", iconset.path, "-o", resources.appendingPathComponent("AppIcon.icns").path]
try! task.run()
task.waitUntilExit()

// Menu bar icon: assets/menubar.svg, a black silhouette on a square canvas. Trim the empty
// margin so it can be drawn at menu bar height, and share the trimmed artwork with the Omarchy
// bar plugin and the website.
let (iconSVG, aspect) = trimmed("assets/menubar.svg")
for path in ["integrations/omarchy/nz.galengreen.mousetail/icon.svg", "website/menubar.svg"] {
    try! iconSVG.write(toFile: path, atomically: true, encoding: .utf8)
}
let trimmedFile = NSTemporaryDirectory() + "menubar-trimmed.svg"
try! iconSVG.write(toFile: trimmedFile, atomically: true, encoding: .utf8)
let barHeight = 16  // points, like other menu bar icons
let barWidth = Int((Double(barHeight) * aspect).rounded())
try! render(trimmedFile, barWidth, height: barHeight).write(to: resources.appendingPathComponent("MenuBarIcon.png"))
try! render(trimmedFile, barWidth * 2, height: barHeight * 2).write(to: resources.appendingPathComponent("MenuBarIcon@2x.png"))

// Web and README artwork.
try! render("assets/logo.svg", 512).write(to: URL(fileURLWithPath: "assets/logo-512.png"))
try! render("assets/logo.svg", 256).write(to: URL(fileURLWithPath: "website/logo.png"))
try! render("assets/logo.svg", 64).write(to: URL(fileURLWithPath: "website/favicon.png"))
print("wrote icons")
