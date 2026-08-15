//! Single instance per server (docs/PROTOCOL.md §9): flock on
//! `<config dir>/watcher.lock`, PID written for the app's stop/restart.
//! Losing the race exits silently — the incumbent is doing the job.

use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::Path;

/// Holds the flock for the process lifetime; dropping releases it.
pub struct InstanceLock {
    _file: std::fs::File,
}

pub fn acquire(path: &Path) -> Option<InstanceLock> {
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)
        .ok()?;
    let locked = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
    if !locked {
        return None;
    }
    let _ = file.set_len(0);
    let _ = writeln!(file, "{}", std::process::id());
    let _ = file.flush();
    Some(InstanceLock { _file: file })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_acquire_in_the_same_process_group_fails_while_held() {
        let dir = std::env::temp_dir().join(format!("sigiltty-watcher-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("watcher.lock");

        let first = acquire(&path);
        assert!(first.is_some());
        // flock is per open-file-description: a second open in this same
        // process must NOT get the lock while the first is held.
        assert!(acquire(&path).is_none());
        drop(first);
        assert!(acquire(&path).is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
