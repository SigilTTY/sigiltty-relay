//! Unix seconds → ISO-8601, rendered at the server's own UTC offset with that
//! offset written into the line (`2026-08-15T19:53:20+08:00`). The watcher
//! runs on the user's machine, so its log should read in the timezone that
//! machine is set to — but a bare local time is undecodable the moment it
//! leaves the box, and DST means even the same box changes its mind twice a
//! year. Carrying the offset costs six characters and removes the ambiguity
//! for good. An offset of zero collapses to `Z`, so a UTC server (most cloud
//! images) renders what the `*_utc` database columns do, character for
//! character, and what the relay logs minus the milliseconds it adds — the
//! relay can emit two lines inside one second, this watcher cannot.
//!
//! Hand-rolled rather than pulling in `chrono`/`time`: this binary ships as a
//! statically linked musl artifact built for size (`opt-level = "z"`, LTO,
//! stripped). The calendar is one well-known algorithm, and the zone lookup
//! is libc's, which is already a dependency.

/// The server's UTC offset in seconds at `unix`, straight from libc — so it
/// follows `TZ` and `/etc/localtime` including DST transitions, without a
/// bundled tzdata. Zero when the zone cannot be resolved (a container with no
/// `/etc/localtime`), which renders as `Z`: honest rather than wrong.
pub fn local_offset(unix: u64) -> i64 {
    // No explicit tzset(): glibc and musl both resolve the zone on the first
    // localtime_r call and cache it, and the watcher's TZ cannot change under
    // it — the app restarts the process on any config change.
    let t = unix as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: localtime_r writes into our own `tm` and takes `t` by pointer;
    // both live for the call. It is the reentrant form, so no shared state.
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        0
    } else {
        tm.tm_gmtoff as i64
    }
}

/// Formats at the server's current offset — what every log line uses.
pub fn iso8601_local(unix: u64) -> String {
    iso8601(unix, local_offset(unix))
}

/// Formats at an explicit offset. Pure, so the calendar can be tested without
/// depending on the machine's zone.
pub fn iso8601(unix: u64, offset_seconds: i64) -> String {
    let local = unix as i64 + offset_seconds;
    // Euclidean, not truncating: a negative offset near the epoch pushes the
    // instant before 1970, where `/` and `%` would round toward zero and land
    // a day out.
    let (year, month, day) = civil_from_days(local.div_euclid(86_400));
    let secs = local.rem_euclid(86_400);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}{}",
        secs / 3600,
        (secs / 60) % 60,
        secs % 60,
        offset_suffix(offset_seconds),
    )
}

fn offset_suffix(offset_seconds: i64) -> String {
    if offset_seconds == 0 {
        return "Z".to_string();
    }
    let sign = if offset_seconds < 0 { '-' } else { '+' };
    let minutes = offset_seconds.abs() / 60;
    format!("{sign}{:02}:{:02}", minutes / 60, minutes % 60)
}

/// Howard Hinnant's `civil_from_days`: shifts to a March-based year so the
/// leap day falls at the end of it, which makes the month/day split pure
/// integer arithmetic with no table and no special-casing of February.
/// Correct across century rules (1900 and 2100 common, 2000 leap).
fn civil_from_days(days: i64) -> (i64, u64, u64) {
    let z = days + 719_468; // shift epoch from 1970-01-01 to 0000-03-01
    let era = z.div_euclid(146_097); // 400-year cycle: 146097 days exactly
    let doe = z.rem_euclid(146_097); // day of era, [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day of (March-based) year
    let mp = (5 * doy + 2) / 153; // March-based month, [0, 11]
    let day = (doy - (153 * mp + 2) / 5 + 1) as u64; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u64; // back to [1, 12]
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TS: u64 = 1_755_230_000; // 2025-08-15T03:53:20Z

    #[test]
    fn zero_offset_renders_the_same_string_the_relay_does() {
        // Same instant as relay/test/timestamps.test.ts — the cross-component
        // pin. On a UTC server this must be byte-identical to the database's
        // *_utc columns, and to the relay's log line up to its milliseconds.
        assert_eq!(iso8601(TS, 0), "2025-08-15T03:53:20Z");
        assert_eq!(iso8601(0, 0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn offsets_shift_the_clock_and_are_written_into_the_line() {
        assert_eq!(iso8601(TS, 8 * 3600), "2025-08-15T11:53:20+08:00");
        // Half-hour and quarter-hour zones exist (India, Nepal) — the offset
        // is minutes, not hours.
        assert_eq!(iso8601(TS, 5 * 3600 + 1800), "2025-08-15T09:23:20+05:30");
        assert_eq!(iso8601(TS, 5 * 3600 + 2700), "2025-08-15T09:38:20+05:45");
        // West of Greenwich the local date can be the previous day.
        assert_eq!(iso8601(TS, -7 * 3600), "2025-08-14T20:53:20-07:00");
        // Negative offset before the epoch: the euclidean-division case.
        assert_eq!(iso8601(0, -3600), "1969-12-31T23:00:00-01:00");
    }

    #[test]
    fn handles_the_boundaries_a_hand_rolled_calendar_gets_wrong() {
        // Last second before a year rolls over.
        assert_eq!(iso8601(946_684_799, 0), "1999-12-31T23:59:59Z");
        // Leap day in an ordinary leap year, and in the 400-year exception.
        assert_eq!(iso8601(1_709_164_800, 0), "2024-02-29T00:00:00Z");
        assert_eq!(iso8601(951_782_400, 0), "2000-02-29T00:00:00Z");
        // 2100 is NOT a leap year — the rule a naive /4 gets wrong.
        assert_eq!(iso8601(4_107_542_400, 0), "2100-03-01T00:00:00Z");
        // Past the signed 32-bit wrap; the watcher's clock is u64 throughout.
        assert_eq!(iso8601(2_147_483_648, 0), "2038-01-19T03:14:08Z");
    }

    #[test]
    fn every_day_of_a_leap_year_round_trips() {
        // Walks 1999-01-01 through 2001-01-01 a day at a time and checks the
        // rendered date advances by exactly one civil day each step — cheap
        // proof that no month length is off by one.
        let mut expected = (1999, 1, 1);
        let mut unix = 915_148_800; // 1999-01-01T00:00:00Z
        for _ in 0..731 {
            let (y, m, d) = expected;
            assert_eq!(iso8601(unix, 0), format!("{y:04}-{m:02}-{d:02}T00:00:00Z"));
            expected = next_day(expected);
            unix += 86_400;
        }
    }

    #[test]
    fn the_machines_own_zone_resolves_to_something_renderable() {
        // Whatever this machine is set to, the offset must be a real one and
        // the line must come out the right shape. Asserting the value would
        // just assert the test runner's TZ.
        let offset = local_offset(TS);
        assert!(offset.abs() <= 18 * 3600, "implausible offset: {offset}");
        let rendered = iso8601_local(TS);
        assert_eq!(rendered.len(), if offset == 0 { 20 } else { 25 });
        assert!(rendered.starts_with("2025-08-1"), "unexpected date: {rendered}");
    }

    fn next_day((y, m, d): (u64, u64, u64)) -> (u64, u64, u64) {
        let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
        let len = match m {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            _ if leap => 29,
            _ => 28,
        };
        match (m, d) {
            (12, 31) => (y + 1, 1, 1),
            _ if d == len => (y, m + 1, 1),
            _ => (y, m, d + 1),
        }
    }
}
