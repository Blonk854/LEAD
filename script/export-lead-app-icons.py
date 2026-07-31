#!/usr/bin/env python3
"""Export LEAD app icons (PNG + ICO) from assets/brand/lead-app-icon-master.png."""

from __future__ import annotations

import io
import struct
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parents[1]
MASTER = ROOT / "assets" / "brand" / "lead-app-icon-master.png"
MASTER_DEV = ROOT / "assets" / "brand" / "lead-app-icon-master-dev.png"

PNG_TARGETS = [
    (ROOT / "crates" / "zed" / "resources" / "app-icon.png", False),
    (ROOT / "crates" / "zed" / "resources" / "app-icon@2x.png", False),
    (ROOT / "crates" / "zed" / "resources" / "app-icon-dev.png", True),
    (ROOT / "crates" / "zed" / "resources" / "app-icon-dev@2x.png", True),
    (ROOT / "crates" / "zed" / "resources" / "app-icon-nightly.png", False),
    (ROOT / "crates" / "zed" / "resources" / "app-icon-nightly@2x.png", False),
    (ROOT / "crates" / "zed" / "resources" / "app-icon-preview.png", False),
    (ROOT / "crates" / "zed" / "resources" / "app-icon-preview@2x.png", False),
]

ICO_TARGETS = [
    (ROOT / "crates" / "zed" / "resources" / "windows" / "app-icon.ico", False),
    (ROOT / "crates" / "zed" / "resources" / "windows" / "app-icon-dev.ico", True),
    (ROOT / "crates" / "zed" / "resources" / "windows" / "app-icon-nightly.ico", False),
    (ROOT / "crates" / "zed" / "resources" / "windows" / "app-icon-preview.ico", False),
    (ROOT / "inno" / "lead-x86_64" / "app-icon.ico", False),
    (ROOT / "inno" / "lead-x86_64" / "app-icon-dev.ico", True),
    (ROOT / "inno" / "lead-x86_64" / "app-icon-nightly.ico", False),
    (ROOT / "inno" / "lead-x86_64" / "app-icon-preview.ico", False),
]

ICO_SIZES = [(16, 16), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)]

LEAD_LOGO = ROOT / "assets" / "images" / "lead_logo.png"
LEAD_LOGO_SIZE = 192

# LEAD pencil yellow + dark squircle tones
BADGE_FILL = (212, 168, 50, 235)
BADGE_BORDER = (255, 220, 90, 255)
BADGE_TEXT = (26, 26, 30, 255)


def _load_font(size: int) -> ImageFont.FreeTypeFont | ImageFont.ImageFont:
    candidates = [
        Path("C:/Windows/Fonts/segoeuib.ttf"),
        Path("C:/Windows/Fonts/arialbd.ttf"),
        Path("/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf"),
        Path("/System/Library/Fonts/Supplemental/Arial Bold.ttf"),
    ]
    for path in candidates:
        if path.is_file():
            return ImageFont.truetype(str(path), size=size)
    return ImageFont.load_default()


def _rounded_rect(
    draw: ImageDraw.ImageDraw,
    box: tuple[float, float, float, float],
    radius: float,
    fill: tuple[int, int, int, int],
    outline: tuple[int, int, int, int] | None = None,
    width: int = 1,
) -> None:
    draw.rounded_rectangle(box, radius=radius, fill=fill, outline=outline, width=width)


def apply_dev_badge(img: Image.Image) -> Image.Image:
    """Place a DEV pill in the bottom-right, clear of the pencil tip (lower-left)."""
    out = img.copy().convert("RGBA")
    w, h = out.size
    draw = ImageDraw.Draw(out)

    badge_h = max(12, int(h * 0.13))
    badge_w = max(36, int(w * 0.26))
    margin = max(6, int(w * 0.055))
    radius = badge_h * 0.22
    border = max(1, int(h * 0.004))

    x1 = w - margin
    y1 = h - margin
    x0 = x1 - badge_w
    y0 = y1 - badge_h
    box = (x0, y0, x1, y1)

    _rounded_rect(
        draw,
        box,
        radius,
        fill=BADGE_FILL,
        outline=BADGE_BORDER,
        width=border,
    )

    font_size = max(10, int(badge_h * 0.52))
    font = _load_font(font_size)
    label = "DEV"
    text_bbox = draw.textbbox((0, 0), label, font=font)
    text_w = text_bbox[2] - text_bbox[0]
    text_h = text_bbox[3] - text_bbox[1]
    text_x = x0 + (badge_w - text_w) / 2
    text_y = y0 + (badge_h - text_h) / 2 - text_bbox[1]
    draw.text((text_x, text_y), label, fill=BADGE_TEXT, font=font)

    return out


def export_pngs(master: Image.Image, master_dev: Image.Image) -> None:
    cache: dict[tuple[int, bool], Image.Image] = {}

    def get(size: int, dev: bool) -> Image.Image:
        key = (size, dev)
        if key not in cache:
            source = master_dev if dev else master
            cache[key] = source.resize((size, size), Image.Resampling.LANCZOS)
        return cache[key]

    for target, dev in PNG_TARGETS:
        size = 1024 if "@2x" in target.name else 512
        out = target
        out.parent.mkdir(parents=True, exist_ok=True)
        get(size, dev).save(out, format="PNG", optimize=True)
        print(f"wrote {out.relative_to(ROOT)}")


def _png_bytes(img: Image.Image) -> bytes:
    buf = io.BytesIO()
    img.save(buf, format="PNG")
    return buf.getvalue()


def write_png_ico(path: Path, source: Image.Image) -> None:
    """Write a Vista+ PNG-compressed multi-size ICO (Pillow's BMP ICO is tiny/broken here)."""
    pngs: list[tuple[int, bytes]] = []
    for width, height in ICO_SIZES:
        resized = source.resize((width, height), Image.Resampling.LANCZOS).convert("RGBA")
        pngs.append((width, _png_bytes(resized)))

    header = struct.pack("<HHH", 0, 1, len(pngs))
    offset = 6 + 16 * len(pngs)
    entries = bytearray()
    payload = bytearray()
    for size, png in pngs:
        w = 0 if size >= 256 else size
        h = 0 if size >= 256 else size
        entries.extend(struct.pack("<BBBBHHII", w, h, 0, 0, 1, 32, len(png), offset))
        payload.extend(png)
        offset += len(png)

    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_bytes(header + bytes(entries) + bytes(payload))
    tmp.replace(path)


def export_icos(master: Image.Image, master_dev: Image.Image) -> None:
    for target, dev in ICO_TARGETS:
        source = master_dev if dev else master
        write_png_ico(target, source)
        print(f"wrote {target.relative_to(ROOT)} ({target.stat().st_size} bytes)")


def export_lead_logo(master: Image.Image) -> None:
    LEAD_LOGO.parent.mkdir(parents=True, exist_ok=True)
    logo = master.resize((LEAD_LOGO_SIZE, LEAD_LOGO_SIZE), Image.Resampling.LANCZOS)
    logo.save(LEAD_LOGO, format="PNG", optimize=True)
    print(f"wrote {LEAD_LOGO.relative_to(ROOT)}")


def main() -> None:
    if not MASTER.is_file():
        raise SystemExit(f"Missing master icon: {MASTER}")
    master = Image.open(MASTER).convert("RGBA")
    master_dev = apply_dev_badge(master)
    master_dev.save(MASTER_DEV, format="PNG", optimize=True)
    print(f"wrote {MASTER_DEV.relative_to(ROOT)}")
    export_pngs(master, master_dev)
    export_icos(master, master_dev)
    export_lead_logo(master)
    print("Done.")


if __name__ == "__main__":
    main()
