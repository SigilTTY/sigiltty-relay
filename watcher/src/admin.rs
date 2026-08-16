//! `status` and `uninstall` (docs/PROTOCOL.md §10): the deployment's own
//! account of itself, and its removal.
//!
//! **flock is the liveness truth, not the PID file.** The lock file records a
//! PID for convenience, but a PID only means something while its process
//! lives: after a crash the number is a corpse's, and once the OS recycles it
//! the same number belongs to a stranger. So both commands ask the kernel
//! instead — open the lock file READ-ONLY and try to take it. Taking it means
//! nobody holds it, which means no watcher is running and the PID inside is
//! stale; failing to take it means the PID inside is a live holder's, and
//! only then is it a legitimate target for a signal. This is also why the
//! probe cannot reuse `lock::acquire`, which truncates the file and writes
//! its own PID — a probe must leave no trace.
//!
//! Every path is injected through `Paths`, so the removal logic is tested
//! against a temp directory rather than against a developer's real config.

use crate::config;
use std::io::Read;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Everything a deployment consists of. Resolved from the config path (which
/// the app always passes explicitly) plus the XDG data dir.
pub struct Paths {
    pub config: PathBuf,
    pub config_dir: PathBuf,
    pub lock: PathBuf,
    pub data_dir: PathBuf,
    pub binary: PathBuf,
    /// Legacy: the install-time marker the version check used to read. The
    /// binary answers `--version` for itself now, so this is only ever
    /// removed, never written or believed.
    pub version_marker: PathBuf,
    pub log: PathBuf,
}

/// `$XDG_DATA_HOME/sigiltty`, falling back to `~/.local/share/sigiltty` —
/// the same pair the app's bootstrap resolves, and the mirror image of
/// `config::config_dir`.
pub fn data_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("sigiltty");
        }
    }
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()))
        .join(".local")
        .join("share")
        .join("sigiltty")
}

impl Paths {
    pub fn resolve(config: &Path) -> Paths {
        let data = data_dir();
        Paths {
            config_dir: config.parent().unwrap_or(Path::new(".")).to_path_buf(),
            lock: config.with_file_name("watcher.lock"),
            config: config.to_path_buf(),
            binary: data.join("sigiltty-watcher"),
            version_marker: data.join("sigiltty-watcher.version"),
            log: data.join("watcher.log"),
            data_dir: data,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Instance {
    /// Nobody holds the lock. Any PID in the file belongs to a dead run.
    NotRunning,
    /// The lock is held; the PID is the holder's own (None if the file was
    /// somehow written without one).
    Running(Option<u32>),
}

pub fn probe(lock: &Path) -> Instance {
    // No create: probing must never bring the lock file into existence.
    let Ok(mut file) = std::fs::File::open(lock) else {
        return Instance::NotRunning;
    };
    // flock needs no write access — a read-only fd locks just as well.
    let free = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
    if free {
        // Dropping the file releases what we just took; nothing was written.
        return Instance::NotRunning;
    }
    let mut contents = String::new();
    let _ = file.read_to_string(&mut contents);
    Instance::Running(contents.lines().next().and_then(|l| l.trim().parse().ok()))
}

/// One line of `key=value`, the app's health probe in a single command.
///
/// Never prints device credentials — `expires` and `fingerprint` are the only
/// config content here, and both are the app's own routing metadata.
///
/// A config that exists but cannot be parsed reports `present` with neither
/// field: to the app that reads as a deployment whose TTL and target set are
/// unknown, whose repair is a reinstall — which rewrites the broken file.
pub fn status_line(paths: &Paths, version: &str) -> String {
    let present = paths.config.exists();
    let mut line = format!(
        "config={} version={}",
        if present { "present" } else { "absent" },
        version,
    );
    match probe(&paths.lock) {
        Instance::Running(pid) => {
            line.push_str(" running=yes");
            if let Some(pid) = pid {
                line.push_str(&format!(" pid={pid}"));
            }
        }
        Instance::NotRunning => line.push_str(" running=no"),
    }
    if let Ok(cfg) = config::load(&paths.config) {
        line.push_str(&format!(" expires={}", cfg.expires_at));
        if !cfg.targets_fingerprint.is_empty() {
            line.push_str(&format!(" fingerprint={}", cfg.targets_fingerprint));
        }
        line.push_str(&format!(" targets={}", cfg.targets.len()));
    }
    line
}

/// SIGTERM the lock holder and wait for the lock to come free — the release
/// IS the confirmation, and unlike `kill -0` it cannot be fooled by a recycled
/// PID. The watcher installs no signal handler, so SIGTERM ends it at once;
/// the SIGKILL after two seconds is for a process wedged somewhere the first
/// signal cannot reach it.
///
/// Orphaned `herdr agent wait` children are left alone deliberately: they die
/// of SIGPIPE at their next write (PROTOCOL §9's documented reaper), and
/// pattern-killing by name on someone else's server is not ours to do.
///
/// Returns whether nothing is running when it returns.
pub fn stop(lock: &Path, sleep: &dyn Fn(Duration), log: &dyn Fn(&str)) -> bool {
    let pid = match probe(lock) {
        Instance::NotRunning => return true,
        Instance::Running(None) => {
            log("a watcher holds the lock but wrote no pid; not signalling");
            return false;
        }
        Instance::Running(Some(pid)) => pid,
    };
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    if wait_for_release(lock, 20, sleep) {
        log(&format!("stopped watcher (pid {pid})"));
        return true;
    }
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
    if wait_for_release(lock, 10, sleep) {
        log(&format!("stopped watcher (pid {pid}, needed SIGKILL)"));
        return true;
    }
    log(&format!("watcher (pid {pid}) is still running"));
    false
}

fn wait_for_release(lock: &Path, tries: u32, sleep: &dyn Fn(Duration)) -> bool {
    for _ in 0..tries {
        sleep(Duration::from_millis(100));
        if probe(lock) == Instance::NotRunning {
            return true;
        }
    }
    false
}

/// Stop the watcher and take the deployment off the server. Idempotent by
/// construction — a file that is already gone is not an error — because the
/// app retries a failed removal on its next connection, and a second attempt
/// that reported failure would keep it retrying forever.
///
/// Returns whether the server is clean; the caller turns that into the exit
/// status.
pub fn uninstall(
    paths: &Paths,
    keep_binary: bool,
    sleep: &dyn Fn(Duration),
    log: &dyn Fn(&str),
) -> bool {
    let mut clean = stop(&paths.lock, sleep, log);

    // `.new` siblings are the installer's temp names (PROTOCOL §10: partial
    // downloads never take the final name). A failed install leaves them
    // behind, and they would otherwise keep the directory from emptying.
    let config_new = with_suffix(&paths.config, ".new");
    let binary_new = with_suffix(&paths.binary, ".new");
    let mut doomed = vec![
        &paths.config,
        &config_new,
        &paths.lock,
        &paths.version_marker,
        &paths.log,
        &binary_new,
    ];
    if !keep_binary {
        doomed.push(&paths.binary);
    }

    for path in doomed {
        match std::fs::remove_file(path) {
            Ok(()) => log(&format!("removed {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                log(&format!("could not remove {}: {e}", path.display()));
                clean = false;
            }
        }
    }
    if keep_binary && paths.binary.exists() {
        log(&format!("kept {}", paths.binary.display()));
    }

    // remove_dir refuses a non-empty directory, and that refusal IS the
    // check: whatever else lives there is not ours to delete.
    let mut emptied = 0;
    let mut dirs = vec![&paths.config_dir];
    if paths.data_dir != paths.config_dir {
        dirs.push(&paths.data_dir);
    }
    for dir in dirs {
        if std::fs::remove_dir(dir).is_ok() {
            emptied += 1;
        }
    }
    if emptied > 0 {
        log(&format!("removed {emptied} empty director{}", if emptied == 1 { "y" } else { "ies" }));
    }

    log(if clean { "uninstalled" } else { "uninstall incomplete" });
    clean
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Per-test scratch root. Uses a counter as well as the pid because the
    /// whole suite runs in one process.
    fn scratch(name: &str) -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "sigiltty-admin-{}-{}-{name}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A full deployment on disk: config dir and data dir with every file the
    /// bootstrap creates.
    fn deployment(root: &Path) -> Paths {
        let config_dir = root.join("config");
        let data_dir = root.join("data");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::create_dir_all(&data_dir).unwrap();
        let paths = Paths {
            config: config_dir.join("relay.json"),
            lock: config_dir.join("watcher.lock"),
            binary: data_dir.join("sigiltty-watcher"),
            version_marker: data_dir.join("sigiltty-watcher.version"),
            log: data_dir.join("watcher.log"),
            config_dir,
            data_dir,
        };
        std::fs::write(&paths.config, config_json(4_102_444_800)).unwrap();
        // A PID nobody is signalling: the lock is free, so the stop path
        // never gets as far as reading it (which is the point of the probe).
        std::fs::write(&paths.lock, "999999\n").unwrap();
        std::fs::write(&paths.binary, b"\x7fELF not really").unwrap();
        std::fs::write(&paths.version_marker, "v0.1.3").unwrap();
        std::fs::write(&paths.log, "[2026-08-16T09:00:00Z] watching\n").unwrap();
        paths
    }

    fn config_json(expires: u64) -> String {
        format!(
            r#"{{"v":1,"relayURL":"https://r.example","serverID":"S","serverName":"box",
                 "herdrBinary":"/usr/bin/herdr","expiresAt":{expires},
                 "targetsFingerprint":"9f2c0d",
                 "devices":[{{"routingID":"ROUTING-ID","secret":"SHARED-SECRET",
                              "publicKey":"DEVICE-PUBKEY","platform":"ios"}}],
                 "targets":[{{"paneID":"w1:p4","herdrSession":null,"label":null}}]}}"#
        )
    }

    fn collect(lines: &std::cell::RefCell<Vec<String>>) -> impl Fn(&str) + '_ {
        move |m: &str| lines.borrow_mut().push(m.to_string())
    }

    #[test]
    fn the_lock_not_the_pid_file_says_whether_a_watcher_runs() {
        let root = scratch("probe");
        let lock = root.join("watcher.lock");
        // No lock file at all.
        assert_eq!(probe(&lock), Instance::NotRunning);
        // A lock file left behind by a dead run: the PID inside is a corpse's
        // (or, worse, has been recycled), and flock says so.
        std::fs::write(&lock, "999999\n").unwrap();
        assert_eq!(probe(&lock), Instance::NotRunning);
        // Probing must not have created or rewritten anything.
        assert_eq!(std::fs::read_to_string(&lock).unwrap(), "999999\n");
        // Held for real: flock is per open-file-description, so our own
        // second open is refused exactly as another process would be.
        let held = crate::lock::acquire(&lock).unwrap();
        assert_eq!(probe(&lock), Instance::Running(Some(std::process::id())));
        drop(held);
        assert_eq!(probe(&lock), Instance::NotRunning);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn status_reports_a_live_deployment_without_leaking_credentials() {
        let root = scratch("status");
        let paths = deployment(&root);
        let line = status_line(&paths, "0.1.4");
        assert_eq!(
            line,
            "config=present version=0.1.4 running=no expires=4102444800 fingerprint=9f2c0d targets=1"
        );
        // Device credentials sit in the file this line was built from, and
        // the line goes back over SSH into the app's logs.
        for leak in ["SHARED-SECRET", "DEVICE-PUBKEY", "ROUTING-ID"] {
            assert!(!line.contains(leak), "{line} leaks {leak}");
        }

        let held = crate::lock::acquire(&paths.lock).unwrap();
        let running = status_line(&paths, "0.1.4");
        assert!(running.contains(&format!("running=yes pid={}", std::process::id())));
        drop(held);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn status_of_a_server_that_was_never_deployed() {
        let root = scratch("status-absent");
        let paths = Paths::resolve(&root.join("sigiltty").join("relay.json"));
        assert_eq!(status_line(&paths, "0.1.4"), "config=absent version=0.1.4 running=no");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A config the watcher cannot parse still counts as deployed: the app's
    /// repair for a missing TTL is a reinstall, which rewrites the file.
    #[test]
    fn status_of_a_broken_config_is_present_with_nothing_else() {
        let root = scratch("status-broken");
        let paths = deployment(&root);
        std::fs::write(&paths.config, "{ this is not json").unwrap();
        assert_eq!(status_line(&paths, "0.1.4"), "config=present version=0.1.4 running=no");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn uninstall_leaves_the_server_as_if_never_deployed() {
        let root = scratch("uninstall");
        let paths = deployment(&root);
        // A failed install's leftovers, which would otherwise keep the data
        // directory from emptying.
        std::fs::write(with_suffix(&paths.binary, ".new"), b"partial").unwrap();

        let lines = std::cell::RefCell::new(Vec::new());
        assert!(uninstall(&paths, false, &|_| {}, &collect(&lines)));

        for path in [&paths.config, &paths.lock, &paths.binary, &paths.version_marker, &paths.log] {
            assert!(!path.exists(), "{} survived", path.display());
        }
        assert!(!paths.config_dir.exists() && !paths.data_dir.exists());
        let log = lines.borrow().join("\n");
        assert!(log.contains("removed 2 empty directories"), "{log}");
        assert!(log.ends_with("uninstalled"), "{log}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn keep_binary_removes_the_deployment_but_not_the_download() {
        let root = scratch("uninstall-keep");
        let paths = deployment(&root);
        let lines = std::cell::RefCell::new(Vec::new());
        assert!(uninstall(&paths, true, &|_| {}, &collect(&lines)));

        assert!(paths.binary.exists());
        assert!(!paths.config.exists() && !paths.lock.exists());
        assert!(!paths.version_marker.exists() && !paths.log.exists());
        // The config dir emptied; the data dir still holds the binary.
        assert!(!paths.config_dir.exists());
        assert!(paths.data_dir.exists());
        assert!(lines.borrow().iter().any(|l| l.starts_with("kept ")));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The app retries a failed removal on its next connection, so the second
    /// run must succeed too — a "nothing to remove" that reported failure
    /// would keep it retrying forever.
    #[test]
    fn uninstalling_twice_is_still_a_success() {
        let root = scratch("uninstall-twice");
        let paths = deployment(&root);
        assert!(uninstall(&paths, false, &|_| {}, &|_| {}));
        let lines = std::cell::RefCell::new(Vec::new());
        assert!(uninstall(&paths, false, &|_| {}, &collect(&lines)));
        assert_eq!(lines.borrow().as_slice(), ["uninstalled"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Someone else's files in our directory are not ours to delete.
    #[test]
    fn a_directory_holding_anything_else_survives() {
        let root = scratch("uninstall-shared");
        let paths = deployment(&root);
        let stranger = paths.data_dir.join("notes.txt");
        std::fs::write(&stranger, "not mine").unwrap();
        assert!(uninstall(&paths, false, &|_| {}, &|_| {}));
        assert!(stranger.exists());
        assert!(paths.data_dir.exists() && !paths.binary.exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
