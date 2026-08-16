//! sigiltty-watcher — server-side herdr agent watcher for SigilTTY
//! offline push (docs/PROTOCOL.md; design: SigilTTY ADR-0014). Reads the
//! app-written config once, watches every target pane on its own thread,
//! reports the stable transitions `herdr::report_status` admits to the
//! relay (→blocked, →done, and the seen finish `working → idle`), and dies
//! silently at TTL expiry / config removal / persistent failure — the
//! app's per-connection health check is the only recovery path.

mod admin;
mod cli;
mod config;
mod herdr;
mod lock;
mod logcap;
mod relay;
mod seal;
mod timefmt;
mod watch;

use herdr::{parse_wait_output, wait_args, AgentStatus, WaitOutcome};
use std::os::unix::io::AsRawFd;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// stderr, because stdout belongs to the commands that answer questions
/// (`--version`, `status`, `uninstall`) and their output is parsed. The
/// timestamp is our own: the watcher is started by the app's bootstrap, not
/// by an init system that would stamp lines for us, so a bare message is
/// undatable once it lands in a redirect.
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

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() {
    let command = match cli::parse(std::env::args().skip(1), config::default_config_path()) {
        Ok(command) => command,
        Err(message) => {
            eprintln!("{message}\ntry: sigiltty-watcher --help");
            std::process::exit(2);
        }
    };
    match command {
        // The bare semver, unchanged since 0.1.0 and unchanged forever: this
        // is what the bootstrap compares against the version it pins, and it
        // replaced a marker file precisely because it cannot go stale.
        cli::Command::Version => println!("{VERSION}"),
        cli::Command::Help => println!("{}", cli::USAGE),
        cli::Command::Status(config) => {
            println!("{}", admin::status_line(&admin::Paths::resolve(&config), VERSION))
        }
        cli::Command::Uninstall { config, keep_binary } => {
            // Report to stdout: this is an answer to whoever asked for the
            // removal, not the running watcher's diagnostics.
            let clean = admin::uninstall(
                &admin::Paths::resolve(&config),
                keep_binary,
                &|d| std::thread::sleep(d),
                &|m| println!("{m}"),
            );
            if !clean {
                std::process::exit(1);
            }
        }
        cli::Command::Run(config) => run(config),
    }
}

fn run(config_path: std::path::PathBuf) {
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
    let log_path = admin::Paths::resolve(&config_path).log;
    loop {
        std::thread::sleep(Duration::from_secs(60));
        // The log is the only record of what this watcher decided, and a run
        // may last the full TTL, so it gets a ceiling here rather than
        // relying on the app reconnecting to truncate it (PROTOCOL §9).
        if let Some(note) =
            logcap::enforce(std::io::stderr().as_raw_fd(), &log_path, logcap::CAP_BYTES)
        {
            log(&note);
        }
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
