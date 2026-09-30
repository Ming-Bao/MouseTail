// Renders the artwork in assets/ into the app's icon, the menu bar icon (also used by the
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

/// assets/logo.png with the light band along the tile's top and bottom edges painted over in
/// the tile's own colour (it shows as white lines at small sizes). The original is untouched.
func logoWithoutEdgeLines() -> String {
    let source = NSBitmapImageRep(data: try! Data(contentsOf: URL(fileURLWithPath: "assets/logo.png")))!
    // Work on a plain RGBA copy (premultiplied alpha), pixel by pixel.
    let rep = NSBitmapImageRep(data: render("assets/logo.png", source.pixelsWide))!
    let (w, h, band, sample) = (rep.pixelsWide, rep.pixelsHigh, 14, 18)
    var p = [Int](repeating: 0, count: 4)
    func alpha(_ x: Int, _ y: Int) -> Int { rep.getPixel(&p, atX: x, y: y); return p[3] }
    for x in 0..<w {
        // This column's top and bottom edges of the tile.
        guard let top = (0..<h).first(where: { alpha(x, $0) > 0 }),
              let bottom = (0..<h).last(where: { alpha(x, $0) > 0 }),
              bottom - top > 2 * sample else { continue }
        for (edge, step) in [(top, 1), (bottom, -1)] {
            var fill = [Int](repeating: 0, count: 4)
            rep.getPixel(&fill, atX: x, y: edge + step * sample)
            for i in 0..<band {
                let y = edge + step * i
                let a = alpha(x, y)
                var out = [fill[0] * a / 255, fill[1] * a / 255, fill[2] * a / 255, a]
                rep.setPixel(&out, atX: x, y: y)
            }
        }
    }
    let path = NSTemporaryDirectory() + "logo-clean.png"
    try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: path))
    return path
}

let resources = URL(fileURLWithPath: "apps/macos/Resources")
try! FileManager.default.createDirectory(at: resources, withIntermediateDirectories: true)

let logo = logoWithoutEdgeLines()

// App icon. The logo fills its canvas; macOS icons sit on an 824-in-1024 grid.
let iconset = URL(fileURLWithPath: NSTemporaryDirectory()).appendingPathComponent("AppIcon.iconset")
try? FileManager.default.removeItem(at: iconset)
try! FileManager.default.createDirectory(at: iconset, withIntermediateDirectories: true)
for size in [16, 32, 128, 256, 512] {
    try! render(logo, size, inset: 100 / 1024).write(to: iconset.appendingPathComponent("icon_\(size)x\(size).png"))
    try! render(logo, size * 2, inset: 100 / 1024).write(to: iconset.appendingPathComponent("icon_\(size)x\(size)@2x.png"))
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

// The logo comes in three versions:
//   assets/logo.png        the app icon, on its tile: the app, and marketing (README, website)
//   assets/menubar.svg     black and white: the Mac menu bar, the Omarchy bar
//   assets/logo-mouse.svg  just the mouse and trail, for places with their own background
//                          (the website's nav, the Mac menu's header)
let (mouseSVG, _) = trimmed("assets/logo-mouse.svg")
for path in ["website/logo-mouse.svg", "apps/macos/Resources/LogoMouse.svg"] {
    try! mouseSVG.write(toFile: path, atomically: true, encoding: .utf8)
}

// Web and README artwork.
try! render(logo, 512).write(to: URL(fileURLWithPath: "assets/logo-512.png"))
try! render(logo, 256).write(to: URL(fileURLWithPath: "website/logo.png"))
try! render(logo, 64).write(to: URL(fileURLWithPath: "website/favicon.png"))
print("wrote icons")
