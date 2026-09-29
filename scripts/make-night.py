#!/usr/bin/env python3
"""Draws the website's night-hills artwork (paper-cut layers, stars, moon, a kiore on the ridge).

    python3 scripts/make-night.py   → website/art/night.svg
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
    (650, 80, [(1, 1.2), (0.35, 2.9)], 0.4, "#4a5472", "#363e58"),
    (715, 90, [(1, 1.55), (0.3, 3.6)], 2.2, "#3c4461", "#2b3249"),
    (785, 82, [(1, 1.05), (0.4, 2.4)], 4.0, "#303752", "#222840"),
    (855, 72, [(1, 1.8), (0.3, 4.1)], 1.1, "#252b41", "#191e2e"),
    (925, 60, [(1, 1.35), (0.35, 3.3)], 5.2, "#1b2031", "#121623"),
    (990, 45, [(1, 1.7), (0.3, 3.9)], 2.9, "#121622", "#0a0d15"),
]
KIORE_LAYER = 3

out = [f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" preserveAspectRatio="xMidYMax slice">']
out.append("""<defs>
  <linearGradient id="sky" x1="0" y1="0" x2="0" y2="1">
    <stop offset="0" stop-color="#11141f"/>
    <stop offset="0.55" stop-color="#232a40"/>
    <stop offset="1" stop-color="#3d4663"/>
  </linearGradient>
  <radialGradient id="moonglow" cx="0.74" cy="0.23" r="0.36">
    <stop offset="0" stop-color="#dfe5fa" stop-opacity="0.34"/>
    <stop offset="0.4" stop-color="#aeb9df" stop-opacity="0.10"/>
    <stop offset="1" stop-color="#aeb9df" stop-opacity="0"/>
  </radialGradient>
  <radialGradient id="halo">
    <stop offset="0.25" stop-color="#e6ebfb" stop-opacity="0.22"/>
    <stop offset="1" stop-color="#e6ebfb" stop-opacity="0"/>
  </radialGradient>
  <filter id="paper" x="-5%" y="-20%" width="110%" height="140%">
    <feDropShadow dx="0" dy="-7" stdDeviation="14" flood-color="#04060b" flood-opacity="0.7"/>
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
out.append('<g fill="#eef1fb">' + "".join(stars) + "</g>")

# Moon.
# Moon: small and soft, with a halo.
out.append('<circle cx="1184" cy="230" r="95" fill="url(#halo)"/>')
out.append('<circle cx="1184" cy="230" r="26" fill="#eef1fa"/>')
out.append('<circle cx="1177" cy="224" r="5" fill="#cfd6ec" opacity="0.6"/>')
out.append('<circle cx="1192" cy="238" r="3.5" fill="#cfd6ec" opacity="0.5"/>')

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
        f'<path d="{edge}" fill="none" stroke="#c7d0ee" stroke-opacity="{0.14 - i * 0.018:.3f}" stroke-width="1.5"/>'
    )
    if i == KIORE_LAYER:
        # A little kiore on the crest, facing the moon.
        right = [p for p in pts if 900 < p[0] < 1400]
        x0, y0 = min(right, key=lambda p: p[1])
        out.append(
            f'<g transform="translate({x0:.0f} {y0 + 2:.0f}) scale(1.35)" fill="#141926">'
            '<ellipse cx="0" cy="-11" rx="17" ry="12"/>'
            '<circle cx="15" cy="-21" r="8.5"/>'
            '<circle cx="11" cy="-31" r="5.5"/>'
            '<path d="M -15 -6 C -32 -4 -36 -18 -46 -12" fill="none" stroke="#141926" '
            'stroke-width="2.4" stroke-linecap="round"/>'
            "</g>"
        )
    # Mist in front of the farther layers.
    if i < len(layers) - 1:
        out.append(f'<rect x="0" y="{base - 60}" width="{W}" height="200" fill="#8e99bf" opacity="0.03"/>')

out.append(f'<rect width="{W}" height="{H}" filter="url(#grain)" opacity="0.07"/>')
out.append("</svg>")

with open("website/art/night.svg", "w") as f:
    f.write("\n".join(out))
print("wrote website/art/night.svg")
