// Renders the artwork in assets/ into the app's icon, menu bar images and web images.
//   swift scripts/make-icon.swift
import AppKit

/// Draws the image into a px × px canvas, inset by `inset` (a fraction of px) on every side.
func render(_ file: String, _ px: Int, inset: Double = 0) -> Data {
    guard let image = NSImage(contentsOf: URL(fileURLWithPath: file)) else { fatalError("can't read \(file)") }
    let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: px, pixelsHigh: px, bitsPerSample: 8,
                               samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                               colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
    NSGraphicsContext.current?.imageInterpolation = .high
    let margin = Double(px) * inset
    image.draw(in: NSRect(x: margin, y: margin, width: Double(px) - 2 * margin, height: Double(px) - 2 * margin))
    NSGraphicsContext.restoreGraphicsState()
    return rep.representation(using: .png, properties: [:])!
}

let resources = URL(fileURLWithPath: "apps/macos/Resources")
try! FileManager.default.createDirectory(at: resources, withIntermediateDirectories: true)

// App icon. The logo fills its canvas; macOS icons sit on an 824-in-1024 grid.
let iconset = URL(fileURLWithPath: NSTemporaryDirectory()).appendingPathComponent("AppIcon.iconset")
try? FileManager.default.removeItem(at: iconset)
try! FileManager.default.createDirectory(at: iconset, withIntermediateDirectories: true)
for size in [16, 32, 128, 256, 512] {
    try! render("assets/logo.png", size, inset: 100 / 1024).write(to: iconset.appendingPathComponent("icon_\(size)x\(size).png"))
    try! render("assets/logo.png", size * 2, inset: 100 / 1024).write(to: iconset.appendingPathComponent("icon_\(size)x\(size)@2x.png"))
}
let task = Process()
task.executableURL = URL(fileURLWithPath: "/usr/bin/iconutil")
task.arguments = ["-c", "icns", iconset.path, "-o", resources.appendingPathComponent("AppIcon.icns").path]
try! task.run()
task.waitUntilExit()

// Menu bar (template images, 18 pt).
for (svg, name) in [("assets/menubar.svg", "MenuBarIcon"), ("assets/menubar-active.svg", "MenuBarIconActive")] {
    try! render(svg, 18).write(to: resources.appendingPathComponent("\(name).png"))
    try! render(svg, 36).write(to: resources.appendingPathComponent("\(name)@2x.png"))
}

// Web and README artwork.
try! render("assets/logo.png", 512).write(to: URL(fileURLWithPath: "assets/logo-512.png"))
try! render("assets/logo.png", 256).write(to: URL(fileURLWithPath: "website/logo.png"))
try! render("assets/logo.png", 64).write(to: URL(fileURLWithPath: "website/favicon.png"))
print("wrote icons")
