//! The per-target watch loop (docs/PROTOCOL.md §9): level-triggered
//! `agent wait` arming, the 10-second HYSTERESIS window — herdr's status
//! detector is known to flap, and this window is the fix's designed home
//! (the app does no client-side filtering; four device-verified failures
//! stand behind that) — and the awareness model deciding what reports.
//!
//! What reports: a STABLE transition the rule in `herdr::report_status`
//! admits — into blocked or done from anywhere, plus the working → idle
//! finish herdr believes was already seen — where "stable" means it survived
//! the hysteresis window. The window is the entire flap defense — so no
//! further dedup sits on top of it, because the most common real pattern is
//! blocked → (answered) → working → blocked again, and a "same status as last
//! time" filter swallows exactly that. Two silences remain: the run's very
//! first observation (the seed — at deploy time the screen was in front of
//! the user), and a pane with no agent, whose later appearance in a
//! reportable state does report.
//!
//! The remote exec is injected, so the whole loop is scripted-unit-tested;
//! only the process-spawning Remote at the bottom needs a live herdr.

use crate::herdr::{departure_set, report_status, AgentInfo, AgentStatus, WaitOutcome};
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
    /// A stable transition `herdr::report_status` admits. The AgentInfo
    /// carries the status to put ON THE WIRE, which is not always the one
    /// herdr called it (a seen finish travels as `done`).
    pub report: &'a mut dyn FnMut(&AgentInfo),
    pub should_continue: &'a dyn Fn() -> bool,
    pub sleep: &'a dyn Fn(Duration),
    pub log: &'a dyn Fn(&str),
}

pub fn run(remote: &dyn Remote, timing: &Timing, hooks: &mut WatchHooks) {
    // The last STABLE state; None = no agent on the pane (or not yet
    // observed). Transitions are measured against this, never against what
    // was last reported (see the module header).
    let mut current: Option<AgentStatus> = None;
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
                // Heartbeat: one quiet period, re-arm unchanged. Logged
                // because it is the only proof the loop is still turning —
                // "no news" would otherwise be indistinguishable from a dead
                // watcher, and the watcher reports nothing about itself.
                failures = 0;
                (hooks.log)(&format!(
                    "heartbeat (state: {})",
                    current.map(|s| s.as_str()).unwrap_or("no agent")));
            }
            WaitOutcome::Failure(reason) => {
                if !backoff_or_give_up(&reason, &mut failures, timing, hooks) {
                    return;
                }
            }
            WaitOutcome::Missing => {
                failures = 0;
                (hooks.log)("no agent on this pane");
                apply_missing(&mut current, &mut seeded);
                (hooks.sleep)(missing_backoff);
                missing_backoff = (missing_backoff * 2).min(timing.missing_cap);
            }
            WaitOutcome::Status(first) => {
                failures = 0;
                missing_backoff = timing.missing_start;
                match settle(remote, first, timing, hooks) {
                    Settled::Stable(agent) => {
                        let status = agent.status;
                        let previous = current;
                        current = Some(status);
                        if !seeded {
                            // The run's first observation seeds silently: at
                            // deploy time the screen was in front of the user.
                            seeded = true;
                            (hooks.log)(&format!("armed at {}", status.as_str()));
                            continue;
                        }
                        if previous == Some(status) {
                            // Settled back where it started — a flap that
                            // outlived one window but changed nothing.
                            (hooks.log)(&format!("settled back at {}", status.as_str()));
                            continue;
                        }
                        let from = previous.map(|s| s.as_str()).unwrap_or("none");
                        match report_status(previous, status) {
                            Some(reported) => {
                                // Says "as done" only when the wire status
                                // differs from what herdr called it — the
                                // working → idle finish, whose report would
                                // otherwise look like a logging bug.
                                let as_ = if reported == status {
                                    String::new()
                                } else {
                                    format!(" as {}", reported.as_str())
                                };
                                (hooks.log)(&format!("{from} -> {}: reporting{as_}", status.as_str()));
                                (hooks.report)(&AgentInfo { status: reported, ..agent });
                            }
                            None => (hooks.log)(&format!("{from} -> {}", status.as_str())),
                        }
                    }
                    Settled::Missing => {
                        (hooks.log)("no agent on this pane");
                        apply_missing(&mut current, &mut seeded);
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

/// Missing itself never reports, but it does count as an observation: the
/// pane's next agent is a new incarnation, so its appearance in a reportable
/// state IS an event (a hand-started agent blocking on its startup prompt).
fn apply_missing(current: &mut Option<AgentStatus>, seeded: &mut bool) {
    *seeded = true;
    *current = None;
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

    /// The most common real pattern, and the one an over-eager dedup eats:
    /// the user answers the agent's question, it works for a while, then it
    /// asks again. That second blocked is a NEW question and must report,
    /// even though the run was seeded blocked.
    #[test]
    fn reblock_after_a_working_episode_reports() {
        let (reported, _) = run_script(vec![
            agent(Blocked), WaitOutcome::TimedOut,  // seed: blocked, silent
            agent(Working), WaitOutcome::TimedOut,  // answered; nothing to report
            agent(Blocked), WaitOutcome::TimedOut,  // asks again → report
        ]);
        assert_eq!(reported, vec![Blocked]);
    }

    /// The flap defense lives in the hysteresis window, not in a
    /// same-status filter: a candidate that wanders and comes back reports
    /// nothing, while a settled round trip does.
    #[test]
    fn a_state_that_settles_back_where_it_started_is_silent() {
        let (reported, _) = run_script(vec![
            agent(Blocked), WaitOutcome::TimedOut, // seed: blocked
            agent(Done),                            // wanders…
            agent(Blocked), WaitOutcome::TimedOut,  // …and settles back at blocked
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

    /// A finish herdr classified as already seen (`working → idle`, because a
    /// TUI was open on the server) still reaches the phone — as `done`, since
    /// that is what the wire and the NSE understand.
    #[test]
    fn a_seen_finish_still_reports_and_travels_as_done() {
        let (reported, _) = run_script(vec![
            agent(Working), WaitOutcome::TimedOut,  // seed: working
            agent(Idle), WaitOutcome::TimedOut,     // finished, counted seen
        ]);
        assert_eq!(reported, vec![Done]);
    }

    /// The other two idle edges, and the reason the rule reads transitions
    /// rather than statuses: the user opening the pane must never push, and
    /// an answered question with no work after it is not a finish.
    #[test]
    fn the_other_idle_edges_stay_silent() {
        let (after_done, _) = run_script(vec![
            agent(Working), WaitOutcome::TimedOut,  // seed
            agent(Done), WaitOutcome::TimedOut,     // finished, unseen → reports
            agent(Idle), WaitOutcome::TimedOut,     // the user looked at it
        ]);
        assert_eq!(after_done, vec![Done]);

        let (after_blocked, _) = run_script(vec![
            agent(Working), WaitOutcome::TimedOut,  // seed
            agent(Blocked), WaitOutcome::TimedOut,  // asks → reports
            agent(Idle), WaitOutcome::TimedOut,     // answered at the server
        ]);
        assert_eq!(after_blocked, vec![Blocked]);
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
