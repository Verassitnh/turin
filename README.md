# Turin

t3 fork but without all the bullshit.

A Rust/GPUI port of [T3 Code](https://github.com/pingdotgg/t3code): desktop only,
local only, GitHub only. Providers: Codex, Claude, OpenCode, Cursor.

```sh
DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer cargo run -p turin
```

On macOS an unbundled run sets the Dock icon from the app icon. To build a
double-clickable app:

```sh
scripts/bundle-macos.sh            # -> target/bundle/Turin.app
```

The script builds `--release`, writes `Turin.app` with `Info.plist` (bundle id
`dev.turin.app`, `CFBundleIconFile` = `Turin`), and makes `Turin.icns` from
`assets/prod/black-macos-1024.png` with `sips` and `iconutil`. It then ad-hoc
signs the bundle. It needs no network access. Pass a directory as the first
argument to write the bundle somewhere else.

The bundle has no `Assets.car`. The icon source has no light, dark or tinted
variants, so the script skips `actool`. The bundle therefore ships the classic
`.icns` only, with no Liquid Glass compilation. See
[assets/README.md](assets/README.md) for the `actool` path if that changes.

See [PORTING.md](PORTING.md) for how the port maps onto upstream.
