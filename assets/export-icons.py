#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["numpy", "pillow", "scipy"]
# ///
"""Export Turin's prod icon assets from assets/prod/app-icon.icon.

Run from the repository root:  uv run assets/export-icons.py

- iOS / universal / web / Windows renditions come straight from Icon Composer's
  `ictool` (iOS platform, Default rendition, design generation 26), exactly as
  upstream's scripts/export-brand-icons.ts does.
- The macOS PNG needs Icon Composer's GUI-only "macOS pre-Tahoe" preset. Without
  it, this script derives it from upstream's native pre-Tahoe export of the T3
  icon (same background, body, rim and shadow): the T3 glyph is erased, and the
  upstream T's natively-lit pixels are mapped through the same affine transform
  that turns the upstream T path into Turin's T path, then recombined with the
  exact anti-aliased coverage of Turin's path. See assets/README.md.
- The .icns is built from the macOS PNG with sips + iconutil.
"""

from __future__ import annotations

import io
import re
import shutil
import struct
import subprocess
import sys
import tempfile
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageFont
from scipy import ndimage

ROOT = Path(__file__).resolve().parent.parent
PROD = ROOT / "assets" / "prod"
SOURCE = PROD / "app-icon.icon"
UPSTREAM_PROD = ROOT / "upstream" / "assets" / "prod"

DESIGN_GENERATION = "26"
WINDOWS_ICON_SIZES = [16, 24, 32, 48, 64, 128, 256]
# Icon Composer draws the 128-unit SVG at the layer scale from icon.json, centred on
# the 1024pt canvas; the pre-Tahoe macOS body is that canvas at 824px, inset 100px.
LAYER_SCALE = 8.5
MAC_K = LAYER_SCALE * 824 / 1024
MAC_B = 512 - 64 * MAC_K

OUTPUTS = {
    "ios": "black-ios-1024.png",
    "universal": "black-universal-1024.png",
    "macos": "black-macos-1024.png",
    "apple_touch": "turin-black-web-apple-touch-180.png",
    "favicon16": "turin-black-web-favicon-16x16.png",
    "favicon32": "turin-black-web-favicon-32x32.png",
    "favicon_ico": "turin-black-web-favicon.ico",
    "windows_ico": "turin-black-windows.ico",
    "icns": "turin-black.icns",
    "compare": "compare-t3-vs-turin.png",
}


def find_ictool() -> str:
    candidates = [
        "/Applications/Xcode.app/Contents/Applications/Icon Composer.app/Contents/Executables/ictool",
        "/Applications/Icon Composer.app/Contents/Executables/ictool",
    ]
    for candidate in candidates:
        if Path(candidate).exists():
            return candidate
    sys.exit("Icon Composer's ictool not found; install Xcode 26+ or Icon Composer 2+.")


def ictool_png(tool: str, icon: Path, size: int, out: Path) -> bytes:
    subprocess.run(
        [tool, str(icon), "--export-image", "--output-file", str(out), "--platform", "iOS",
         "--rendition", "Default", "--width", str(size), "--height", str(size), "--scale", "1",
         "--design-generation", DESIGN_GENERATION],
        check=True, capture_output=True,
    )
    data = out.read_bytes()
    w, h = struct.unpack(">II", data[16:24])
    assert (w, h) == (size, size), f"ictool produced {w}x{h}, expected {size}"
    return data


def encode_png_ico(images: list[tuple[int, bytes]]) -> bytes:
    """Same layout as upstream's encodePngIco: PNG-compressed entries, 32bpp."""
    header = struct.pack("<HHH", 0, 1, len(images))
    offset = 6 + 16 * len(images)
    entries, blobs = b"", b""
    for size, png in images:
        dim = 0 if size == 256 else size
        entries += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(png), offset)
        offset += len(png)
        blobs += png
    return header + entries + blobs


# --- SVG path helpers (absolute M/L/H/V/C/Z only, which is all the marks use) ---

def svg_paths(svg: Path) -> list[str]:
    return re.findall(r'<path[^>]*\sd="([^"]+)"', svg.read_text())


def path_polys(d: str, steps: int = 24) -> list[list[tuple[float, float]]]:
    toks = re.findall(r"[MLHVCZ]|-?\d*\.?\d+", d)
    polys, cur, i, cmd, x, y = [], [], 0, None, 0.0, 0.0

    def num() -> float:
        nonlocal i
        i += 1
        return float(toks[i - 1])

    while i < len(toks):
        if toks[i] in "MLHVCZ":
            cmd = toks[i]
            i += 1
        if cmd == "M":
            if cur:
                polys.append(cur)
            x, y = num(), num()
            cur, cmd = [(x, y)], "L"
        elif cmd == "L":
            x, y = num(), num()
            cur.append((x, y))
        elif cmd == "H":
            x = num()
            cur.append((x, y))
        elif cmd == "V":
            y = num()
            cur.append((x, y))
        elif cmd == "C":
            x1, y1, x2, y2, x3, y3 = (num() for _ in range(6))
            for k in range(1, steps + 1):
                t, m = k / steps, 1 - k / steps
                cur.append((m**3 * x + 3 * m * m * t * x1 + 3 * m * t * t * x2 + t**3 * x3,
                            m**3 * y + 3 * m * m * t * y1 + 3 * m * t * t * y2 + t**3 * y3))
            x, y = x3, y3
        elif cmd == "Z":
            polys.append(cur)
            cur, cmd = [], None
    if cur:
        polys.append(cur)
    return polys


def subpaths(d: str) -> list[str]:
    return [p + "Z" for p in d.split("Z") if p.strip()]


def bbox(d: str) -> tuple[float, float, float, float]:
    pts = [p for poly in path_polys(d) for p in poly]
    xs, ys = [p[0] for p in pts], [p[1] for p in pts]
    return min(xs), min(ys), max(xs), max(ys)


def coverage(d: str, size: int, k: float, b: float, ss: int = 8) -> np.ndarray:
    """Anti-aliased coverage of path d (128-unit space) drawn at px = k*u + b."""
    im = Image.new("L", (size * ss, size * ss), 0)
    draw = ImageDraw.Draw(im)
    for poly in path_polys(d):
        draw.polygon([((k * u + b) * ss, (k * v + b) * ss) for u, v in poly], fill=255)
    return np.asarray(im.resize((size, size), Image.BOX)).astype(float) / 255


# --- macOS pre-Tahoe ---

def macos_pre_tahoe(upstream_png: Path, upstream_mark: str, upstream_t: str, turin_t: str) -> Image.Image:
    up = np.asarray(Image.open(upstream_png).convert("RGBA")).astype(float)
    rgb, alpha = up[..., :3], up[..., 3]

    # Affine map upstream T -> Turin T, derived from the two paths' bounding boxes.
    ox0, oy0, ox1, oy1 = bbox(upstream_t)
    nx0, ny0, nx1, ny1 = bbox(turin_t)
    s = (nx1 - nx0) / (ox1 - ox0)
    assert abs(s - (ny1 - ny0) / (oy1 - oy0)) < 1e-6, "Turin T must be a uniform scale of upstream's"
    tx, ty = nx0 - s * ox0, ny0 - s * oy0
    for (u, v), (u2, v2) in zip(path_polys(upstream_t)[0], path_polys(turin_t)[0]):
        assert abs(s * u + tx - u2) < 1e-3 and abs(s * v + ty - v2) < 1e-3, "path is not the upstream T"

    # 1. Erase the T3 glyph (the background around it is pure black). Filling holes
    #    covers the "3" path's self-overlap, which the even-odd polygon fill leaves open.
    glyph = ndimage.binary_fill_holes(
        ndimage.binary_dilation(coverage(upstream_mark, 1024, MAC_K, MAC_B) > 0, iterations=3)
    )
    base = rgb.copy()
    base[glyph] = 0

    # 2. Native shading field of the upstream T: its fully-covered pixels, extended
    #    outward by nearest-neighbour so edge pixels sample interior shading.
    full = coverage(upstream_t, 1024, MAC_K, MAC_B) >= 0.999
    iy, ix = ndimage.distance_transform_edt(~full, return_distances=False, return_indices=True)
    field = rgb[iy, ix]

    # 3. Pull that field through the inverse affine (pixel centres) onto Turin's T.
    c = np.arange(1024) + 0.5
    src_x = (c - MAC_B - MAC_K * tx) / s + MAC_B - 0.5
    src_y = (c - MAC_B - MAC_K * ty) / s + MAC_B - 0.5
    yy, xx = np.meshgrid(src_y, src_x, indexing="ij")
    shaded = np.stack(
        [ndimage.map_coordinates(field[..., ch], [yy, xx], order=1, mode="nearest") for ch in range(3)], -1
    )

    # 4. Composite with Turin's exact coverage.
    cov = coverage(turin_t, 1024, MAC_K, MAC_B)[..., None]
    out = base * (1 - cov) + shaded * cov
    return Image.fromarray(np.dstack([np.clip(np.rint(out), 0, 255), alpha]).astype(np.uint8), "RGBA")


def build_icns(macos_png: Path, out: Path, tmp: Path) -> None:
    iconset = tmp / "turin.iconset"
    iconset.mkdir()
    for pt in (16, 32, 128, 256, 512):
        for scale in (1, 2):
            px = pt * scale
            name = f"icon_{pt}x{pt}{'@2x' if scale == 2 else ''}.png"
            if px == 1024:
                shutil.copy(macos_png, iconset / name)
            else:
                subprocess.run(["sips", "-z", str(px), str(px), str(macos_png), "--out", str(iconset / name)],
                               check=True, capture_output=True)
    subprocess.run(["iconutil", "-c", "icns", str(iconset), "-o", str(out)], check=True)


# --- comparison sheet ---

def compare_sheet(out: Path) -> None:
    cols = [  # (label, upstream file, turin file, display size, zoom)
        ("macOS 1024 (50%)", "black-macos-1024.png", OUTPUTS["macos"], 512, 1),
        ("iOS / universal 1024 (50%)", "black-ios-1024.png", OUTPUTS["ios"], 512, 1),
        ("apple-touch 180", "t3-black-web-apple-touch-180.png", OUTPUTS["apple_touch"], 180, 1),
        ("favicon 32 (1x, 4x)", "t3-black-web-favicon-32x32.png", OUTPUTS["favicon32"], 32, 4),
        ("favicon 16 (1x, 8x)", "t3-black-web-favicon-16x16.png", OUTPUTS["favicon16"], 16, 8),
    ]
    pad, label_h, row_label_w = 24, 34, 110
    try:
        font = ImageFont.truetype("/System/Library/Fonts/Helvetica.ttc", 18)
    except OSError:
        font = ImageFont.load_default()
    widths = [(d + (d * z + pad if z > 1 else 0)) for _, _, _, d, z in cols]
    W = row_label_w + sum(widths) + pad * (len(cols) + 1)
    H = label_h + 2 * (512 + pad) + pad
    sheet = Image.new("RGBA", (W, H), (236, 236, 236, 255))
    draw = ImageDraw.Draw(sheet)
    for r, (row, base) in enumerate((("T3 Code", UPSTREAM_PROD), ("Turin", PROD))):
        y = label_h + pad + r * (512 + pad)
        draw.text((pad, y + 246), row, fill=(40, 40, 40), font=font)
        x = row_label_w + pad
        for (label, up_name, turin_name, d, z), w in zip(cols, widths):
            if r == 0:
                draw.text((x, 8), label, fill=(40, 40, 40), font=font)
            im = Image.open(base / (up_name if r == 0 else turin_name)).convert("RGBA")
            shown = im.resize((d, d), Image.LANCZOS) if im.width != d else im
            sheet.alpha_composite(shown, (x, y + (512 - d) // 2))
            if z > 1:
                big = shown.resize((d * z, d * z), Image.NEAREST)
                sheet.alpha_composite(big, (x + d + pad, y + (512 - d * z) // 2))
            x += w + pad
    sheet.convert("RGB").save(out, optimize=True)


def main() -> None:
    tool = find_ictool()
    version = subprocess.run([tool, "--version"], capture_output=True, text=True).stdout
    print("ictool", " ".join(version.split()))

    turin_t = svg_paths(SOURCE / "Assets" / "text.svg")[0]
    upstream_mark = svg_paths(UPSTREAM_PROD / "app-icon.icon" / "Assets" / "text.svg")[0]
    upstream_t = subpaths(upstream_mark)[0]

    with tempfile.TemporaryDirectory(prefix="turin-icons-") as tmp_name:
        tmp = Path(tmp_name)
        render = {size: ictool_png(tool, SOURCE, size, tmp / f"ios-{size}.png")
                  for size in sorted({1024, 180, *WINDOWS_ICON_SIZES})}

        (PROD / OUTPUTS["ios"]).write_bytes(render[1024])
        (PROD / OUTPUTS["universal"]).write_bytes(render[1024])
        (PROD / OUTPUTS["apple_touch"]).write_bytes(render[180])
        (PROD / OUTPUTS["favicon16"]).write_bytes(render[16])
        (PROD / OUTPUTS["favicon32"]).write_bytes(render[32])
        ico = encode_png_ico([(size, render[size]) for size in WINDOWS_ICON_SIZES])
        (PROD / OUTPUTS["favicon_ico"]).write_bytes(ico)
        (PROD / OUTPUTS["windows_ico"]).write_bytes(ico)

        mac = macos_pre_tahoe(UPSTREAM_PROD / "black-macos-1024.png", upstream_mark, upstream_t, turin_t)
        buf = io.BytesIO()
        mac.save(buf, "PNG", optimize=True)
        (PROD / OUTPUTS["macos"]).write_bytes(buf.getvalue())

        build_icns(PROD / OUTPUTS["macos"], PROD / OUTPUTS["icns"], tmp)

    compare_sheet(PROD / OUTPUTS["compare"])
    for name in OUTPUTS.values():
        print("wrote", (PROD / name).relative_to(ROOT))


if __name__ == "__main__":
    main()
