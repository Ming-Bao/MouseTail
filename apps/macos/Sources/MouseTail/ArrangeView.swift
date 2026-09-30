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
            Text("Drag each computer to where it sits on your desk. Push the cursor off a highlighted edge to move to the other computer.")
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            GeometryReader { geo in
                if let layout = model.layout {
                    canvas(layout, size: geo.size)
                } else {
                    ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            }
            .frame(minWidth: 420, idealWidth: 560, minHeight: 260, idealHeight: 340)
            .background(RoundedRectangle(cornerRadius: 12).fill(Color(nsColor: .underPageBackgroundColor)))
            .clipShape(RoundedRectangle(cornerRadius: 12))

            HStack {
                legend
                Spacer()
                Button("Done") { dismiss() }
                    .keyboardShortcut(.defaultAction)
            }
        }
        .padding(20)
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
                Capsule().fill(Color.accentColor).frame(width: 14, height: 4)
            }
        }
        .font(.caption)
        .foregroundStyle(.secondary)
    }

    private func swatch(_ kind: Kind) -> some View {
        RoundedRectangle(cornerRadius: 2).fill(kind.fill).frame(width: 14, height: 10)
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
                    .stroke(Color.accentColor, style: StrokeStyle(lineWidth: 5, lineCap: .round))
                    .shadow(color: Color.accentColor.opacity(0.6), radius: 4)
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
                kind: machine.this ? .thisMac : (machine.connected ? .other : .offline),
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

enum Kind {
    case thisMac, other, offline

    var fill: Color {
        switch self {
        case .thisMac: return Color(red: 0.27, green: 0.52, blue: 0.95)
        case .other: return Color(red: 0.55, green: 0.40, blue: 0.90)
        case .offline: return Color.gray.opacity(0.6)
        }
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
                .fill(kind.fill.gradient)
            if display.primary {
                // A menu bar strip marks each computer's main display, as macOS does.
                UnevenRoundedRectangle(topLeadingRadius: 5, topTrailingRadius: 5)
                    .fill(.white.opacity(0.85))
                    .frame(height: 6)
            }
            VStack(spacing: 2) {
                Text(machine.this ? "This Mac" : machine.name)
                    .font(.callout.weight(.semibold))
                Text(kind == .offline ? "Offline" : (machine.this ? Self.localName(display) : display.name))
                    .font(.caption2)
                    .opacity(0.85)
            }
            .foregroundStyle(.white)
            .lineLimit(1)
            .minimumScaleFactor(0.5)
            .padding(6)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        .overlay(RoundedRectangle(cornerRadius: 5).stroke(.white.opacity(lifted ? 0.9 : 0.35), lineWidth: lifted ? 2 : 1))
        .shadow(color: .black.opacity(lifted ? 0.35 : 0.15), radius: lifted ? 10 : 2, y: lifted ? 6 : 1)
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
