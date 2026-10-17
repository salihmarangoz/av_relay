# Logo

The AV Relay logo is drawn from plain geometry by `generate.py`: a camera lens
sending two arcs of signal to the right, i.e. one computer's camera and
microphone, relayed to another. Nothing is drawn by hand. To change the logo,
change the numbers at the top of the script and run it again.

```
python3 -m venv .venv && .venv/bin/pip install -r assets/logo/requirements.txt
.venv/bin/python assets/logo/generate.py          # rewrites assets/generated/
.venv/bin/python assets/logo/generate.py --check  # CI: fails if they're stale
```

What it writes to `assets/generated/`:

| file | used by |
|---|---|
| `app-icon.svg`, `app-icon-<N>.png` | docs, release pages |
| `AppIcon.icns`, `AppIcon.iconset/` | the Mac app bundle |
| `macos-menubar/menubar-{off,on,live,disabled}[@2x].png` (+ `.svg`) | the Mac menu bar (template images) |
| `linux/hicolor/...` | the Linux package: the app icon, and the tray icons in four states (`-symbolic.svg`, plus white PNGs for trays that don't recolour symbolic icons) |
| `windows/app.ico`, `windows/tray-{off,on,live,disabled}-{light,dark}.ico` | the Windows executable, installer, shortcuts, and theme-aware system tray |

The four states, the same on all three systems:

- **off**: just the lens (not connected)
- **on**: lens and arcs (connected)
- **live**: filled lens and arcs (the camera or microphone is in use)
- **disabled**: the lens struck through (AV Relay switched off in its menu)

The bitmaps are drawn at 4x and scaled down with Pillow, so they don't depend
on an SVG renderer and come out the same on every machine. PNG compression
differs between platforms, so `--check` compares PNGs by their pixels, and
the script leaves a file alone when only its compression would change.
