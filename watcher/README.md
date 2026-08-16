# sigiltty-watcher

Rust implementation of the watcher behavior contract ([docs/PROTOCOL.md](../docs/PROTOCOL.md) §9): one thread per target pane runs the level-triggered `herdr agent wait` loop, a 10-second hysteresis window settles herdr's flapping status detector, and the stable transitions the reporting rule admits (`herdr::report_status` — →blocked, →done, and the `working → idle` finish herdr counted as already seen, which travels as `done`) are HPKE-sealed per device and posted to the relay.

```bash
cargo test          # 45 cases: scripted watch loop, CLI parsing, status/uninstall, log cap, HPKE roundtrip, flock, date rendering
cargo build --release
```

The binary describes itself, so managing a deployment needs no side files (PROTOCOL §10):

```bash
sigiltty-watcher [--config <path>]              # watch (default, and what the bootstrap starts)
sigiltty-watcher --version                      # bare semver — the authoritative installed version
sigiltty-watcher --help
sigiltty-watcher status [--config <path>]
sigiltty-watcher uninstall [--config <path>] [--keep-binary]
```

`status` prints one machine-readable line with no user content in it — no server name, no pane IDs, no device credentials:

```
config=present version=0.1.3 running=yes pid=8421 expires=4102444800 fingerprint=9f2c0d4ab1 targets=1
config=absent version=0.1.3 running=no
```

`uninstall` stops the watcher, then removes the config, the lock, the log, the legacy version marker and (unless `--keep-binary`) the installed binary, plus any directory that leaves empty. Removing nothing is still exit 0 — the app retries removal on its next connection, so this has to be idempotent. Exit 1 means something survived; exit 2 is a usage error.

Config defaults to `$XDG_CONFIG_HOME/sigiltty/relay.json` (fallback `~/.config/sigiltty/relay.json`), read once — the app rewrites and restarts on any change. Single instance via flock on `watcher.lock` next to the config, PID inside; the **lock** is what says a watcher is alive, since a PID outlives its process and gets recycled, so `status` and `uninstall` decide liveness with a non-blocking flock probe and only signal a PID whose holder that probe proved is still there. Exits silently on TTL expiry, config removal, lock loss, or exhausted failure retries; the app's per-connection health check is the only recovery path.

Diagnostics go to stderr (stdout belongs to the commands that answer questions), each line stamped in ISO-8601 **at the server's own UTC offset, with that offset in the line** (`src/timefmt.rs`, libc's zone so DST is followed). Local time is what you want when reading a log on the box it came from; the offset is what makes it still readable anywhere else. A server left on UTC — most cloud images — collapses to `Z` and lines up with the relay's own lines and the `*_utc` database columns with no conversion at all. The watcher is started by the bootstrap rather than an init system, so nothing else would stamp them.

```
[2026-08-15T19:27:17+08:00] watching 2 target(s) for 1 device(s), expires at 2026-09-14T19:27:17+08:00
[2026-08-15T19:31:02+08:00] [%4] relay accepted 1 entr(ies): sent
```

That file is capped at **5 MB** (`src/logcap.rs`, checked on the supervisor's once-a-minute tick): over the cap it is rewritten as its own second half, cut at a line boundary, and the watcher's write offset moves with it — so it swings between 2.5 MB and 5 MB rather than growing for the whole seven-day TTL. Keeping the tail rather than the head is deliberate: when something goes wrong, the recent lines are the ones worth having. The bootstrap's per-run truncation still happens; this is the ceiling for a run nobody has reconnected to. Only a regular file whose device+inode match `$XDG_DATA_HOME/sigiltty/watcher.log` is ever touched, so running the watcher in a terminal (or piping it anywhere) is unaffected.

Release targets (bootstrap contract, PROTOCOL §10), built by CI and published to GitHub Releases on `v*` tags: `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl` (static), `x86_64-apple-darwin`, `aarch64-apple-darwin` (native slices).
