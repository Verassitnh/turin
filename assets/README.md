# Brand icons

Turin's icon is upstream T3 Code's production icon (`upstream/assets/prod`) with a single T in
place of "T3". Only `prod` exists; upstream's `dev` (blueprint) and `nightly` variants are not
mirrored.

## Sources

`export-icons.py` writes the first two sources below from upstream's files and one number, the T
scale (`DEFAULT_T_SCALE = 1.0`, or `--t-scale`). Don't edit them by hand.

- `prod/app-icon.icon` is the Icon Composer project. `icon.json` is copied byte-for-byte from
  upstream prod: solid black fill, one `Text` layer at scale 8.5, neutral shadow at 0.5,
  translucency 0.5. `Assets/text.svg` is upstream's file with the T3 path swapped for Turin's T.
- `prod/logo.svg` is upstream's flat 128px `logo.svg` (10-radius black tile plus white glyph)
  with the same swap.
- `prod/wordmark.svg` is hand-written. It is the bare T at upstream's original size, with a tight
  viewBox and `currentColor`. It plays the same role as upstream's `T3Wordmark` component, for
  in-app brand slots.

The T is upstream's T subpath with the "3" dropped, at upstream's exact size and weight. It is
only translated, by `matrix(1 0 0 1 24.0691 0)`. That centres it on x = 64 and keeps the T3
pair's cap-height centre at y = 65, so its top and bottom line up with upstream's T. The result
is `M57.52 93V47.56H39.6V37H88.4V47.56H70.48V93H57.52Z`, with a box of x 39.6–88.4, y 37–93 on
the 128 grid.

**Decision: 1.0.** The user asked for "exactly like the t3 code icon except with a T instead of a
T3" and chose upstream's T at 1.0 over the earlier 1.25 enlargement. At 1.25 the cap height went
from 56 to 70 and the stem from 12.96 to 16.2. `prod/compare-scale-options.png` is the record of
that comparison: upstream next to 1.0 (chosen) and 1.25, at macOS 1024, 180, 32 and 16. Rebuild
it with `uv run assets/export-icons.py --scale-options 1.0 1.25`, which renders in a temp dir
and leaves the committed exports alone.

The scale stays a single knob, `DEFAULT_T_SCALE` in the script (`--t-scale` overrides it). The
script scales about the T's own centre, then applies the same centring. Changing the scale
rewrites the sources and every export. To preview another scale without touching `prod/`, use
`--t-scale S --out-dir /tmp/x`.

## Exports

Run `uv run assets/export-icons.py` from the repository root. It needs the `upstream` checkout
and Icon Composer 2+ (bundled with Xcode 26+). It finds `ictool` the way upstream's export script
does. `ICON_COMPOSER_TOOL` wins outright. Otherwise it takes the newest 2+ build from
`DEVELOPER_DIR`, `xcode-select -p`, a standalone `Icon Composer.app` in `/Applications` or
`~/Applications`, or any `Xcode*.app` in `/Applications` or `~/Downloads`.

The Python dependencies are pinned in the script header with an `exclude-newer` cutoff and locked
in `export-icons.py.lock`. The comparison sheets use Pillow's bundled font, not a system font.
The committed exports were produced with this toolchain:

- Icon Composer `ictool` 27.0 (bundle 129), from Xcode in `/Applications`, design generation 26
- Python 3.14.3, numpy 2.5.4, Pillow 12.3.0 (zlib-ng 1.3.1, FreeType 2.14.3), pypng 0.20220715.0,
  scipy 1.18.1
- macOS `sips` and `iconutil`, on macOS 27.0.1

With that toolchain, re-running the script reproduces every committed file byte-for-byte, so
`git status assets/` shows whether the exports are stale. A different `ictool` build changes
the ictool renders slightly, and a different zlib changes the PNG bytes but not the pixels. Both
show up as modified files without any design change.

| File | Size | Produced by |
| --- | --- | --- |
| `black-ios-1024.png`, `black-universal-1024.png` | 1024 | `ictool --platform iOS --rendition Default --design-generation 26`, same as upstream's export script |
| `turin-black-web-apple-touch-180.png`, `-favicon-16x16.png`, `-favicon-32x32.png` | 180/16/32 | ictool, rendered natively at each size |
| `turin-black-web-favicon.ico`, `turin-black-windows.ico` | 16–256 | ictool renditions at 16, 24, 32, 48, 64, 128 and 256, packed as PNG-in-ICO like upstream's `encodePngIco` |
| `black-macos-1024.png` | 1024 | Derived; see below |
| `turin-black.icns` | 16–1024 | `sips` downscales of the macOS PNG, packed with `iconutil` |
| `compare-t3-vs-turin.png` | – | Side-by-side comparison with upstream at 1024, 180, 32 and 16 |

Every raster, including each ICO and ICNS member, is 8-bit RGBA sRGB with an `sRGB` chunk, the
same format as upstream's exports. Icon Composer 27 writes 16-bit Display P3. The script decodes
that at full depth and converts P3 to sRGB (both use the sRGB curve and D65 white). It then
rounds to 8 bits. Rendering upstream's own `.icon` through this path gives a mean difference of
0.16 levels from upstream's committed 1024 PNG.

### macOS PNG

Upstream exports this PNG by hand with Icon Composer's GUI-only **macOS pre-Tahoe** preset
(824px body inset 100px, plus native shadow). `ictool` cannot produce that preset, and GUI
scripting was not available, so the script derives it from upstream's native export
`upstream/assets/prod/black-macos-1024.png`:

1. Erase the T3 glyph from upstream's PNG. The surrounding body is pure black, and the body, rim
   and shadow are untouched.
2. Take the natively lit pixels of upstream's T (vertical gradient, lighter bottom lip, edge
   treatment). Map them through the same affine transform that turns upstream's T path into
   Turin's.
3. Composite them with the exact anti-aliased coverage of Turin's path.

Checks:

- With an identity transform, the pipeline reproduces upstream's PNG byte-for-byte outside the
  glyph. Inside, it matches exactly except for a 1px anti-aliased edge (about 0.2px sub-pixel
  offset).
- `xcrun actool` builds a native 256px pre-Tahoe icns from the `.icon` project. Compared with
  that render, this PNG differs by a mean of 3.2 levels. Upstream's own GUI export differs from
  its actool render by 3.8 levels.

For a fully native PNG, open `prod/app-icon.icon` in Icon Composer. Export with Platform
`macOS pre-Tahoe`, Appearance `Default`, Size `1024pt`, Scale `1×` to `prod/black-macos-1024.png`.
Then run `uv run assets/export-icons.py --keep-macos-png`, which leaves that PNG untouched. The
icns and the compare sheet are rebuilt from it. Once a native PNG is committed, always pass
`--keep-macos-png`. Without the flag, the script overwrites it with the derived PNG.

## Shipping on macOS 26+

For the Liquid Glass icon on Tahoe, compile the project into the bundle's resources:

```sh
uv run assets/export-icons.py --bundle-resources <Bundle>/Contents/Resources
```

This runs:

```sh
xcrun actool "$PWD/assets/prod/app-icon.icon" --compile <Bundle>/Contents/Resources \
  --platform macosx --minimum-deployment-target 11.0 --app-icon app-icon \
  --output-partial-info-plist <tmp>/partial.plist
```

It writes `Assets.car` and `app-icon.icns`. Set `CFBundleIconName = app-icon` and
`CFBundleIconFile = app-icon` in Info.plist, which are the keys in the partial plist. The
`app-icon.icns` fallback only goes up to 256px. On macOS 11–15 the full-size icon comes from
`Assets.car`.

`--app-icon` must be the `.icon` file's basename. With any other name, actool exits 0 and writes
nothing, and the bundle ships without an icon. The flag fails unless `Assets.car` and
`app-icon.icns` are non-empty and the plist keys match. If you call actool yourself, check the
outputs the same way.

To ship only the classic icon, copy `turin-black.icns` into `Contents/Resources` and set
`CFBundleIconFile = turin-black` instead.

Do not edit the generated PNG, ICO or ICNS files directly.
