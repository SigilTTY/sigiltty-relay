# sigiltty-watcher

Rust implementation of the watcher behavior contract ([docs/PROTOCOL.md](../docs/PROTOCOL.md) §9): one thread per target pane runs the level-triggered `herdr agent wait` loop, a 10-second hysteresis window settles herdr's flapping status detector, and stable →blocked/→done transitions — filtered by the awareness model (`src/watch.rs` header) — are HPKE-sealed per device and posted to the relay.

```bash
cargo test          # 18 cases: scripted watch loop, CLI parsing, HPKE roundtrip, flock
cargo build --release
./target/release/sigiltty-watcher --version
./target/release/sigiltty-watcher [--config /path/to/relay.json]
```

Config defaults to `$XDG_CONFIG_HOME/sigiltty/relay.json` (fallback `~/.config/sigiltty/relay.json`), read once — the app rewrites and restarts on any change. Single instance via flock on `watcher.lock` next to the config (PID inside). Exits silently on TTL expiry, config removal, lock loss, or exhausted failure retries; the app's per-connection health check is the only recovery path.

Release targets (bootstrap contract, PROTOCOL §10), built by CI and published to GitHub Releases on `v*` tags: `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl` (static), `x86_64-apple-darwin`, `aarch64-apple-darwin` (native slices).
