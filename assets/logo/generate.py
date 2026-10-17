# SPDX-License-Identifier: GPL-3.0-or-later
"""Draws the AV Relay logo from plain geometry and writes every icon the apps
need. The motif: a camera lens sending two arcs of signal to the right, i.e.
one computer's camera and microphone, relayed to another.

    python3 -m venv .venv && .venv/bin/pip install -r assets/logo/requirements.txt
    .venv/bin/python assets/logo/generate.py          # writes assets/generated/
    .venv/bin/python assets/logo/generate.py --check  # fails if out of date

Output (assets/generated/):
- app-icon.svg, app-icon-<N>.png: the full-colour icon (rounded square)
- AppIcon.iconset/ and AppIcon.icns: the same for macOS, inside Apple's
  margin with its shadow
- linux/hicolor/...: the app icon plus symbolic tray icons for four states
  (off: lens only; on: lens and arcs; live: filled lens and arcs; disabled:
  the lens struck through), and an index.theme so a source tree can use them
  as a theme
- macos-menubar/: black template images for the menu bar, same four states

Everything is drawn at 4x and scaled down, so it doesn't depend on any SVG
renderer and comes out the same on every machine. (PNG compression differs
between platforms, so --check compares PNGs by their pixels.)
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
# Disabled: a slash from top left to bottom right across the lens, with a
# transparent gap along its upper side so it reads over the ring.
SLASH_W, SLASH_GAP = 0.062, 0.045
# From the lens centre along each axis: the caps end at the ring's bounding
# box, which keeps them inside the glyph.
SLASH_REACH = RING_R + RING_W / 2 - SLASH_W / 2
# The motif sits inside a padded box in the app icon, and fills the
# glyph icons.
APP_PAD = 0.20
GLYPH_PAD = 0.02

SUPERSAMPLE = 4

GLYPH_STATES = ("off", "on", "live", "disabled")


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


def slash_line(scale, ox, oy, size):
    """The disabled state's slash: its ends, in pixels."""
    cx, cy = LENS_C
    to_px = lambda u, v: (size * (ox + u * scale), size * (oy + v * scale))
    return to_px(cx - SLASH_REACH, cy - SLASH_REACH), to_px(cx + SLASH_REACH, cy + SLASH_REACH)


def slash_polygon(p0, p1, half, shift):
    """A stroke from p0 to p1 of half-width `half` as a polygon (no caps),
    moved by `shift` (pixels) along its upper-right normal."""
    dx, dy = p1[0] - p0[0], p1[1] - p0[1]
    n = math.hypot(dx, dy)
    nx, ny = dy / n, -dx / n  # perpendicular, pointing up and right
    cx, cy = nx * shift, ny * shift
    return [(p0[0] + nx * half + cx, p0[1] + ny * half + cy), (p1[0] + nx * half + cx, p1[1] + ny * half + cy),
            (p1[0] - nx * half + cx, p1[1] - ny * half + cy), (p0[0] - nx * half + cx, p0[1] - ny * half + cy)]


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
    if state != "disabled":
        return f"""<svg xmlns="http://www.w3.org/2000/svg" width="{size}" height="{size}" viewBox="0 0 {size} {size}">
  {svg_motif(state, size, GLYPH_PAD, colour)}
</svg>
"""
    scale, ox, oy = placement(GLYPH_PAD)
    p0, p1 = slash_line(scale, ox, oy, size)
    w, gap = SLASH_W * size * scale, SLASH_GAP * size * scale
    # Only <path> and <circle>, and the gap as a clip: a symbolic icon gets
    # recoloured by forcing the fill of its shapes (GNOME Shell's tray does
    # it for rect, circle and path only), which would also hit a mask's
    # content. A clip uses only the geometry.
    poly = lambda pts: "M " + " L ".join(f"{x:.3f} {y:.3f}" for x, y in pts) + " Z"
    cut = slash_polygon(p0, p1, w / 2 + gap / 2, gap / 2)
    caps = "".join(f'<circle cx="{x:.3f}" cy="{y:.3f}" r="{w / 2:.3f}" fill="{colour}"/>' for x, y in (p0, p1))
    return f"""<svg xmlns="http://www.w3.org/2000/svg" width="{size}" height="{size}" viewBox="0 0 {size} {size}">
  <clipPath id="gap">
    <path clip-rule="evenodd" d="M 0 0 H {size} V {size} H 0 Z {poly(cut)}"/>
  </clipPath>
  <g clip-path="url(#gap)">
  {svg_motif(state, size, GLYPH_PAD, colour)}
  </g>
  <path fill="{colour}" d="{poly(slash_polygon(p0, p1, w / 2, 0))}"/>{caps}
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
    draw = ImageDraw.Draw(layer)
    draw_motif(draw, state, big, GLYPH_PAD, colour + (255,))
    if state == "disabled":
        scale, ox, oy = placement(GLYPH_PAD)
        p0, p1 = slash_line(scale, ox, oy, big)
        w, gap = SLASH_W * big * scale, SLASH_GAP * big * scale
        draw.polygon(slash_polygon(p0, p1, w / 2 + gap / 2, gap / 2), fill=(0, 0, 0, 0))
        draw.polygon(slash_polygon(p0, p1, w / 2, 0), fill=colour + (255,))
        for x, y in (p0, p1):
            draw.ellipse([x - w / 2, y - w / 2, x + w / 2, y + w / 2], fill=colour + (255,))
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


def render_mac_icon(size):
    """The app icon as macOS (Big Sur and later) draws its icons: the rounded
    square at 824/1024 of the canvas, centred, over a soft shadow (Apple's
    template: 10 px down, 10 px blur, 30% black at 1024). Our corner radius
    (22.37% of the square) already matches Apple's 185/824."""
    from PIL import ImageFilter

    inner = round(size * 824 / 1024)
    margin = (size - inner) // 2
    big, m4, i4 = size * SUPERSAMPLE, margin * SUPERSAMPLE, inner * SUPERSAMPLE
    off = round(big * 10 / 1024)
    shadow = Image.new("RGBA", (big, big), (0, 0, 0, 0))
    ImageDraw.Draw(shadow).rounded_rectangle([m4, m4 + off, m4 + i4 - 1, m4 + i4 - 1 + off], radius=corner_radius(i4), fill=(0, 0, 0, 77))
    canvas = shadow.filter(ImageFilter.GaussianBlur(big * 10 / 1024)).resize((size, size), Image.LANCZOS)
    canvas.alpha_composite(render_app_icon(inner), (margin, margin))
    return canvas


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


def ico(pngs):
    """Windows multi-resolution icon, with PNG payloads (Vista and newer)."""
    offset = 6 + 16 * len(pngs)
    directory, body = b"", b""
    for size, data in sorted(pngs.items()):
        directory += struct.pack("<BBBBHHII", size if size < 256 else 0,
                                 size if size < 256 else 0, 0, 0, 1, 32, len(data), offset)
        body += data
        offset += len(data)
    return struct.pack("<HHH", 0, 1, len(pngs)) + directory + body


# ---------- output -------------------------------------------------------------


def build():
    """Every output file as {relative path: bytes}."""
    files = {}
    files["app-icon.svg"] = svg_app_icon().encode()
    sizes = [16, 32, 48, 64, 128, 256, 512, 1024]
    pngs = {n: png_bytes(render_app_icon(n)) for n in sizes}
    for n in sizes:
        files[f"app-icon-{n}.png"] = pngs[n]

    files["windows/app.ico"] = ico({n: pngs[n] for n in (16, 32, 48, 64, 128, 256)})
    for state in GLYPH_STATES:
        for theme, colour in (("light", (32, 32, 32)), ("dark", (255, 255, 255))):
            files[f"windows/tray-{state}-{theme}.ico"] = ico({
                n: png_bytes(render_glyph(state, n, colour)) for n in (16, 20, 24, 32, 40, 48)
            })

    # macOS: iconset (iconutil input) and a ready .icns, with Apple's margin.
    mac = {n: png_bytes(render_mac_icon(n)) for n in sizes}
    for pt in (16, 32, 128, 256, 512):
        files[f"AppIcon.iconset/icon_{pt}x{pt}.png"] = mac[pt]
        files[f"AppIcon.iconset/icon_{pt}x{pt}@2x.png"] = mac[pt * 2]
    files["AppIcon.icns"] = icns(mac)

    # Linux: hicolor theme layout, ready to copy to /usr/share/icons.
    hi = "linux/hicolor"
    files[f"{hi}/scalable/apps/{APP_ID}.svg"] = svg_app_icon().encode()
    for n in (16, 32, 48, 64, 128, 256, 512):
        files[f"{hi}/{n}x{n}/apps/{APP_ID}.png"] = pngs[n]
    for state in GLYPH_STATES:
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
    for state in GLYPH_STATES:
        files[f"macos-menubar/menubar-{state}.svg"] = svg_glyph(state, "#000000", 18).encode()
        files[f"macos-menubar/menubar-{state}.png"] = png_bytes(render_glyph(state, 18, (0, 0, 0)))
        files[f"macos-menubar/menubar-{state}@2x.png"] = png_bytes(render_glyph(state, 36, (0, 0, 0)))
    return files


def pixels(data):
    img = Image.open(io.BytesIO(data))
    return img.size, img.mode, img.tobytes()


def icns_entries(data):
    """An .icns file's entries: [(kind, PNG bytes)]."""
    entries, at = [], 8
    while at < len(data):
        kind, length = data[at:at + 4], struct.unpack(">I", data[at + 4:at + 8])[0]
        entries.append((kind, data[at + 8:at + length]))
        at += length
    return entries


def same(path, new, old):
    """Whether a file on disk matches what the script makes now. PNGs (also
    inside the .icns) by their pixels: zlib's output differs by platform."""
    if new == old:
        return True
    if path.endswith(".svg") or path.endswith("index.theme"):
        return new.replace(b"\r\n", b"\n") == old.replace(b"\r\n", b"\n")
    if path.endswith(".png"):
        return pixels(new) == pixels(old)
    if path.endswith(".icns"):
        a, b = icns_entries(new), icns_entries(old)
        return len(a) == len(b) and all(ka == kb and pixels(da) == pixels(db) for (ka, da), (kb, db) in zip(a, b))
    if path.endswith(".ico"):
        def entries(data):
            count = struct.unpack_from("<H", data, 4)[0]
            return [pixels(data[offset:offset+length]) for i in range(count)
                    for length, offset in [struct.unpack_from("<II", data, 6 + i*16 + 8)]]
        return entries(new) == entries(old)
    return False


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--check", action="store_true", help="fail if assets/generated is out of date")
    args = ap.parse_args()
    files = build()
    if args.check:
        stale = [p for p, data in files.items() if not (OUT / p).is_file() or not same(p, data, (OUT / p).read_bytes())]
        if stale:
            print("out of date:", *stale, sep="\n  ")
            sys.exit(1)
        print(f"{len(files)} files up to date")
        return
    written = 0
    for path, data in files.items():
        target = OUT / path
        # Unchanged files stay as they are, so another platform's zlib doesn't touch them.
        if target.is_file() and same(path, data, target.read_bytes()):
            continue
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
        written += 1
    print(f"wrote {written} of {len(files)} files to {OUT.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
