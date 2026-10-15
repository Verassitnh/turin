#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = ["numpy==2.5.4", "pillow==12.3.0", "pypng==0.20220715.0", "scipy==1.18.1"]
# [tool.uv]
# exclude-newer = "2026-10-14T00:00:00Z"
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

import argparse
import io
import json
import os
import plistlib
import re
import shutil
import struct
import subprocess
import sys
import tempfile
from pathlib import Path

import numpy as np
import png
from PIL import Image, ImageDraw, ImageFont
from PIL.PngImagePlugin import PngInfo
from scipy import ndimage

ROOT = Path(__file__).resolve().parent.parent
PROD = ROOT / "assets" / "prod"
UPSTREAM_PROD = ROOT / "upstream" / "assets" / "prod"

DESIGN_GENERATION = "26"
# Upstream's T path is scaled by this and re-centred (see place_t). Pending user sign-off
# against 1.0 (upstream's size): compare with `--scale-options 1.0 1.25`.
DEFAULT_T_SCALE = 1.25
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


ICTOOL_IN_APP = ("Icon Composer.app", "Contents", "Executables", "ictool")


def probe_ictool(path: Path) -> tuple[int, str] | None:
    """(bundle-version, "short (bundle)") if path is an Icon Composer 2+ ictool, else None."""
    if not path.is_file():
        return None
    try:
        out = subprocess.run([str(path), "--version"], capture_output=True, text=True, timeout=30)
        info = json.loads(out.stdout)
        short, bundle = info["short-bundle-version"], info["bundle-version"]
        if out.returncode != 0 or int(short.split(".")[0]) < 2:
            return None
        return int(bundle), f"{short} ({bundle})"
    except (OSError, ValueError, KeyError, subprocess.TimeoutExpired):
        return None


def find_ictool() -> tuple[str, str]:
    """Same search as upstream's export-brand-icons.ts: ICON_COMPOSER_TOOL wins outright,
    otherwise the newest Icon Composer 2+ among DEVELOPER_DIR, `xcode-select -p`, standalone
    installs and any Xcode*.app in /Applications or ~/Downloads."""
    configured = os.environ.get("ICON_COMPOSER_TOOL", "").strip()
    if configured:
        probed = probe_ictool(Path(configured))
        if probed is None:
            sys.exit(f"ICON_COMPOSER_TOOL is not an Icon Composer 2+ ictool: {configured}")
        return configured, probed[1]

    developer_dirs = [os.environ.get("DEVELOPER_DIR", "").strip()]
    try:
        developer_dirs.append(subprocess.run(["xcode-select", "-p"], capture_output=True, text=True).stdout.strip())
    except OSError:
        pass
    home = Path.home()
    candidates = [Path(d).parent.joinpath("Applications", *ICTOOL_IN_APP) for d in developer_dirs if d]
    candidates += [Path("/Applications", *ICTOOL_IN_APP), home.joinpath("Applications", *ICTOOL_IN_APP)]
    for directory in (Path("/Applications"), home / "Downloads"):
        candidates += [xcode.joinpath("Contents", "Applications", *ICTOOL_IN_APP)
                       for xcode in sorted(directory.glob("Xcode*.app"))]
    probed = [(p, str(c)) for c in dict.fromkeys(candidates) if (p := probe_ictool(c))]
    if not probed:
        sys.exit("No Icon Composer 2+ ictool found (design generation 26 needs it). Install Xcode 26+ "
                 "or Icon Composer 2+, or set ICON_COMPOSER_TOOL to Icon Composer.app/Contents/Executables/ictool.")
    (_, version), path = max(probed, key=lambda item: item[0][0])
    return path, version


def ictool_png(tool: str, icon: Path, size: int, out: Path) -> bytes:
    subprocess.run(
        [tool, str(icon), "--export-image", "--output-file", str(out), "--platform", "iOS",
         "--rendition", "Default", "--width", str(size), "--height", str(size), "--scale", "1",
         "--design-generation", DESIGN_GENERATION],
        check=True, capture_output=True,
    )
    image = to_srgb8(out.read_bytes())
    assert image.size == (size, size), f"ictool produced {image.size}, expected {size}"
    return srgb_png(image)


# --- colour normalisation ---
# Icon Composer 27 writes 16-bit Display P3 PNGs; upstream's committed exports (and everything
# here) are 8-bit RGBA sRGB with an sRGB chunk. Both spaces share the sRGB transfer curve and D65.
P3_TO_SRGB = np.array([[1.2249401, -0.2249404, 0.0],
                       [-0.0420569, 1.0420571, 0.0],
                       [-0.0196376, -0.0786361, 1.0982735]])


SRGB_CHRM = struct.pack(">8I", 31270, 32900, 64000, 33000, 30000, 60000, 15000, 6000)


def png_chunks(data: bytes) -> dict[str, bytes]:
    chunks, i = {}, 8
    while i < len(data):
        (n,), kind = struct.unpack(">I", data[i:i + 4]), data[i + 4:i + 8].decode("latin-1")
        chunks.setdefault(kind, data[i + 8:i + 8 + n])
        i += 12 + n
    return chunks


def to_srgb8(data: bytes) -> Image.Image:
    """Decode a PNG at full bit depth and return it as 8-bit RGBA in sRGB."""
    width, height, rows, info = png.Reader(bytes=data).asDirect()
    assert info["planes"] == 4, "expected an RGBA PNG"
    pixels = np.vstack([np.asarray(row, dtype=np.float64) for row in rows]).reshape(height, width, 4)
    pixels /= 2 ** info["bitdepth"] - 1
    chunks = png_chunks(data)
    cicp_primaries = chunks["cICP"][0] if "cICP" in chunks else None
    iccp_name = chunks["iCCP"].split(b"\0")[0] if "iCCP" in chunks else b""
    if cicp_primaries == 12 or b"P3" in iccp_name:
        rgb = pixels[..., :3]
        linear = np.where(rgb <= 0.04045, rgb / 12.92, ((rgb + 0.055) / 1.055) ** 2.4)
        linear = np.clip((linear[..., None, :] * P3_TO_SRGB).sum(-1), 0, 1)
        pixels[..., :3] = np.where(linear <= 0.0031308, linear * 12.92, 1.055 * linear ** (1 / 2.4) - 0.055)
    elif not ("sRGB" in chunks or cicp_primaries == 1 or b"sRGB" in iccp_name
            or chunks.get("cHRM") == SRGB_CHRM  # sips writes gAMA 1/2.2 + sRGB primaries
            or not chunks.keys() & {"iCCP", "cICP", "gAMA", "cHRM"}):
        sys.exit(f"unexpected PNG colour space (iCCP {iccp_name!r}, cICP primaries {cicp_primaries})")
    return Image.fromarray(np.rint(pixels * 255).astype(np.uint8))


def srgb_png(image: Image.Image) -> bytes:
    """8-bit RGBA PNG with an sRGB chunk (perceptual intent), matching upstream's exports."""
    info = PngInfo()
    info.add(b"sRGB", b"\x00")
    buf = io.BytesIO()
    image.convert("RGBA").save(buf, "PNG", optimize=True, pnginfo=info)
    return buf.getvalue()


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


def fmt(v: float) -> str:
    return f"{v:.4f}".rstrip("0").rstrip(".")


def place_t(upstream_t: str, scale: float) -> str:
    """Upstream's T scaled about nothing but itself: scale, then translate so it is centred on
    x = 64 and keeps its cap-height centre (y = 65, the T3 pair's) on the 128 grid."""
    x0, y0, x1, y1 = bbox(upstream_t)
    tx, ty = 64 - scale * (x0 + x1) / 2, (y0 + y1) / 2 * (1 - scale)
    d, cmd, n = "", None, 0
    for tok in re.findall(r"[MLHVCZ]|-?\d*\.?\d+", upstream_t):
        if tok in "MLHVCZ":
            d, cmd, n = d + tok, tok, 0
            continue
        on_x = cmd == "H" or (cmd != "V" and n % 2 == 0)
        d += (" " if n else "") + fmt(scale * float(tok) + (tx if on_x else ty))
        n += 1
    return d


def write_sources(out_dir: Path, turin_t: str) -> None:
    """The Icon Composer project and logo.svg: upstream prod's files with the T3 path swapped
    for Turin's T. icon.json is copied unchanged."""
    upstream_svg = (UPSTREAM_PROD / "app-icon.icon" / "Assets" / "text.svg").read_text()
    upstream_logo = (UPSTREAM_PROD / "logo.svg").read_text()
    mark = svg_paths(UPSTREAM_PROD / "app-icon.icon" / "Assets" / "text.svg")[0]
    assert upstream_svg.count(mark) == 1 and upstream_logo.count(mark) == 1
    project = out_dir / "app-icon.icon"
    (project / "Assets").mkdir(parents=True, exist_ok=True)
    shutil.copy(UPSTREAM_PROD / "app-icon.icon" / "icon.json", project / "icon.json")
    (project / "Assets" / "text.svg").write_text(upstream_svg.replace(mark, turin_t))
    (out_dir / "logo.svg").write_text(upstream_logo.replace(mark, turin_t))


def coverage(d: str, size: int, k: float, b: float, ss: int = 8) -> np.ndarray:
    """Anti-aliased coverage of path d (128-unit space) drawn at px = k*u + b."""
    im = Image.new("L", (size * ss, size * ss), 0)
    draw = ImageDraw.Draw(im)
    for poly in path_polys(d):
        draw.polygon([((k * u + b) * ss, (k * v + b) * ss) for u, v in poly], fill=255)
    return np.asarray(im.resize((size, size), Image.BOX)).astype(float) / 255


# --- macOS pre-Tahoe ---

def macos_pre_tahoe(upstream_png: Path, upstream_mark: str, upstream_t: str, turin_t: str) -> Image.Image:
    up = np.asarray(to_srgb8(upstream_png.read_bytes())).astype(float)
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
    return Image.fromarray(np.dstack([np.clip(np.rint(out), 0, 255), alpha]).astype(np.uint8))


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
                (iconset / name).write_bytes(srgb_png(to_srgb8((iconset / name).read_bytes())))
    subprocess.run(["iconutil", "-c", "icns", str(iconset), "-o", str(out)], check=True)


# --- macOS app bundle (Tahoe Liquid Glass icon) ---

def compile_bundle_icon(icon: Path, resources: Path) -> None:
    """actool the .icon into Assets.car + <name>.icns. actool exits 0 and writes nothing when
    --app-icon doesn't match the project's basename, so check the outputs explicitly."""
    name = icon.stem
    resources.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="turin-actool-") as tmp_name:
        plist = Path(tmp_name) / "partial.plist"
        subprocess.run(
            ["xcrun", "actool", str(icon.resolve()), "--compile", str(resources.resolve()),
             "--platform", "macosx", "--minimum-deployment-target", "11.0", "--app-icon", name,
             "--output-partial-info-plist", str(plist)],
            check=True, capture_output=True,
        )
        keys = plistlib.loads(plist.read_bytes())
    for output in (resources / "Assets.car", resources / f"{name}.icns"):
        if not output.is_file() or output.stat().st_size == 0:
            sys.exit(f"actool produced no {output.name}; nothing was compiled")
    if keys.get("CFBundleIconName") != name or keys.get("CFBundleIconFile") != name:
        sys.exit(f"unexpected actool Info.plist keys: {keys}")
    print(f"compiled {resources}/Assets.car and {name}.icns; set Info.plist "
          f"CFBundleIconName = {name} and CFBundleIconFile = {name}")


# --- comparison sheets ---

def sheet_font() -> ImageFont.FreeTypeFont:
    # Pillow's bundled font, so the sheets don't depend on system fonts.
    return ImageFont.load_default(size=18)


def compare_sheet(out_dir: Path) -> None:
    cols = [  # (label, upstream file, turin file, display size, zoom)
        ("macOS 1024 (50%)", "black-macos-1024.png", OUTPUTS["macos"], 512, 1),
        ("iOS / universal 1024 (50%)", "black-ios-1024.png", OUTPUTS["ios"], 512, 1),
        ("apple-touch 180", "t3-black-web-apple-touch-180.png", OUTPUTS["apple_touch"], 180, 1),
        ("favicon 32 (1x, 4x)", "t3-black-web-favicon-32x32.png", OUTPUTS["favicon32"], 32, 4),
        ("favicon 16 (1x, 8x)", "t3-black-web-favicon-16x16.png", OUTPUTS["favicon16"], 16, 8),
    ]
    pad, label_h, row_label_w = 24, 34, 110
    font = sheet_font()
    widths = [(d + (d * z + pad if z > 1 else 0)) for _, _, _, d, z in cols]
    W = row_label_w + sum(widths) + pad * (len(cols) + 1)
    H = label_h + 2 * (512 + pad) + pad
    sheet = Image.new("RGBA", (W, H), (236, 236, 236, 255))
    draw = ImageDraw.Draw(sheet)
    for r, (row, base) in enumerate((("T3 Code", UPSTREAM_PROD), ("Turin", out_dir))):
        y = label_h + pad + r * (512 + pad)
        draw.text((pad, y + 246), row, fill=(40, 40, 40), font=font)
        x = row_label_w + pad
        for (label, up_name, turin_name, d, z), w in zip(cols, widths):
            if r == 0:
                draw.text((x, 8), label, fill=(40, 40, 40), font=font)
            im = to_srgb8((base / (up_name if r == 0 else turin_name)).read_bytes())
            shown = im.resize((d, d), Image.LANCZOS) if im.width != d else im
            sheet.alpha_composite(shown, (x, y + (512 - d) // 2))
            if z > 1:
                big = shown.resize((d * z, d * z), Image.NEAREST)
                sheet.alpha_composite(big, (x + d + pad, y + (512 - d * z) // 2))
            x += w + pad
    (out_dir / OUTPUTS["compare"]).write_bytes(srgb_png(sheet))


def scale_options_sheet(tool: str, scales: list[float], out: Path) -> None:
    """Upstream next to Turin at each T scale, every column through the same pipeline: macOS is
    upstream's native pre-Tahoe export vs the derived PNG; 180/32/16 are ictool renders (upstream's
    .icon re-rendered with the same ictool, so tool drift doesn't show up as a difference)."""
    with tempfile.TemporaryDirectory(prefix="turin-scales-") as tmp_name:
        tmp = Path(tmp_name)
        columns = [("T3 Code (upstream)", {
            "macos": to_srgb8((UPSTREAM_PROD / "black-macos-1024.png").read_bytes()),
            **{size: to_srgb8(ictool_png(tool, UPSTREAM_PROD / "app-icon.icon", size, tmp / f"up-{size}.png"))
               for size in (180, 32, 16)},
        })]
        for scale in scales:
            out_dir = tmp / f"scale-{scale}"
            export(tool, out_dir, scale, keep_macos_png=False, quiet=True)
            columns.append((f"Turin, T at {scale:g}x" + (" (committed)" if scale == DEFAULT_T_SCALE else ""), {
                "macos": to_srgb8((out_dir / OUTPUTS["macos"]).read_bytes()),
                180: to_srgb8((out_dir / OUTPUTS["apple_touch"]).read_bytes()),
                32: to_srgb8((out_dir / OUTPUTS["favicon32"]).read_bytes()),
                16: to_srgb8((out_dir / OUTPUTS["favicon16"]).read_bytes()),
            }))

    rows = [("macOS 1024\n(50%)", "macos", 512, 1), ("apple-touch\n180", 180, 180, 1),
            ("favicon 32\n(1x, 4x)", 32, 32, 4), ("favicon 16\n(1x, 8x)", 16, 16, 8)]
    pad, label_h, row_label_w, col_w = 24, 40, 130, 560
    heights = [max(d, d * z) for _, _, d, z in rows]
    sheet = Image.new("RGBA", (row_label_w + len(columns) * (col_w + pad) + pad,
                               label_h + sum(heights) + pad * (len(rows) + 1)), (236, 236, 236, 255))
    draw = ImageDraw.Draw(sheet)
    font = sheet_font()
    for c, (title, images) in enumerate(columns):
        x0 = row_label_w + pad + c * (col_w + pad)
        draw.text((x0, 10), title, fill=(40, 40, 40), font=font)
        y = label_h + pad
        for (label, key, d, z), h in zip(rows, heights):
            if c == 0:
                draw.multiline_text((pad, y + h // 2 - 20), label, fill=(40, 40, 40), font=font)
            im = images[key]
            shown = im.resize((d, d), Image.LANCZOS) if im.width != d else im
            sheet.alpha_composite(shown, (x0, y + (h - d) // 2))
            if z > 1:
                sheet.alpha_composite(shown.resize((d * z, d * z), Image.NEAREST), (x0 + d + pad, y + (h - d * z) // 2))
            y += h + pad
    out.write_bytes(srgb_png(sheet))


def export(tool: str, out_dir: Path, scale: float, keep_macos_png: bool, quiet: bool = False) -> None:
    upstream_mark = svg_paths(UPSTREAM_PROD / "app-icon.icon" / "Assets" / "text.svg")[0]
    upstream_t = subpaths(upstream_mark)[0]
    turin_t = place_t(upstream_t, scale)
    write_sources(out_dir, turin_t)

    with tempfile.TemporaryDirectory(prefix="turin-icons-") as tmp_name:
        tmp = Path(tmp_name)
        render = {size: ictool_png(tool, out_dir / "app-icon.icon", size, tmp / f"ios-{size}.png")
                  for size in sorted({1024, 180, *WINDOWS_ICON_SIZES})}

        (out_dir / OUTPUTS["ios"]).write_bytes(render[1024])
        (out_dir / OUTPUTS["universal"]).write_bytes(render[1024])
        (out_dir / OUTPUTS["apple_touch"]).write_bytes(render[180])
        (out_dir / OUTPUTS["favicon16"]).write_bytes(render[16])
        (out_dir / OUTPUTS["favicon32"]).write_bytes(render[32])
        ico = encode_png_ico([(size, render[size]) for size in WINDOWS_ICON_SIZES])
        (out_dir / OUTPUTS["favicon_ico"]).write_bytes(ico)
        (out_dir / OUTPUTS["windows_ico"]).write_bytes(ico)

        if not keep_macos_png:
            mac = macos_pre_tahoe(UPSTREAM_PROD / "black-macos-1024.png", upstream_mark, upstream_t, turin_t)
            (out_dir / OUTPUTS["macos"]).write_bytes(srgb_png(mac))
        build_icns(out_dir / OUTPUTS["macos"], out_dir / OUTPUTS["icns"], tmp)

    compare_sheet(out_dir)
    if not quiet:
        for name in ["app-icon.icon", "logo.svg", *OUTPUTS.values()]:
            kept = name == OUTPUTS["macos"] and keep_macos_png
            print("kept" if kept else "wrote", out_dir / name)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--t-scale", type=float, default=DEFAULT_T_SCALE,
                        help=f"scale of upstream's T path, re-centred in the tile (default {DEFAULT_T_SCALE:g}; "
                             "1.0 keeps upstream's T size)")
    parser.add_argument("--out-dir", type=Path, default=PROD,
                        help="where sources and exports are written (default assets/prod)")
    parser.add_argument("--keep-macos-png", action="store_true",
                        help="leave black-macos-1024.png untouched (e.g. a native Icon Composer "
                             "pre-Tahoe export) and build the icns and compare sheet from it")
    parser.add_argument("--scale-options", type=float, nargs="+", metavar="SCALE",
                        help="only write prod/compare-scale-options.png comparing upstream with these "
                             "T scales (rendered in a temp dir; committed exports are untouched)")
    parser.add_argument("--bundle-resources", type=Path, metavar="DIR",
                        help="only compile prod/app-icon.icon with actool into DIR (an app bundle's "
                             "Contents/Resources) and verify Assets.car + app-icon.icns were written")
    args = parser.parse_args()
    if args.bundle_resources:
        compile_bundle_icon(args.out_dir / "app-icon.icon", args.bundle_resources)
        return

    tool, version = find_ictool()
    print(f"ictool {version}: {tool}")
    if args.scale_options:
        scale_options_sheet(tool, args.scale_options, PROD / "compare-scale-options.png")
        print("wrote", PROD / "compare-scale-options.png")
        return
    export(tool, args.out_dir.resolve(), args.t_scale, args.keep_macos_png)


if __name__ == "__main__":
    main()
