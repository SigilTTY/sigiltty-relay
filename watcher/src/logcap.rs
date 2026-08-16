//! Keeping the log under a size cap (docs/PROTOCOL.md §9).
//!
//! The watcher does not open its log: the bootstrap redirects our stderr at
//! `$XDG_DATA_HOME/sigiltty/watcher.log` and truncates it per run. That is
//! enough while the app reconnects often, but a single run may last the full
//! seven-day TTL, and a herdr that flaps for days writes a line every settle
//! window — so the run's own output needs a ceiling that does not depend on
//! anyone reconnecting.
//!
//! Trimming keeps the TAIL, not the head: the lines worth having when
//! something goes wrong are the most recent ones. It rewrites the file IN
//! PLACE and moves our own file offset to match, because the obvious
//! alternatives both break the running process — renaming the file leaves our
//! stderr pointing at an unlinked inode (every later line vanishes), and
//! truncating without moving the offset makes the kernel pad the gap back to
//! the old position with zero bytes, so the "trimmed" file is instantly as
//! large as before, only now full of NULs.
//!
//! Two guards on whose file we are allowed to touch: it must be a REGULAR
//! file (a terminal or a pipe has no size to cap — that is the developer
//! running the watcher by hand), and its device+inode must be the ones the
//! path resolves to. If someone redirected our stderr somewhere else, that
//! file belongs to them and this does nothing.

use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::MetadataExt;
use std::os::unix::io::RawFd;
use std::path::Path;

/// The ceiling. Reached, the log becomes its own second half, so on-disk size
/// swings between 2.5 MB and 5 MB and never exceeds the cap by more than one
/// supervisor tick of writing (kilobytes, even when herdr flaps).
pub const CAP_BYTES: u64 = 5 * 1024 * 1024;

/// Returns a line to log when it trimmed, `None` when it left the file alone.
///
/// Racy by construction: the watch threads write to this same fd while we
/// rewrite it, so a line landing in the microseconds between reading the tail
/// and resetting the offset can be lost. That is the right trade — the
/// alternative is a lock on every log line to protect an event that happens
/// once per gigabyte of flapping.
pub fn enforce(fd: RawFd, path: &Path, cap: u64) -> Option<String> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut st) } != 0 {
        return None;
    }
    if (st.st_mode as u32) & (libc::S_IFMT as u32) != (libc::S_IFREG as u32) {
        return None;
    }
    let size = st.st_size as u64;
    if size <= cap {
        return None;
    }
    // Our stderr must BE this path, not merely resemble it.
    let meta = std::fs::metadata(path).ok()?;
    if meta.dev() != st.st_dev as u64 || meta.ino() != st.st_ino as u64 {
        return None;
    }

    let keep = cap / 2;
    let mut tail = Vec::with_capacity(keep as usize + 1);
    {
        let mut file = std::fs::File::open(path).ok()?;
        file.seek(SeekFrom::Start(size - keep)).ok()?;
        file.read_to_end(&mut tail).ok()?;
    }
    // Land on a line boundary: the cut almost always falls mid-line, and half
    // a timestamp at the top of a log reads as corruption.
    if let Some(nl) = tail.iter().position(|b| *b == b'\n') {
        tail.drain(..=nl);
    }

    let mut file = std::fs::OpenOptions::new().write(true).open(path).ok()?;
    file.write_all(&tail).ok()?;
    file.set_len(tail.len() as u64).ok()?;
    // Continue writing where the kept tail ends. Harmless if stderr happens
    // to be O_APPEND (then the kernel ignores the offset anyway).
    unsafe { libc::lseek(fd, tail.len() as libc::off_t, libc::SEEK_SET) };
    Some(format!("log reached {size} bytes, kept the last {}", tail.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::io::AsRawFd;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("sigiltty-logcap-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("watcher.log")
    }

    /// Writes `lines` numbered lines through a handle that stays open, the
    /// way the running watcher holds its stderr.
    fn writer(path: &Path) -> std::fs::File {
        std::fs::OpenOptions::new().create(true).write(true).truncate(true).open(path).unwrap()
    }

    #[test]
    fn an_oversized_log_becomes_its_own_tail() {
        let path = scratch("oversized");
        let mut out = writer(&path);
        for i in 0..1000 {
            writeln!(out, "[ts] line {i:04} ------------------------------").unwrap();
        }
        let before = std::fs::metadata(&path).unwrap().len();
        let cap = before / 2;

        let note = enforce(out.as_raw_fd(), &path, cap).expect("should trim");
        assert!(note.starts_with(&format!("log reached {before} bytes")), "{note}");

        let kept = std::fs::read_to_string(&path).unwrap();
        assert!((kept.len() as u64) <= cap);
        // The tail survived, the head did not, and the first line is whole.
        assert!(kept.ends_with("line 0999 ------------------------------\n"));
        assert!(!kept.contains("line 0000"));
        assert!(kept.starts_with("[ts] line "));
    }

    /// The offset reset is the part that is easy to get wrong: without it the
    /// next write lands at the old position and the kernel fills the gap with
    /// NULs, leaving the file as big as it was before the trim.
    #[test]
    fn writing_continues_after_the_tail_without_a_hole() {
        let path = scratch("continues");
        let mut out = writer(&path);
        for i in 0..1000 {
            writeln!(out, "[ts] line {i:04} ------------------------------").unwrap();
        }
        let cap = std::fs::metadata(&path).unwrap().len() / 2;
        enforce(out.as_raw_fd(), &path, cap).unwrap();
        let after_trim = std::fs::metadata(&path).unwrap().len();

        writeln!(out, "[ts] after the trim").unwrap();

        let grew = std::fs::metadata(&path).unwrap().len() - after_trim;
        assert_eq!(grew, "[ts] after the trim\n".len() as u64);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.ends_with("[ts] after the trim\n"));
        assert!(!text.contains('\0'));
    }

    #[test]
    fn a_log_under_the_cap_is_left_alone() {
        let path = scratch("under");
        let mut out = writer(&path);
        writeln!(out, "[ts] one short line").unwrap();
        assert_eq!(enforce(out.as_raw_fd(), &path, CAP_BYTES), None);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[ts] one short line\n");
    }

    /// Someone redirected our stderr elsewhere: the file at the expected path
    /// is not ours to rewrite, however big it is.
    #[test]
    fn a_stderr_pointing_somewhere_else_is_never_touched() {
        let path = scratch("elsewhere");
        let decoy = path.with_file_name("someone-elses.log");
        std::fs::write(&decoy, "not ours\n").unwrap();
        let mut out = writer(&path);
        for i in 0..200 {
            writeln!(out, "[ts] line {i:04}").unwrap();
        }
        // fd is the real log, but we claim the decoy is where it lives.
        assert_eq!(enforce(out.as_raw_fd(), &decoy, 16), None);
        assert_eq!(std::fs::read_to_string(&decoy).unwrap(), "not ours\n");
    }

    /// A developer running the watcher in a terminal: stderr is a tty (or a
    /// pipe under `| less`), which has no size and must not be `ftruncate`d.
    #[test]
    fn a_non_regular_stderr_has_no_size_to_cap() {
        let path = scratch("pipe");
        let mut fds = [0 as libc::c_int; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        assert_eq!(enforce(fds[1], &path, 0), None);
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
    }
}
