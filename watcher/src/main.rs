//! sigiltty-watcher — server-side herdr agent watcher for SigilTTY
//! offline push (docs/PROTOCOL.md; design: SigilTTY ADR-0014). Reads the
//! app-written config once, watches every target pane on its own thread,
//! reports stable →blocked/→done transitions to the relay, and dies
//! silently at TTL expiry / config removal / persistent failure — the
//! app's per-connection health check is the only recovery path.

mod config;
mod herdr;
mod lock;
mod relay;
mod seal;
mod timefmt;
mod watch;

use herdr::{parse_wait_output, wait_args, AgentStatus, WaitOutcome};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// stderr, because stdout is reserved for `--version`. The timestamp is our
/// own: the watcher is started by the app's bootstrap, not by an init system
/// that would stamp lines for us, so a bare message is undatable once it
/// lands in a redirect.
fn log(message: &str) {
    eprintln!("[{}] {message}", timefmt::iso8601_local(now_unix()));
}

/// Spawns the herdr CLI directly (no shell — we ARE on the server, so the
/// login-shell quoting traps of the SSH path don't apply). The remote
/// --timeout is the only clock; stdout and stderr are parsed together.
struct ProcessRemote {
    binary: String,
    pane_id: String,
    session: Option<String>,
}

impl watch::Remote for ProcessRemote {
    fn wait(&self, until: &[AgentStatus], timeout_ms: u64) -> WaitOutcome {
        let args = wait_args(&self.pane_id, until, timeout_ms, self.session.as_deref());
        let output = match Command::new(&self.binary).args(&args).output() {
            Ok(output) => output,
            Err(e) => return WaitOutcome::Failure(format!("spawn {}: {e}", self.binary)),
        };
        let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
        combined.push('\n');
        combined.push_str(&String::from_utf8_lossy(&output.stderr));
        parse_wait_output(&combined)
    }
}

fn main() {
    let mut config_path = config::default_config_path();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--version" => {
                println!("{}", env!("CARGO_PKG_VERSION"));
                return;
            }
            "--config" => {
                let Some(path) = args.next() else {
                    eprintln!("--config requires a path");
                    std::process::exit(2);
                };
                config_path = path.into();
            }
            other => {
                eprintln!("unknown argument: {other}");
                std::process::exit(2);
            }
        }
    }

    // Exit conditions are all silent successes (PROTOCOL §9): no config =
    // opt-in revoked; expired = TTL did its job; lock lost = incumbent runs.
    let cfg = match config::load(&config_path) {
        Ok(cfg) => cfg,
        Err(e) => {
            log(&format!("no usable config, exiting: {e}"));
            return;
        }
    };
    if cfg.expires_at <= now_unix() {
        log("config expired, exiting");
        return;
    }
    let lock_path = config_path.with_file_name("watcher.lock");
    let Some(_lock) = lock::acquire(&lock_path) else {
        log("another instance holds the lock, exiting");
        return;
    };
    if cfg.targets.is_empty() || cfg.devices.is_empty() {
        log("nothing to watch or nowhere to send, exiting");
        return;
    }

    log(&format!(
        "watching {} target(s) for {} device(s), expires at {}",
        cfg.targets.len(), cfg.devices.len(), timefmt::iso8601_local(cfg.expires_at),
    ));

    let cfg = Arc::new(cfg);
    let shutdown = Arc::new(AtomicBool::new(false));
    let client = Arc::new(relay::RelayClient::new(&cfg.relay_url));

    let handles: Vec<_> = cfg
        .targets
        .iter()
        .cloned()
        .map(|target| {
            let cfg = Arc::clone(&cfg);
            let shutdown = Arc::clone(&shutdown);
            let client = Arc::clone(&client);
            std::thread::spawn(move || watch_target(&cfg, target, &client, &shutdown))
        })
        .collect();

    // Supervisor: TTL and config-presence are re-checked here; watch
    // threads block in five-minute waits, so exiting the PROCESS is the
    // prompt path — orphaned herdr waits die of SIGPIPE at their next
    // write, at most one remote timeout away (the documented reaper).
    loop {
        std::thread::sleep(Duration::from_secs(60));
        let expired = cfg.expires_at <= now_unix();
        let revoked = !config_path.exists();
        let all_done = handles.iter().all(|h| h.is_finished());
        if expired || revoked || all_done {
            log(if expired {
                "ttl expired, exiting"
            } else if revoked {
                "config removed, exiting"
            } else {
                "all watches ended, exiting"
            });
            shutdown.store(true, Ordering::Relaxed);
            std::process::exit(0);
        }
    }
}

fn watch_target(
    cfg: &config::RelayConfig,
    target: config::Target,
    client: &relay::RelayClient,
    shutdown: &AtomicBool,
) {
    let remote = ProcessRemote {
        binary: cfg.herdr_binary.clone(),
        pane_id: target.pane_id.clone(),
        session: target.herdr_session.clone(),
    };
    let expires_at = cfg.expires_at;
    let pane = target.pane_id.clone();
    let sleep = |d: Duration| std::thread::sleep(d);
    let target_log = move |m: &str| log(&format!("[{pane}] {m}"));

    let mut report = |agent: &herdr::AgentInfo| {
        client.report(cfg, &target, agent, now_unix(), &sleep, &target_log);
    };
    let mut hooks = watch::WatchHooks {
        report: &mut report,
        should_continue: &|| !shutdown.load(Ordering::Relaxed) && now_unix() < expires_at,
        sleep: &sleep,
        log: &target_log,
    };
    watch::run(&remote, &watch::Timing::default(), &mut hooks);
}
