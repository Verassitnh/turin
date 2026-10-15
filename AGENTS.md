# Agents

Read PORTING.md before touching code. It is the source of truth for upstream
mapping, Effect→Rust conventions, the cut list, and the Deviations log.

- Upstream reference checkout: `./upstream` (symlink to ../upstream).
- Build env: `export PATH=$HOME/.cargo/bin:$PATH DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer`
  (GPUI needs the full Xcode toolchain for the Metal shader compiler).
- Check: `cargo build --workspace && cargo test --workspace && cargo clippy --workspace`.
- Only edit the crates/modules you own for your task; other agents run in parallel.
