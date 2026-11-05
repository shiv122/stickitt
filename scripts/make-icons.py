"""Builds every icon from the master logo (a 1024x1024 PNG with transparent padding).

    python scripts/make-icons.py [path/to/logo.png]

Writes, in assets/:
  icon.png      1024x1024 RGBA master: the macOS app icon (scripts/package-macos.sh turns it
                into AppIcon.icns) and the README logo.
  stickitt.ico  Windows icon, 16-256 px, embedded in the .exe (build.rs).
  icon-64.png   Window and tray icon, embedded in the binary.

Windows and tray icons are cropped to the artwork: macOS-style padding would make them look
small next to other icons. Needs Pillow (pip install pillow).
"""

import sys
from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
ASSETS = ROOT / "assets"
ICO_SIZES = [16, 20, 24, 32, 40, 48, 64, 96, 128, 256]


def cropped(master: Image.Image) -> Image.Image:
    """A square around the artwork (ignoring its faint drop shadow), with a 3% margin."""
    alpha = master.getchannel("A").point(lambda a: 255 if a > 32 else 0)
    left, top, right, bottom = alpha.getbbox()
    side = max(right - left, bottom - top) * 1.03
    cx, cy = (left + right) / 2, (top + bottom) / 2
    box = (round(cx - side / 2), round(cy - side / 2), round(cx + side / 2), round(cy + side / 2))
    return master.crop(box)


def main() -> None:
    source = Path(sys.argv[1]) if len(sys.argv) > 1 else ASSETS / "icon.png"
    master = Image.open(source).convert("RGBA")
    if master.size != (1024, 1024):
        sys.exit(f"{source}: expected 1024x1024, got {master.size[0]}x{master.size[1]}")
    ASSETS.mkdir(exist_ok=True)
    master.save(ASSETS / "icon.png", optimize=True)

    tight = cropped(master)
    tight.resize((256, 256), Image.LANCZOS).save(ASSETS / "stickitt.ico", sizes=[(s, s) for s in ICO_SIZES])
    tight.resize((64, 64), Image.LANCZOS).save(ASSETS / "icon-64.png", optimize=True)
    for name in ("icon.png", "stickitt.ico", "icon-64.png"):
        print(f"assets/{name}  {(ASSETS / name).stat().st_size:,} bytes")


if __name__ == "__main__":
    main()
