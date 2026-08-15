# sigiltty-watcher

Rust implementation of the watcher behavior contract ([docs/PROTOCOL.md](../docs/PROTOCOL.md) §9): one thread per target pane runs the level-triggered `herdr agent wait` loop, a 10-second hysteresis window settles herdr's flapping status detector, and stable →blocked/→done transitions — filtered by the awareness model (`src/watch.rs` header) — are HPKE-sealed per device and posted to the relay.

```bash
cargo test          # 24 cases: scripted watch loop, CLI parsing, HPKE roundtrip, flock, date rendering
cargo build --release
./target/release/sigiltty-watcher --version
./target/release/sigiltty-watcher [--config /path/to/relay.json]
```

Config defaults to `$XDG_CONFIG_HOME/sigiltty/relay.json` (fallback `~/.config/sigiltty/relay.json`), read once — the app rewrites and restarts on any change. Single instance via flock on `watcher.lock` next to the config (PID inside). Exits silently on TTL expiry, config removal, lock loss, or exhausted failure retries; the app's per-connection health check is the only recovery path.

Diagnostics go to stderr (stdout is reserved for `--version`), each line stamped in ISO-8601 **at the server's own UTC offset, with that offset in the line** (`src/timefmt.rs`, libc's zone so DST is followed). Local time is what you want when reading a log on the box it came from; the offset is what makes it still readable anywhere else. A server left on UTC — most cloud images — collapses to `Z` and lines up with the relay's own lines and the `*_utc` database columns with no conversion at all. The watcher is started by the bootstrap rather than an init system, so nothing else would stamp them.

```
[2026-08-15T19:27:17+08:00] watching 2 target(s) for 1 device(s), expires at 2026-09-14T19:27:17+08:00
[2026-08-15T19:31:02+08:00] [%4] relay accepted 1 entr(ies): sent
```

Release targets (bootstrap contract, PROTOCOL §10), built by CI and published to GitHub Releases on `v*` tags: `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl` (static), `x86_64-apple-darwin`, `aarch64-apple-darwin` (native slices).
