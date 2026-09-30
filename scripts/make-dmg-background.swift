// Draws the background of the Mac download's window: warm paper with soft glows in the logo's
// colours, and the logo's trail looping from MouseTail across to Applications. Light, because
// Finder draws the icons' names in black over a background picture.
//   swift scripts/make-dmg-background.swift <out.png> <scale>
// The layout matches scripts/package-mac.sh: a 640 × 400 point window, MouseTail's icon
// centred at 160,170 and Applications' at 480,170.
import AppKit

let (width, height) = (640.0, 400.0)
let out = CommandLine.arguments[1]
let scale = Double(CommandLine.arguments[2]) ?? 1

let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(width * scale), pixelsHigh: Int(height * scale),
                           bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                           colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
rep.size = NSSize(width: width, height: height)
NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
// Draw top-down, like Finder's coordinates.
let flip = NSAffineTransform()
flip.translateX(by: 0, yBy: height)
flip.scaleX(by: 1, yBy: -1)
flip.concat()

func color(_ hex: Int, _ alpha: Double = 1) -> NSColor {
    NSColor(srgbRed: Double(hex >> 16 & 0xff) / 255, green: Double(hex >> 8 & 0xff) / 255,
            blue: Double(hex & 0xff) / 255, alpha: alpha)
}

// Paper, a touch warmer towards the bottom, with glows from the logo: the ear's coral behind
// MouseTail, the trail's gold behind Applications, a little rose between.
NSGradient(colors: [color(0xfbfaf6), color(0xf2efe6)])!.draw(in: NSRect(x: 0, y: 0, width: width, height: height), angle: -90)
func glow(_ x: Double, _ y: Double, _ r: Double, _ hex: Int, _ alpha: Double) {
    NSGradient(colors: [color(hex, alpha), color(hex, 0)])!
        .draw(fromCenter: NSPoint(x: x, y: y), radius: 0, toCenter: NSPoint(x: x, y: y), radius: r, options: [])
}
glow(160, 170, 200, 0xffa47e, 0.22)
glow(480, 170, 200, 0xffe06e, 0.24)
glow(320, 120, 140, 0xe593af, 0.12)

// The trail: a loop, as in the logo, then on to Applications.
let trail = NSBezierPath()
trail.move(to: NSPoint(x: 238, y: 185))
trail.curve(to: NSPoint(x: 318, y: 131), controlPoint1: NSPoint(x: 272, y: 199), controlPoint2: NSPoint(x: 322, y: 169))
trail.curve(to: NSPoint(x: 282, y: 125), controlPoint1: NSPoint(x: 314, y: 103), controlPoint2: NSPoint(x: 284, y: 103))
trail.curve(to: NSPoint(x: 332, y: 181), controlPoint1: NSPoint(x: 280, y: 151), controlPoint2: NSPoint(x: 300, y: 181))
trail.curve(to: NSPoint(x: 398, y: 170), controlPoint1: NSPoint(x: 358, y: 181), controlPoint2: NSPoint(x: 380, y: 174))
let head = NSBezierPath()
head.move(to: NSPoint(x: 384, y: 157))
head.line(to: NSPoint(x: 401, y: 169.5))
head.line(to: NSPoint(x: 386, y: 184))
for path in [trail, head] {
    path.lineCapStyle = .round
    path.lineJoinStyle = .round
    path.lineWidth = 3.5
}
// Glowing: a wide soft halo, a tighter one, then the stroke itself.
for (alpha, blur) in [(0.9, 16.0), (0.7, 5.0), (0.0, 0.0)] {
    NSGraphicsContext.saveGraphicsState()
    let shadow = NSShadow()
    shadow.shadowColor = color(0xffc93c, alpha)
    shadow.shadowBlurRadius = blur * scale
    shadow.set()
    color(0xf0b429).setStroke()
    for path in [trail, head] { path.stroke() }
    NSGraphicsContext.restoreGraphicsState()
}

// The instruction, under the icons' names.
func text(_ string: String, y: Double, size: Double, weight: NSFont.Weight, alpha: Double) {
    let paragraph = NSMutableParagraphStyle()
    paragraph.alignment = .center
    let attributes: [NSAttributedString.Key: Any] = [
        .font: NSFont.systemFont(ofSize: size, weight: weight),
        .foregroundColor: color(0x1b1c1c, alpha),
        .paragraphStyle: paragraph,
    ]
    // Text only draws upright in a flipped context if the context says it's flipped.
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(cgContext: NSGraphicsContext.current!.cgContext, flipped: true)
    NSAttributedString(string: string, attributes: attributes)
        .draw(in: NSRect(x: 0, y: y, width: width, height: size * 2))
    NSGraphicsContext.restoreGraphicsState()
}
text("Drag MouseTail into Applications", y: 306, size: 15, weight: .semibold, alpha: 0.88)
text("Then open it, and look for the mouse in your menu bar", y: 330, size: 12, weight: .regular, alpha: 0.5)

NSGraphicsContext.restoreGraphicsState()
try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: out))
