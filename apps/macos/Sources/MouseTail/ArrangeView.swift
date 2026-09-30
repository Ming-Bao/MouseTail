import SwiftUI

/// Drag other computers to where they sit on your desk, like System Settings → Displays →
/// Arrange. Drops snap to the nearest edge; highlighted edges are where the cursor crosses.
struct ArrangeView: View {
    @EnvironmentObject var model: AppModel
    @Environment(\.dismiss) private var dismiss

    /// Machine being dragged and how far (in view points).
    @State private var dragging: String?
    @State private var translation: CGSize = .zero
    /// The view transform is frozen during a drag so the canvas doesn't rescale under you.
    @State private var frozen: Transform?

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Arrange Displays").font(.title2.weight(.semibold))
                .foregroundStyle(Brand.text)
            Text("Drag each computer to where it sits on your desk. Push the cursor off a highlighted edge to move to the other computer.")
                .foregroundStyle(Brand.muted)
                .fixedSize(horizontal: false, vertical: true)

            GeometryReader { geo in
                if let layout = model.layout {
                    canvas(layout, size: geo.size)
                } else {
                    ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            }
            .frame(minWidth: 420, idealWidth: 560, minHeight: 260, idealHeight: 340)
            .background(RoundedRectangle(cornerRadius: 12).fill(Brand.card))
            .overlay(RoundedRectangle(cornerRadius: 12).stroke(Brand.line))
            .clipShape(RoundedRectangle(cornerRadius: 12))

            HStack {
                legend
                Spacer()
                Button("Done") { dismiss() }
                    .buttonStyle(GlowButton())
                    .keyboardShortcut(.defaultAction)
            }
        }
        .padding(20)
        .background(Brand.background)
        // The brand is dark, whatever the Mac's appearance.
        .preferredColorScheme(.dark)
        // Bounded on every side: an unbounded canvas lets macOS size the window to the screen.
        .frame(minWidth: 480, idealWidth: 620, maxWidth: 1400,
               minHeight: 380, idealHeight: 460, maxHeight: 1000)
        .task {
            while !Task.isCancelled {
                if dragging == nil { await model.refreshLayout() }
                try? await Task.sleep(for: .seconds(2))
            }
        }
    }

    private var legend: some View {
        HStack(spacing: 14) {
            Label { Text("This Mac") } icon: { swatch(.thisMac) }
            Label { Text("Other computers") } icon: { swatch(.other) }
            Label { Text("Cursor crosses here") } icon: {
                Capsule().fill(Brand.glow).frame(width: 14, height: 3)
                    .shadow(color: Brand.glowSoft, radius: 3)
            }
        }
        .font(.caption)
        .foregroundStyle(Brand.muted)
    }

    private func swatch(_ kind: Kind) -> some View {
        RoundedRectangle(cornerRadius: 2).fill(kind.fill)
            .overlay(RoundedRectangle(cornerRadius: 2).stroke(.white.opacity(kind.border)))
            .frame(width: 14, height: 10)
    }

    // MARK: Canvas

    private func canvas(_ layout: LayoutInfo, size: CGSize) -> some View {
        let t = frozen ?? Transform(fitting: layout, in: size)
        return ZStack(alignment: .topLeading) {
            ForEach(layout.shown) { machine in
                machineView(machine, t: t)
            }
            if dragging == nil {
                ForEach(Array((layout.crossings ?? []).enumerated()), id: \.offset) { _, edge in
                    Path { p in
                        guard edge.count == 2 else { return }
                        p.move(to: t.view(edge[0]))
                        p.addLine(to: t.view(edge[1]))
                    }
                    .stroke(Brand.glow, style: StrokeStyle(lineWidth: 3, lineCap: .round))
                    .shadow(color: Brand.glowSoft, radius: 6)
                    .allowsHitTesting(false)
                }
            }
        }
        .frame(width: size.width, height: size.height, alignment: .topLeading)
    }

    private func machineView(_ machine: Machine, t: Transform) -> some View {
        let shift = dragging == machine.id ? translation : .zero
        return ForEach(machine.displays) { display in
            let r = display.rect.offset(by: machine.offset ?? Point(x: 0, y: 0))
            let frame = t.view(r)
            DisplayTile(
                machine: machine,
                display: display,
                kind: machine.this ? .thisMac : (machine.connected && machine.paused != true ? .other : .offline),
                lifted: dragging == machine.id
            )
            .frame(width: frame.width, height: frame.height)
            .position(x: frame.midX + shift.width, y: frame.midY + shift.height)
            .gesture(machine.this ? nil : drag(machine, t: t))
        }
    }

    private func drag(_ machine: Machine, t: Transform) -> some Gesture {
        DragGesture(minimumDistance: 2)
            .onChanged { value in
                if dragging == nil {
                    frozen = t
                    dragging = machine.id
                }
                translation = value.translation
            }
            .onEnded { value in
                let offset = machine.offset ?? Point(x: 0, y: 0)
                let desired = Point(
                    x: offset.x + Double(value.translation.width) / t.scale,
                    y: offset.y + Double(value.translation.height) / t.scale
                )
                Task {
                    // The layout arrives already moved to where it snapped; shift the tile
                    // back to where it was let go, so it glides from there rather than
                    // jumping by the drag distance first.
                    if let snapped = await model.place(machine, at: desired) {
                        translation = CGSize(
                            width: (desired.x - snapped.x) * t.scale,
                            height: (desired.y - snapped.y) * t.scale
                        )
                    }
                    withAnimation(.spring(duration: 0.25)) {
                        dragging = nil
                        translation = .zero
                        frozen = nil
                    }
                }
            }
    }
}

/// Maps layout points to view points, fitting everything with a margin.
struct Transform: Equatable {
    var scale: Double
    var origin: CGPoint
    var bounds: Rect

    init(fitting layout: LayoutInfo, in size: CGSize) {
        let rects = layout.shown.flatMap(\.placed)
        let b = rects.dropFirst().reduce(rects.first ?? Rect(x: 0, y: 0, w: 1, h: 1)) { $0.union($1) }
        let margin = 40.0
        let s = min((Double(size.width) - 2 * margin) / b.w, (Double(size.height) - 2 * margin) / b.h)
        scale = max(s, 0.01)
        bounds = b
        origin = CGPoint(
            x: (Double(size.width) - b.w * scale) / 2,
            y: (Double(size.height) - b.h * scale) / 2
        )
    }

    func view(_ p: Point) -> CGPoint {
        CGPoint(x: origin.x + (p.x - bounds.x) * scale, y: origin.y + (p.y - bounds.y) * scale)
    }

    func view(_ r: Rect) -> CGRect {
        let o = view(Point(x: r.x, y: r.y))
        return CGRect(x: o.x, y: o.y, width: r.w * scale, height: r.h * scale)
    }
}

/// MouseTail's colours, as on the website: near-black, warm off-white, and the logo's glowing
/// yellow tail for where the cursor crosses.
enum Brand {
    static let background = Color(red: 0.031, green: 0.031, blue: 0.031) // #080808
    static let card = Color(red: 0.067, green: 0.071, blue: 0.071) // #111212
    static let text = Color(red: 0.949, green: 0.945, blue: 0.925) // #f2f1ec
    static let muted = text.opacity(0.64)
    static let faint = text.opacity(0.42)
    static let line = text.opacity(0.09)
    static let glow = Color(red: 1.0, green: 0.922, blue: 0.655) // #ffeba7
    static let glowSoft = Color(red: 1.0, green: 0.878, blue: 0.431).opacity(0.55)
}

/// Each kind of display tile: graphite for this Mac, warm for the others (as on the website's
/// arrangement), dimmer when offline.
enum Kind {
    case thisMac, other, offline

    var fill: LinearGradient {
        let (top, bottom): (Color, Color) = switch self {
        case .thisMac: (Color(red: 0.220, green: 0.224, blue: 0.224), Color(red: 0.161, green: 0.165, blue: 0.165))
        case .other: (Color(red: 0.290, green: 0.271, blue: 0.208), Color(red: 0.208, green: 0.196, blue: 0.149))
        case .offline: (Color(red: 0.137, green: 0.141, blue: 0.141), Color(red: 0.106, green: 0.110, blue: 0.110))
        }
        return LinearGradient(colors: [top, bottom], startPoint: .top, endPoint: .bottom)
    }

    /// Edge opacity (white).
    var border: Double {
        switch self {
        case .thisMac: return 0.12
        case .other: return 0.18
        case .offline: return 0.08
        }
    }

    var label: Color {
        switch self {
        case .thisMac: return Brand.muted
        case .other: return Brand.text
        case .offline: return Brand.faint
        }
    }
}

/// The website's light pill, glowing: for the window's one button.
struct GlowButton: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.callout.weight(.medium))
            .foregroundStyle(Brand.background)
            .padding(.horizontal, 16)
            .padding(.vertical, 6)
            .background(Capsule().fill(Brand.glow.opacity(configuration.isPressed ? 0.8 : 1)))
            .shadow(color: Brand.glowSoft.opacity(0.6), radius: 6)
    }
}

struct DisplayTile: View {
    let machine: Machine
    let display: Display
    let kind: Kind
    let lifted: Bool
    /// Whether we've pushed the open-hand cursor (so it's popped exactly once).
    @State private var hovering = false

    /// macOS's own name for one of this Mac's displays (e.g. "DELL P2715Q").
    static func localName(_ display: Display) -> String {
        let screen = NSScreen.screens.first {
            ($0.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? NSNumber)?.stringValue == display.id
        }
        return screen?.localizedName ?? display.name
    }

    var body: some View {
        ZStack(alignment: .top) {
            RoundedRectangle(cornerRadius: 5)
                .fill(kind.fill)
            if display.primary {
                // A menu bar strip marks each computer's main display, as macOS does.
                UnevenRoundedRectangle(topLeadingRadius: 5, topTrailingRadius: 5)
                    .fill(Brand.text.opacity(0.45))
                    .frame(height: 5)
            }
            VStack(spacing: 2) {
                Text(machine.this ? "This Mac" : machine.name)
                    .font(.callout.weight(.semibold))
                Text(kind == .offline ? "Offline" : (machine.this ? Self.localName(display) : display.name))
                    .font(.caption2)
                    .opacity(0.85)
            }
            .foregroundStyle(kind.label)
            .lineLimit(1)
            .minimumScaleFactor(0.5)
            .padding(6)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        .overlay(
            RoundedRectangle(cornerRadius: 5)
                .stroke(lifted ? Brand.glow : .white.opacity(kind.border), lineWidth: lifted ? 1.5 : 1)
        )
        .shadow(color: lifted ? Brand.glowSoft : .black.opacity(0.3), radius: lifted ? 12 : 2, y: lifted ? 4 : 1)
        .contentShape(Rectangle())
        .onHover { inside in
            guard !machine.this, inside != hovering else { return }
            if inside { NSCursor.openHand.push() } else { NSCursor.pop() }
            hovering = inside
        }
        // Gone from under the pointer (unpaired, window closed): don't leave the hand behind.
        .onDisappear {
            if hovering {
                NSCursor.pop()
                hovering = false
            }
        }
    }
}
