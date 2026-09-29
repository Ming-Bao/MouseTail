// Renders the logo artwork in assets/ into the app's icon and menu bar images.
//   swift scripts/make-icon.swift
import AppKit

func render(_ svg: String, _ px: Int) -> Data {
    guard let image = NSImage(contentsOf: URL(fileURLWithPath: svg)) else { fatalError("can't read \(svg)") }
    let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: px, pixelsHigh: px, bitsPerSample: 8,
                               samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                               colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
    NSGraphicsContext.current?.imageInterpolation = .high
    image.draw(in: NSRect(x: 0, y: 0, width: px, height: px))
    NSGraphicsContext.restoreGraphicsState()
    return rep.representation(using: .png, properties: [:])!
}

let resources = URL(fileURLWithPath: "apps/macos/Resources")
try! FileManager.default.createDirectory(at: resources, withIntermediateDirectories: true)

// App icon.
let iconset = URL(fileURLWithPath: NSTemporaryDirectory()).appendingPathComponent("AppIcon.iconset")
try? FileManager.default.removeItem(at: iconset)
try! FileManager.default.createDirectory(at: iconset, withIntermediateDirectories: true)
for size in [16, 32, 128, 256, 512] {
    try! render("assets/logo.svg", size).write(to: iconset.appendingPathComponent("icon_\(size)x\(size).png"))
    try! render("assets/logo.svg", size * 2).write(to: iconset.appendingPathComponent("icon_\(size)x\(size)@2x.png"))
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
try! render("assets/logo.svg", 512).write(to: URL(fileURLWithPath: "assets/logo-512.png"))
print("wrote icons")
