//! The per-target watch loop (docs/PROTOCOL.md §9): level-triggered
//! `agent wait` arming, the 10-second HYSTERESIS window — herdr's status
//! detector is known to flap, and this window is the fix's designed home
//! (the app does no client-side filtering; four device-verified failures
//! stand behind that) — and the awareness model deciding what reports.
//!
//! Awareness model: `last_reported` is the status the user is presumed
//! aware of for this agent INCARNATION — the run's seed (screen was in
//! front of them at deploy time) or the last pushed status. A stable
//! →blocked/→done reports only when it differs from it; a stable missing
//! resets it (a restarted agent is a new incarnation, so its appearance
//! reports — the hand-started-agent-blocks-on-startup case).
//!
//! The remote exec is injected, so the whole loop is scripted-unit-tested;
//! only the process-spawning Remote at the bottom needs a live herdr.

use crate::herdr::{departure_set, AgentInfo, AgentStatus, WaitOutcome};
use std::time::Duration;

pub trait Remote {
    fn wait(&self, until: &[AgentStatus], timeout_ms: u64) -> WaitOutcome;
}

pub struct Timing {
    /// Hysteresis: a new status must hold this long before it counts.
    pub settle_ms: u64,
    /// Armed-round heartbeat (the remote --timeout, also orphan reaper).
    pub wait_timeout_ms: u64,
    /// Waiting-state probe cadence when the pane hosts no agent.
    pub missing_start: Duration,
    pub missing_cap: Duration,
    /// Bounded mechanism-failure retries; exhausting them ends the watch.
    pub failure_backoffs: Vec<Duration>,
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            settle_ms: 10_000,
            wait_timeout_ms: 300_000,
            missing_start: Duration::from_secs(30),
            missing_cap: Duration::from_secs(240),
            failure_backoffs: vec![
                Duration::from_secs(5),
                Duration::from_secs(15),
                Duration::from_secs(45),
            ],
        }
    }
}

pub struct WatchHooks<'a> {
    /// A stable, notifiable, not-yet-known-to-the-user transition.
    pub report: &'a mut dyn FnMut(&AgentInfo),
    pub should_continue: &'a dyn Fn() -> bool,
    pub sleep: &'a dyn Fn(Duration),
    pub log: &'a dyn Fn(&str),
}

pub fn run(remote: &dyn Remote, timing: &Timing, hooks: &mut WatchHooks) {
    // Last STABLE state; None = no agent on the pane (or not yet observed).
    let mut current: Option<AgentStatus> = None;
    // See the awareness model in the module header.
    let mut last_reported: Option<AgentStatus> = None;
    let mut seeded = false;
    let mut missing_backoff = timing.missing_start;
    let mut failures: usize = 0;

    while (hooks.should_continue)() {
        let armed = remote.wait(&departure_set(current), timing.wait_timeout_ms);
        if !(hooks.should_continue)() {
            return;
        }
        match armed {
            WaitOutcome::TimedOut => {
                // Heartbeat: one quiet period, re-arm unchanged.
                failures = 0;
            }
            WaitOutcome::Failure(reason) => {
                if !backoff_or_give_up(&reason, &mut failures, timing, hooks) {
                    return;
                }
            }
            WaitOutcome::Missing => {
                failures = 0;
                apply_missing(&mut current, &mut last_reported, &mut seeded);
                (hooks.sleep)(missing_backoff);
                missing_backoff = (missing_backoff * 2).min(timing.missing_cap);
            }
            WaitOutcome::Status(first) => {
                failures = 0;
                missing_backoff = timing.missing_start;
                match settle(remote, first, timing, hooks) {
                    Settled::Stable(agent) => {
                        let status = agent.status;
                        if !seeded {
                            // The run's first observation seeds silently.
                            seeded = true;
                            current = Some(status);
                            last_reported = Some(status);
                            continue;
                        }
                        current = Some(status);
                        if status.notifiable() && last_reported != Some(status) {
                            last_reported = Some(status);
                            (hooks.report)(&agent);
                        }
                    }
                    Settled::Missing => {
                        apply_missing(&mut current, &mut last_reported, &mut seeded);
                        (hooks.sleep)(missing_backoff);
                        missing_backoff = (missing_backoff * 2).min(timing.missing_cap);
                    }
                    Settled::Failed(reason) => {
                        if !backoff_or_give_up(&reason, &mut failures, timing, hooks) {
                            return;
                        }
                    }
                    Settled::Stopped => return,
                }
            }
        }
    }
}

enum Settled {
    Stable(AgentInfo),
    Missing,
    Failed(String),
    Stopped,
}

/// The hysteresis window: keep confirm-waiting for departure from the
/// candidate with the settle timeout; every change restarts the window;
/// only a full quiet window (TimedOut) makes the candidate stable. Bounces
/// shorter than the window vanish here, unreported.
fn settle(
    remote: &dyn Remote,
    first: AgentInfo,
    timing: &Timing,
    hooks: &WatchHooks,
) -> Settled {
    let mut candidate = first;
    loop {
        if !(hooks.should_continue)() {
            return Settled::Stopped;
        }
        match remote.wait(&departure_set(Some(candidate.status)), timing.settle_ms) {
            WaitOutcome::TimedOut => return Settled::Stable(candidate),
            WaitOutcome::Status(next) => candidate = next,
            WaitOutcome::Missing => return Settled::Missing,
            WaitOutcome::Failure(reason) => return Settled::Failed(reason),
        }
    }
}

fn apply_missing(
    current: &mut Option<AgentStatus>,
    last_reported: &mut Option<AgentStatus>,
    seeded: &mut bool,
) {
    *seeded = true;
    *current = None;
    // Awareness reset: the incarnation is over — a later appearance in a
    // notifiable state must report. Missing itself never does.
    *last_reported = None;
}

/// True = keep going (slept a backoff); false = retries exhausted, the
/// watch is over (silent — recovery is the app's per-connection health
/// check; the watcher never notifies about itself).
fn backoff_or_give_up(
    reason: &str,
    failures: &mut usize,
    timing: &Timing,
    hooks: &WatchHooks,
) -> bool {
    if *failures >= timing.failure_backoffs.len() {
        (hooks.log)(&format!("watch gave up: {reason}"));
        return false;
    }
    (hooks.log)(&format!("watch retrying: {reason}"));
    let backoff = timing.failure_backoffs[*failures];
    *failures += 1;
    (hooks.sleep)(backoff);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct Script {
        responses: RefCell<Vec<WaitOutcome>>,
        calls: RefCell<Vec<(Vec<AgentStatus>, u64)>>,
    }

    impl Script {
        fn new(responses: Vec<WaitOutcome>) -> Script {
            Script {
                responses: RefCell::new(responses),
                calls: RefCell::new(Vec::new()),
            }
        }
    }

    impl Remote for Script {
        fn wait(&self, until: &[AgentStatus], timeout_ms: u64) -> WaitOutcome {
            self.calls.borrow_mut().push((until.to_vec(), timeout_ms));
            let mut responses = self.responses.borrow_mut();
            if responses.is_empty() {
                // Script exhausted → mechanism failure → the bounded
                // failure path ends the run deterministically.
                WaitOutcome::Failure("script exhausted".into())
            } else {
                responses.remove(0)
            }
        }
    }

    fn agent(status: AgentStatus) -> WaitOutcome {
        WaitOutcome::Status(AgentInfo { status, name: Some("api-refactor".into()), agent_kind: Some("claude".into()) })
    }

    fn fast_timing() -> Timing {
        Timing {
            settle_ms: 10,
            wait_timeout_ms: 300,
            missing_start: Duration::from_millis(1),
            missing_cap: Duration::from_millis(2),
            failure_backoffs: vec![Duration::ZERO, Duration::ZERO, Duration::ZERO],
        }
    }

    /// Runs a script to its exhaustion-driven end; returns reported
    /// statuses and the recorded wait calls.
    fn run_script(responses: Vec<WaitOutcome>) -> (Vec<AgentStatus>, Vec<(Vec<AgentStatus>, u64)>) {
        let script = Script::new(responses);
        let reported = RefCell::new(Vec::new());
        let mut report = |info: &AgentInfo| reported.borrow_mut().push(info.status);
        let mut hooks = WatchHooks {
            report: &mut report,
            should_continue: &|| true,
            sleep: &|_| {},
            log: &|_| {},
        };
        run(&script, &fast_timing(), &mut hooks);
        (reported.into_inner(), script.calls.into_inner())
    }

    use AgentStatus::*;

    #[test]
    fn seed_settles_but_never_reports_and_real_transition_does() {
        let (reported, calls) = run_script(vec![
            agent(Blocked), WaitOutcome::TimedOut, // seed: blocked, stable — silent
            agent(Done), WaitOutcome::TimedOut,    // real transition — reports
        ]);
        assert_eq!(reported, vec![Done]);
        // First armed call probes all five; the settle round uses the
        // hysteresis timeout and the candidate's departure set.
        assert_eq!(calls[0].0.len(), 5);
        assert_eq!(calls[1].1, 10);
        assert!(!calls[1].0.contains(&Blocked));
        // Re-arm after stable blocked excludes blocked, full heartbeat.
        assert_eq!(calls[2].1, 300);
        assert!(!calls[2].0.contains(&Blocked));
    }

    #[test]
    fn bounce_inside_the_window_vanishes() {
        let (reported, _) = run_script(vec![
            agent(Working), WaitOutcome::TimedOut,  // seed: working
            agent(Blocked),                          // flap up…
            agent(Working), WaitOutcome::TimedOut,   // …and back within the window
        ]);
        // Stable state equals where we started — nothing to report.
        assert_eq!(reported, Vec::<AgentStatus>::new());
    }

    #[test]
    fn reblock_after_a_working_episode_is_suppressed_by_awareness() {
        let (reported, _) = run_script(vec![
            agent(Blocked), WaitOutcome::TimedOut,  // seed: blocked (user saw it)
            agent(Working), WaitOutcome::TimedOut,  // silent (not notifiable)
            agent(Blocked), WaitOutcome::TimedOut,  // same status the user knows
        ]);
        assert_eq!(reported, Vec::<AgentStatus>::new());
    }

    #[test]
    fn slow_flap_reports_each_side_at_most_alternately() {
        let (reported, _) = run_script(vec![
            agent(Working), WaitOutcome::TimedOut, // seed
            agent(Blocked), WaitOutcome::TimedOut,
            agent(Done), WaitOutcome::TimedOut,
            agent(Blocked), WaitOutcome::TimedOut,
        ]);
        // Alternating stable states each differ from the last reported one:
        // the relay's per-pane 60 s clock is the second (final) layer.
        assert_eq!(reported, vec![Blocked, Done, Blocked]);
    }

    #[test]
    fn appearance_after_missing_seed_reports() {
        let (reported, _) = run_script(vec![
            WaitOutcome::Missing,                   // seed: no agent — silent
            agent(Blocked), WaitOutcome::TimedOut,  // hand-started agent blocks on startup
        ]);
        assert_eq!(reported, vec![Blocked]);
    }

    #[test]
    fn vanish_resets_awareness_so_the_restart_reports() {
        let (reported, _) = run_script(vec![
            agent(Blocked), WaitOutcome::TimedOut, // seed: blocked
            WaitOutcome::Missing,                   // agent released — silent
            agent(Blocked), WaitOutcome::TimedOut,  // new incarnation blocks again
        ]);
        assert_eq!(reported, vec![Blocked]);
    }

    #[test]
    fn vanish_during_settle_is_missing_and_silent() {
        let (reported, _) = run_script(vec![
            agent(Working), WaitOutcome::TimedOut,  // seed
            agent(Blocked), WaitOutcome::Missing,   // vanished inside the window
        ]);
        assert_eq!(reported, Vec::<AgentStatus>::new());
    }

    #[test]
    fn heartbeat_rearms_with_the_same_departure_set() {
        let (_, calls) = run_script(vec![
            agent(Idle), WaitOutcome::TimedOut,     // seed: idle
            WaitOutcome::TimedOut,                   // quiet heartbeat
        ]);
        assert_eq!(calls[2].0, calls[3].0);
        assert_eq!(calls[3].1, 300);
    }

    #[test]
    fn failures_retry_boundedly_then_give_up() {
        let (reported, calls) = run_script(vec![]);
        assert_eq!(reported, Vec::<AgentStatus>::new());
        // Initial call + three backoff retries, then the terminal give-up.
        assert_eq!(calls.len(), 4);
    }
}
