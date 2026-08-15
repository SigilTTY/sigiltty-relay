//! herdr CLI surface: `agent wait` argument building and noise-tolerant
//! output parsing. Mirrors the app's HerdrCLI semantics (herdr ≥ 0.8.0):
//! the wait is LEVEL-triggered — a target already in an `--until` status
//! answers immediately — so callers wait for DEPARTURE from the current
//! status. Flags are space-separated (`--flag=value` is not supported by
//! herdr's subparsers); `--session <name>` is a global flag.

use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStatus {
    Idle,
    Working,
    Blocked,
    Done,
    Unknown,
}

impl AgentStatus {
    /// Unknown herdr status values degrade to Unknown, never to an error —
    /// same posture as the app (forward compatibility with new statuses).
    pub fn parse(raw: &str) -> AgentStatus {
        match raw {
            "idle" => AgentStatus::Idle,
            "working" => AgentStatus::Working,
            "blocked" => AgentStatus::Blocked,
            "done" => AgentStatus::Done,
            _ => AgentStatus::Unknown,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            AgentStatus::Idle => "idle",
            AgentStatus::Working => "working",
            AgentStatus::Blocked => "blocked",
            AgentStatus::Done => "done",
            AgentStatus::Unknown => "unknown",
        }
    }

    /// Only →blocked and →done ever notify (PROTOCOL §9, app decision #4).
    pub fn notifiable(&self) -> bool {
        matches!(self, AgentStatus::Blocked | AgentStatus::Done)
    }
}

pub const ALL_STATUSES: [AgentStatus; 5] = [
    AgentStatus::Idle,
    AgentStatus::Working,
    AgentStatus::Blocked,
    AgentStatus::Done,
    AgentStatus::Unknown,
];

/// Departure set: all five statuses minus the current one; all five when
/// there is no current status (probe — answers instantly).
pub fn departure_set(current: Option<AgentStatus>) -> Vec<AgentStatus> {
    ALL_STATUSES
        .iter()
        .copied()
        .filter(|s| Some(*s) != current)
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentInfo {
    pub status: AgentStatus,
    pub name: Option<String>,
    pub agent_kind: Option<String>,
}

impl AgentInfo {
    /// Title label, mirroring the app's fallback chain exactly:
    /// user-given name > agent kind > "Agent".
    pub fn label(&self) -> String {
        self.name
            .clone()
            .or_else(|| self.agent_kind.clone())
            .unwrap_or_else(|| "Agent".into())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitOutcome {
    Status(AgentInfo),
    /// No agent on the pane right now (`agent_not_found` upfront, or
    /// `agent_not_running` when it vanished mid-wait).
    Missing,
    /// Remote `--timeout` fired: pure heartbeat, nothing changed.
    TimedOut,
    Failure(String),
}

pub fn wait_args(
    pane_id: &str,
    until: &[AgentStatus],
    timeout_ms: u64,
    session: Option<&str>,
) -> Vec<String> {
    let mut args = vec!["agent".into(), "wait".into(), pane_id.into()];
    for status in until {
        args.push("--until".into());
        args.push(status.as_str().into());
    }
    args.push("--timeout".into());
    args.push(timeout_ms.to_string());
    if let Some(session) = session {
        args.push("--session".into());
        args.push(session.into());
    }
    args
}

#[derive(Deserialize)]
struct WaitLine {
    result: Option<WaitResult>,
    error: Option<WaitError>,
}

#[derive(Deserialize)]
struct WaitResult {
    agent: Option<RawAgent>,
}

#[derive(Deserialize)]
struct RawAgent {
    agent_status: Option<String>,
    name: Option<String>,
    agent: Option<String>,
}

#[derive(Deserialize)]
struct WaitError {
    code: Option<String>,
}

/// Noise-tolerant, line-by-line (the app's decodeJSONLine posture): scan
/// combined stdout+stderr, first line that decodes into a wait result or
/// error wins. Anything else — including empty output — is a Failure.
pub fn parse_wait_output(combined: &str) -> WaitOutcome {
    for line in combined.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('{') {
            continue;
        }
        let Ok(decoded) = serde_json::from_str::<WaitLine>(trimmed) else {
            continue;
        };
        if let Some(agent) = decoded.result.and_then(|r| r.agent) {
            return WaitOutcome::Status(AgentInfo {
                status: AgentStatus::parse(agent.agent_status.as_deref().unwrap_or("")),
                name: agent.name,
                agent_kind: agent.agent,
            });
        }
        if let Some(code) = decoded.error.and_then(|e| e.code) {
            return match code.as_str() {
                "timeout" => WaitOutcome::TimedOut,
                "agent_not_found" | "agent_not_running" => WaitOutcome::Missing,
                other => WaitOutcome::Failure(format!("herdr error: {other}")),
            };
        }
    }
    WaitOutcome::Failure("no parseable wait output".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn departure_excludes_current_and_probe_is_all_five() {
        assert_eq!(departure_set(None).len(), 5);
        let from_blocked = departure_set(Some(AgentStatus::Blocked));
        assert_eq!(from_blocked.len(), 4);
        assert!(!from_blocked.contains(&AgentStatus::Blocked));
    }

    #[test]
    fn wait_args_shape() {
        let args = wait_args("w1:p4", &[AgentStatus::Blocked, AgentStatus::Done], 300_000, Some("dev"));
        assert_eq!(
            args,
            vec![
                "agent", "wait", "w1:p4",
                "--until", "blocked", "--until", "done",
                "--timeout", "300000",
                "--session", "dev",
            ]
        );
    }

    #[test]
    fn parses_agent_info_with_noise_around_it() {
        let combined = "some rc-file noise\n{\"id\":\"cli:agent:wait\",\"result\":{\"type\":\"agent_info\",\"agent\":{\"terminal_id\":\"t1\",\"agent_status\":\"blocked\",\"pane_id\":\"w1:p4\",\"name\":\"api-refactor\"}}}\n";
        match parse_wait_output(combined) {
            WaitOutcome::Status(agent) => {
                assert_eq!(agent.status, AgentStatus::Blocked);
                assert_eq!(agent.label(), "api-refactor");
            }
            other => panic!("expected status, got {other:?}"),
        }
    }

    #[test]
    fn classifies_errors() {
        let missing = r#"{"id":"cli:agent:wait","error":{"code":"agent_not_found","message":"no agent"}}"#;
        assert_eq!(parse_wait_output(missing), WaitOutcome::Missing);
        let gone = r#"{"error":{"code":"agent_not_running"}}"#;
        assert_eq!(parse_wait_output(gone), WaitOutcome::Missing);
        let timeout = r#"{"error":{"code":"timeout"}}"#;
        assert_eq!(parse_wait_output(timeout), WaitOutcome::TimedOut);
        assert!(matches!(parse_wait_output(""), WaitOutcome::Failure(_)));
        assert!(matches!(parse_wait_output("{\"error\":{\"code\":\"weird\"}}"), WaitOutcome::Failure(_)));
    }

    #[test]
    fn unknown_status_value_degrades_to_unknown() {
        let novel = r#"{"result":{"type":"agent_info","agent":{"agent_status":"pondering"}}}"#;
        match parse_wait_output(novel) {
            WaitOutcome::Status(agent) => {
                assert_eq!(agent.status, AgentStatus::Unknown);
                assert_eq!(agent.label(), "Agent");
            }
            other => panic!("expected status, got {other:?}"),
        }
    }
}
