#!/usr/bin/env python3
"""Draws the website's night-hills artwork (paper-cut layers, stars, moon).

    python3 scripts/make-night.py   → website/art/night.svg
    scripts/render-night.sh         → the .webp images the site shows
"""
import math
import random

W, H = 1600, 1000
rng = random.Random(7)


def ridge(base, amp, waves, phase, points=48):
    """A smooth hill line: sum of sines, closed down to the bottom of the frame."""
    pts = []
    for i in range(points + 1):
        x = W * i / points
        y = base
        for k, (a, f) in enumerate(waves):
            y -= amp * a * math.sin(x / W * math.pi * f + phase + k * 1.7)
        pts.append((x, y))
    d = f"M {-20} {H + 20} L {-20} {pts[0][1]:.1f} "
    # Catmull-Rom → cubic Béziers for soft curves.
    for i in range(len(pts) - 1):
        p0 = pts[max(i - 1, 0)]
        p1, p2 = pts[i], pts[i + 1]
        p3 = pts[min(i + 2, len(pts) - 1)]
        c1 = (p1[0] + (p2[0] - p0[0]) / 6, p1[1] + (p2[1] - p0[1]) / 6)
        c2 = (p2[0] - (p3[0] - p1[0]) / 6, p2[1] - (p3[1] - p1[1]) / 6)
        d += f"C {c1[0]:.1f} {c1[1]:.1f} {c2[0]:.1f} {c2[1]:.1f} {p2[0]:.1f} {p2[1]:.1f} "
    d += f"L {W + 20} {H + 20} Z"
    return d, pts


layers = [
    # base, amplitude, waves (weight, frequency), phase, top colour, bottom colour
    (650, 80, [(1, 1.2), (0.35, 2.9)], 0.4, "#494945", "#373734"),
    (715, 90, [(1, 1.55), (0.3, 3.6)], 2.2, "#3b3b38", "#2c2c2a"),
    (785, 82, [(1, 1.05), (0.4, 2.4)], 4.0, "#302f2d", "#232322"),
    (855, 72, [(1, 1.8), (0.3, 4.1)], 1.1, "#252524", "#1a1a19"),
    (925, 60, [(1, 1.35), (0.35, 3.3)], 5.2, "#1b1b1a", "#121212"),
    (990, 45, [(1, 1.7), (0.3, 3.9)], 2.9, "#121212", "#0a0a0a"),
]

out = [f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" preserveAspectRatio="xMidYMax slice">']
out.append("""<defs>
  <linearGradient id="sky" x1="0" y1="0" x2="0" y2="1">
    <stop offset="0" stop-color="#0f1010"/>
    <stop offset="0.55" stop-color="#1e1f1f"/>
    <stop offset="1" stop-color="#34342f"/>
  </linearGradient>
  <radialGradient id="moonglow" cx="0.74" cy="0.23" r="0.36">
    <stop offset="0" stop-color="#ffeba7" stop-opacity="0.30"/>
    <stop offset="0.4" stop-color="#e8c96e" stop-opacity="0.09"/>
    <stop offset="1" stop-color="#e8c96e" stop-opacity="0"/>
  </radialGradient>
  <radialGradient id="halo">
    <stop offset="0.25" stop-color="#ffeba7" stop-opacity="0.26"/>
    <stop offset="1" stop-color="#ffeba7" stop-opacity="0"/>
  </radialGradient>
  <filter id="paper" x="-5%" y="-20%" width="110%" height="140%">
    <feDropShadow dx="0" dy="-7" stdDeviation="14" flood-color="#050505" flood-opacity="0.7"/>
  </filter>
  <filter id="grain">
    <feTurbulence type="fractalNoise" baseFrequency="0.9" numOctaves="2" stitchTiles="stitch"/>
    <feColorMatrix values="0 0 0 0 1  0 0 0 0 1  0 0 0 0 1  0 0 0 0.9 0"/>
  </filter>
</defs>""")
out.append(f'<rect width="{W}" height="{H}" fill="url(#sky)"/>')
out.append(f'<rect width="{W}" height="{H}" fill="url(#moonglow)"/>')

# Stars, denser and brighter higher up.
stars = []
for _ in range(170):
    x, y = rng.uniform(0, W), rng.uniform(0, 620) ** 1.08 * 0.9
    r = rng.choice([0.7, 0.8, 1.0, 1.2, 1.5, 1.9])
    o = max(0.15, min(0.9, rng.uniform(0.2, 0.9) * (1 - y / 800)))
    stars.append(f'<circle cx="{x:.0f}" cy="{y:.0f}" r="{r}" opacity="{o:.2f}"/>')
out.append('<g fill="#fff6e0">' + "".join(stars) + "</g>")

# Moon.
# Moon: small and soft, with a halo.
out.append('<circle cx="1184" cy="230" r="95" fill="url(#halo)"/>')
out.append('<circle cx="1184" cy="230" r="26" fill="#fff4d2"/>')
out.append('<circle cx="1177" cy="224" r="5" fill="#e9d9a8" opacity="0.6"/>')
out.append('<circle cx="1192" cy="238" r="3.5" fill="#e9d9a8" opacity="0.5"/>')

for i, (base, amp, waves, phase, top, bottom) in enumerate(layers):
    d, pts = ridge(base, amp, waves, phase)
    out.append(
        f'<linearGradient id="h{i}" x1="0" y1="0" x2="0" y2="1">'
        f'<stop offset="0" stop-color="{top}"/><stop offset="1" stop-color="{bottom}"/></linearGradient>'
    )
    out.append(f'<path d="{d}" fill="url(#h{i})" filter="url(#paper)"/>')
    # A faint moonlit edge along the top of each layer.
    edge = "M " + " L ".join(f"{x:.1f} {y:.1f}" for x, y in pts)
    out.append(
        f'<path d="{edge}" fill="none" stroke="#f0e2bd" stroke-opacity="{0.14 - i * 0.018:.3f}" stroke-width="1.5"/>'
    )
    # Mist in front of the farther layers.
    if i < len(layers) - 1:
        out.append(f'<rect x="0" y="{base - 60}" width="{W}" height="200" fill="#a9a597" opacity="0.03"/>')

out.append(f'<rect width="{W}" height="{H}" filter="url(#grain)" opacity="0.07"/>')
out.append("</svg>")

with open("website/art/night.svg", "w") as f:
    f.write("\n".join(out))
print("wrote website/art/night.svg")
