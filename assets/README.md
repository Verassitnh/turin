# Brand icons

Turin's icon is upstream T3 Code's production icon (`upstream/assets/prod`) with a single T in
place of "T3". Only `prod` exists; upstream's `dev` (blueprint) and `nightly` variants are not
mirrored.

## Sources

- `prod/app-icon.icon` is the Icon Composer project and the source of truth. `icon.json` is
  byte-identical to upstream prod: solid black fill, one `Text` layer at scale 8.5, neutral
  shadow at 0.5, translucency 0.5.
- `prod/app-icon.icon/Assets/text.svg` is the mark: upstream's T subpath with the "3" removed,
  scaled by 1.25 and re-centred. The transform `matrix(1.25 0 0 1.25 14.086375 -16.25)` is baked
  into the path (`M55.9 100V43.2H33.5V30H94.5V43.2H72.1V100H55.9Z`). The T's box is
  x 33.5–94.5, y 30–100 on the 128 grid. That centres it horizontally and keeps upstream's
  cap-height centre (y = 65, one unit below the canvas centre). The top-heavy T needs that slight
  drop to sit optically centred.
- `prod/logo.svg` is the flat 128px mark with the same structure as upstream's `logo.svg`
  (10-radius black tile plus the white glyph).
- `prod/wordmark.svg` is the bare T in upstream's original size, with a tight viewBox and
  `currentColor`. It plays the same role as upstream's `T3Wordmark` component, for in-app brand
  slots.

Why scale 1.25: at 1.0 the lone T looks lost in the tile. At 1.4 its stroke gets visibly heavier
than T3's. At 1.25 the T fills the tile about as fully as the T3 pair does at 1024, 180, 32 and
16px.

## Exports

Run `uv run assets/export-icons.py` from the repository root. It needs the `upstream` checkout
and Icon Composer 2+ (bundled with Xcode 26+). It finds `ictool` the way upstream's export script
does. `ICON_COMPOSER_TOOL` wins outright. Otherwise it takes the newest 2+ build from
`DEVELOPER_DIR`, `xcode-select -p`, a standalone `Icon Composer.app` in `/Applications` or
`~/Applications`, or any `Xcode*.app` in `/Applications` or `~/Downloads`. Its output is deterministic, so
`git status assets/` after a run shows whether the committed exports are stale.

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
`macOS pre-Tahoe`, Appearance `Default`, Size `1024pt`, Scale `1×` to `prod/black-macos-1024.png`,
then re-run only the icns step (or the whole script, then restore the PNG).

## Shipping on macOS 26+

For the Liquid Glass icon on Tahoe, compile the project into the bundle:

```sh
xcrun actool assets/prod/app-icon.icon --compile <Bundle>/Contents/Resources \
  --platform macosx --minimum-deployment-target 11.0 --app-icon AppIcon \
  --output-partial-info-plist /tmp/partial.plist
```

This writes `Assets.car` and `AppIcon.icns`. Set `CFBundleIconName = AppIcon` and
`CFBundleIconFile = AppIcon`. To ship only the classic icon, copy `turin-black.icns` in as the
`CFBundleIconFile` instead.

Do not edit the generated PNG, ICO or ICNS files directly.
