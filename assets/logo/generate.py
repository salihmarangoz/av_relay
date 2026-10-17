# SPDX-License-Identifier: GPL-3.0-or-later
"""Draws the AV Relay logo from plain geometry and writes every icon the apps
need. The motif: a camera lens sending two arcs of signal to the right, i.e.
the Mac's camera and microphone, relayed.

    python3 -m venv .venv && .venv/bin/pip install pillow
    .venv/bin/python assets/logo/generate.py          # writes assets/generated/
    .venv/bin/python assets/logo/generate.py --check  # fails if out of date

Output (assets/generated/):
- app-icon.svg, app-icon-<N>.png: the full-colour icon (rounded square)
- AppIcon.iconset/ and AppIcon.icns: the same for macOS
- linux/hicolor/...: the app icon plus symbolic tray icons for three states
  (off: lens only; on: lens and arcs; live: filled lens and arcs), and an
  index.theme so a source tree can use them as a theme
- macos-menubar/: black template images for the menu bar, same three states

Everything is drawn at 4x and scaled down, so it doesn't depend on any SVG
renderer and comes out the same on every machine.
"""

import argparse
import io
import math
import struct
import sys
from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "assets" / "generated"
APP_ID = "io.github.salihmarangoz.avrelay"

# Background gradient, top-left to bottom-right.
BG_FROM = (31, 58, 147)  # deep blue
BG_TO = (15, 181, 186)  # teal
FG = (255, 255, 255)

# The motif in a unit square (y down). Strokes are fractions of the size.
LENS_C = (0.40, 0.50)
RING_R, RING_W = 0.195, 0.062
DOT_R = 0.080
LIVE_DOT_R = 0.130
ARCS = [(0.315, 0.062), (0.425, 0.062)]  # (radius, stroke)
ARC_SPAN = 42  # degrees either side of 3 o'clock
# The motif sits inside a padded box in the app icon, and fills the
# glyph icons.
APP_PAD = 0.20
GLYPH_PAD = 0.02

SUPERSAMPLE = 4


def corner_radius(size):
    # Close to macOS's icon shape (a squircle with ~22.4% radius).
    return size * 0.2237


# ---------- geometry shared by the SVG and the bitmap -----------------------


def motif_parts(state):
    """Shapes of the motif for a state, in unit coordinates."""
    parts = [("ring", LENS_C, RING_R, RING_W)]
    if state == "live":
        parts.append(("disc", LENS_C, LIVE_DOT_R, 0))
    else:
        parts.append(("disc", LENS_C, DOT_R, 0))
    if state in ("on", "live"):
        for r, w in ARCS:
            parts.append(("arc", LENS_C, r, w))
    return parts


def motif_bounds():
    """The motif's extent (with arcs), to centre it."""
    cx, cy = LENS_C
    r_out, w = ARCS[-1]
    left = cx - RING_R - RING_W / 2
    right = cx + r_out + w / 2
    half = math.sin(math.radians(ARC_SPAN)) * r_out + w / 2
    top, bottom = cy - max(RING_R + RING_W / 2, half), cy + max(RING_R + RING_W / 2, half)
    return left, top, right, bottom


def placement(pad):
    """Scale and offset that fit the motif, centred, inside a square with
    `pad` (fraction) on every side."""
    left, top, right, bottom = motif_bounds()
    span = max(right - left, bottom - top)
    scale = (1 - 2 * pad) / span
    ox = (1 - (right - left) * scale) / 2 - left * scale
    oy = (1 - (bottom - top) * scale) / 2 - top * scale
    return scale, ox, oy


# ---------- SVG --------------------------------------------------------------


def svg_motif(state, size, pad, colour):
    """The motif as filled paths only: symbolic icons are recoloured by
    their fill (GTK, GNOME Shell), so strokes would keep their colour."""
    scale, ox, oy = placement(pad)
    s = size * scale
    tx, ty = size * ox, size * oy
    f = lambda v: f"{v:.3f}"
    out = []
    for kind, (cx, cy), r, w in motif_parts(state):
        x, y = tx + cx * s, ty + cy * s
        if kind == "ring":
            ro, ri = (r + w / 2) * s, (r - w / 2) * s
            circle = lambda rr: f"M {f(x - rr)} {f(y)} a {f(rr)} {f(rr)} 0 1 0 {f(2 * rr)} 0 a {f(rr)} {f(rr)} 0 1 0 {f(-2 * rr)} 0 Z"
            out.append(f'<path fill="{colour}" fill-rule="evenodd" d="{circle(ro)} {circle(ri)}"/>')
        elif kind == "disc":
            out.append(f'<circle cx="{f(x)}" cy="{f(y)}" r="{f(r * s)}" fill="{colour}"/>')
        else:
            # An annular sector with round caps.
            a = math.radians(ARC_SPAN)
            ro, ri, cap = (r + w / 2) * s, (r - w / 2) * s, w / 2 * s
            pt = lambda rr, ang: (x + rr * math.cos(ang), y + rr * math.sin(ang))
            o0, o1 = pt(ro, -a), pt(ro, a)
            i1, i0 = pt(ri, a), pt(ri, -a)
            d = (f"M {f(o0[0])} {f(o0[1])} A {f(ro)} {f(ro)} 0 0 1 {f(o1[0])} {f(o1[1])} "
                 f"A {f(cap)} {f(cap)} 0 0 1 {f(i1[0])} {f(i1[1])} "
                 f"A {f(ri)} {f(ri)} 0 0 0 {f(i0[0])} {f(i0[1])} "
                 f"A {f(cap)} {f(cap)} 0 0 1 {f(o0[0])} {f(o0[1])} Z")
            out.append(f'<path fill="{colour}" d="{d}"/>')
    return "\n  ".join(out)


def svg_app_icon(size=1024):
    hexc = lambda c: "#%02x%02x%02x" % c
    return f"""<svg xmlns="http://www.w3.org/2000/svg" width="{size}" height="{size}" viewBox="0 0 {size} {size}">
  <defs>
    <linearGradient id="bg" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0" stop-color="{hexc(BG_FROM)}"/>
      <stop offset="1" stop-color="{hexc(BG_TO)}"/>
    </linearGradient>
  </defs>
  <rect width="{size}" height="{size}" rx="{corner_radius(size):.3f}" fill="url(#bg)"/>
  {svg_motif("on", size, APP_PAD, hexc(FG))}
</svg>
"""


def svg_glyph(state, colour="#2e3436", size=16):
    return f"""<svg xmlns="http://www.w3.org/2000/svg" width="{size}" height="{size}" viewBox="0 0 {size} {size}">
  {svg_motif(state, size, GLYPH_PAD, colour)}
</svg>
"""


# ---------- bitmaps ----------------------------------------------------------


def draw_motif(draw, state, size, pad, colour):
    scale, ox, oy = placement(pad)
    s = size * scale
    tx, ty = size * ox, size * oy
    for kind, (cx, cy), r, w in motif_parts(state):
        x, y, rr, ww = tx + cx * s, ty + cy * s, r * s, w * s
        if kind == "ring":
            draw.ellipse([x - rr - ww / 2, y - rr - ww / 2, x + rr + ww / 2, y + rr + ww / 2], fill=colour)
            inner = rr - ww / 2
            draw.ellipse([x - inner, y - inner, x + inner, y + inner], fill=(0, 0, 0, 0))
        elif kind == "disc":
            draw.ellipse([x - rr, y - rr, x + rr, y + rr], fill=colour)
        else:
            draw.arc([x - rr - ww / 2, y - rr - ww / 2, x + rr + ww / 2, y + rr + ww / 2], -ARC_SPAN, ARC_SPAN, fill=colour, width=max(1, round(ww)))
            # Round caps.
            for ang in (-ARC_SPAN, ARC_SPAN):
                a = math.radians(ang)
                px, py = x + rr * math.cos(a), y + rr * math.sin(a)
                draw.ellipse([px - ww / 2, py - ww / 2, px + ww / 2, py + ww / 2], fill=colour)


def render_glyph(state, size, colour):
    big = size * SUPERSAMPLE
    # Draw opaque shapes on a separate layer: the ring's hole must cut
    # through only the motif.
    layer = Image.new("RGBA", (big, big), (0, 0, 0, 0))
    draw_motif(ImageDraw.Draw(layer), state, big, GLYPH_PAD, colour + (255,))
    return layer.resize((size, size), Image.LANCZOS)


def render_app_icon(size):
    big = size * SUPERSAMPLE
    # Diagonal gradient.
    grad = Image.new("RGBA", (big, big))
    px = grad.load()
    steps = 2 * big - 2 or 1
    lut = [tuple(round(a + (b - a) * t / steps) for a, b in zip(BG_FROM, BG_TO)) + (255,) for t in range(2 * big - 1)]
    for yy in range(big):
        for xx in range(big):
            px[xx, yy] = lut[xx + yy]
    mask = Image.new("L", (big, big), 0)
    ImageDraw.Draw(mask).rounded_rectangle([0, 0, big - 1, big - 1], radius=corner_radius(big), fill=255)
    icon = Image.new("RGBA", (big, big), (0, 0, 0, 0))
    icon.paste(grad, (0, 0), mask)
    fg = Image.new("RGBA", (big, big), (0, 0, 0, 0))
    draw_motif(ImageDraw.Draw(fg), "on", big, APP_PAD, FG + (255,))
    icon.alpha_composite(fg)
    return icon.resize((size, size), Image.LANCZOS)


def png_bytes(img):
    buf = io.BytesIO()
    img.save(buf, "PNG", optimize=True)
    return buf.getvalue()


def icns(pngs):
    """An .icns file from PNGs keyed by pixel size (PNG-in-ICNS entries)."""
    kinds = [("icp4", 16), ("icp5", 32), ("icp6", 64), ("ic07", 128), ("ic08", 256), ("ic09", 512), ("ic10", 1024),
             ("ic11", 32), ("ic12", 64), ("ic13", 256), ("ic14", 512)]
    body = b""
    for kind, px in kinds:
        data = pngs[px]
        body += kind.encode() + struct.pack(">I", 8 + len(data)) + data
    return b"icns" + struct.pack(">I", 8 + len(body)) + body


# ---------- output -------------------------------------------------------------


def build():
    """Every output file as {relative path: bytes}."""
    files = {}
    files["app-icon.svg"] = svg_app_icon().encode()
    sizes = [16, 32, 48, 64, 128, 256, 512, 1024]
    pngs = {n: png_bytes(render_app_icon(n)) for n in sizes}
    for n in sizes:
        files[f"app-icon-{n}.png"] = pngs[n]

    # macOS: iconset (iconutil input) and a ready .icns.
    for pt in (16, 32, 128, 256, 512):
        files[f"AppIcon.iconset/icon_{pt}x{pt}.png"] = pngs[pt]
        files[f"AppIcon.iconset/icon_{pt}x{pt}@2x.png"] = pngs[pt * 2]
    files["AppIcon.icns"] = icns(pngs)

    # Linux: hicolor theme layout, ready to copy to /usr/share/icons.
    hi = "linux/hicolor"
    files[f"{hi}/scalable/apps/{APP_ID}.svg"] = svg_app_icon().encode()
    for n in (16, 32, 48, 64, 128, 256, 512):
        files[f"{hi}/{n}x{n}/apps/{APP_ID}.png"] = pngs[n]
    for state in ("off", "on", "live"):
        files[f"{hi}/symbolic/apps/{APP_ID}-{state}-symbolic.svg"] = svg_glyph(state).encode()
        # Bitmaps for trays that don't recolour symbolic SVGs (white on dark panels).
        for n in (22, 44):
            files[f"{hi}/{n}x{n}/status/{APP_ID}-{state}.png"] = png_bytes(render_glyph(state, n, (255, 255, 255)))

    # A theme index, so a source tree's icons work as a theme in an extra
    # search path (GTK recolours symbolic icons only when found through a
    # theme, not as loose files). Installed, the system's hicolor has one.
    dirs = ["symbolic/apps", "scalable/apps"] + [f"{n}x{n}/apps" for n in (16, 32, 48, 64, 128, 256, 512)] + ["22x22/status", "44x44/status"]
    index = "[Icon Theme]\nName=Hicolor\nComment=AV Relay icons from a source tree\nDirectories=" + ",".join(dirs) + "\n"
    for d in dirs:
        if d.startswith(("symbolic", "scalable")):
            index += f"\n[{d}]\nSize=16\nMinSize=8\nMaxSize=512\nType=Scalable\n"
        else:
            n = int(d.split("x")[0])
            index += f"\n[{d}]\nSize={n}\nType=Fixed\n"
    files[f"{hi}/index.theme"] = index.encode()

    # macOS menu bar: template images (black + alpha; macOS tints them).
    for state in ("off", "on", "live"):
        files[f"macos-menubar/menubar-{state}.svg"] = svg_glyph(state, "#000000", 18).encode()
        files[f"macos-menubar/menubar-{state}.png"] = png_bytes(render_glyph(state, 18, (0, 0, 0)))
        files[f"macos-menubar/menubar-{state}@2x.png"] = png_bytes(render_glyph(state, 36, (0, 0, 0)))
    return files


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--check", action="store_true", help="fail if assets/generated is out of date")
    args = ap.parse_args()
    files = build()
    if args.check:
        stale = [p for p, data in files.items() if not (OUT / p).is_file() or (OUT / p).read_bytes() != data]
        if stale:
            print("out of date:", *stale, sep="\n  ")
            sys.exit(1)
        print(f"{len(files)} files up to date")
        return
    for path, data in files.items():
        target = OUT / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
    print(f"wrote {len(files)} files to {OUT.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
