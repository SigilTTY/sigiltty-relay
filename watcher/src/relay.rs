//! Relay client (docs/PROTOCOL.md §3.3): one agent event fans out as one
//! entry per device, sealed to that device's key. `sendFailed` entries are
//! retried on a short schedule that stays inside the ±300 s replay window;
//! everything else is final for this event (rate limits are the relay's
//! job; dead routings are the app's next health check's job).

use crate::config::{RelayConfig, Target};
use crate::herdr::AgentInfo;
use crate::seal;
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Serialize, Clone)]
struct EventEntry {
    #[serde(rename = "routingID")]
    routing_id: String,
    secret: String,
    #[serde(rename = "collapseKey")]
    collapse_key: String,
    ts: u64,
    ciphertext: String,
}

#[derive(Deserialize)]
struct EventsResponse {
    results: Vec<String>,
}

/// Encrypted payload plaintext (PROTOCOL §5).
#[derive(Serialize)]
struct EventPayload<'a> {
    v: u32,
    #[serde(rename = "serverID")]
    server_id: &'a str,
    #[serde(rename = "serverName")]
    server_name: &'a str,
    #[serde(rename = "paneID")]
    pane_id: &'a str,
    #[serde(rename = "herdrSession")]
    herdr_session: &'a Option<String>,
    #[serde(rename = "agentLabel")]
    agent_label: String,
    #[serde(rename = "paneLabel")]
    pane_label: &'a Option<String>,
    status: &'a str,
    ts: u64,
}

pub fn collapse_key(server_id: &str, pane_id: &str) -> String {
    format!("herdr-{server_id}-{pane_id}")
}

const RETRY_DELAYS: [Duration; 2] = [Duration::from_secs(30), Duration::from_secs(120)];

pub struct RelayClient {
    events_url: String,
}

impl RelayClient {
    pub fn new(relay_url: &str) -> RelayClient {
        RelayClient {
            events_url: format!("{}/v1/events", relay_url.trim_end_matches('/')),
        }
    }

    /// Seals and posts one agent event to every device in the config.
    /// Blocking (runs on the target's watch thread — delivery outranks the
    /// next observation). Failures beyond the retry schedule are dropped
    /// silently: the watcher never notifies about itself.
    pub fn report(
        &self,
        config: &RelayConfig,
        target: &Target,
        agent: &AgentInfo,
        now: u64,
        sleep: &dyn Fn(Duration),
        log: &dyn Fn(&str),
    ) {
        let key = collapse_key(&config.server_id, &target.pane_id);
        let payload = EventPayload {
            v: 1,
            server_id: &config.server_id,
            server_name: &config.server_name,
            pane_id: &target.pane_id,
            herdr_session: &target.herdr_session,
            agent_label: agent.label(),
            pane_label: &target.label,
            status: agent.status.as_str(),
            ts: now,
        };
        let Ok(plaintext) = serde_json::to_vec(&payload) else {
            return;
        };

        let mut entries: Vec<EventEntry> = Vec::new();
        for device in &config.devices {
            match seal::seal(&device.public_key, key.as_bytes(), &plaintext) {
                Ok(ciphertext) => entries.push(EventEntry {
                    routing_id: device.routing_id.clone(),
                    secret: device.secret.clone(),
                    collapse_key: key.clone(),
                    ts: now,
                    ciphertext,
                }),
                Err(e) => log(&format!("seal for {}: {e}", device.routing_id)),
            }
        }

        let mut pending = entries;
        for attempt in 0..=RETRY_DELAYS.len() {
            if pending.is_empty() {
                return;
            }
            match self.post(&pending) {
                Ok(results) => {
                    let mut retry = Vec::new();
                    for (entry, result) in pending.iter().zip(results.iter()) {
                        match result.as_str() {
                            "sent" | "rateLimited" => {}
                            "sendFailed" => retry.push(entry.clone()),
                            other => log(&format!("relay dropped {}: {other}", entry.routing_id)),
                        }
                    }
                    pending = retry;
                }
                Err(e) => log(&format!("relay post: {e}")),
                // Transport error: retry the whole batch on the schedule.
            }
            if pending.is_empty() || attempt == RETRY_DELAYS.len() {
                return;
            }
            sleep(RETRY_DELAYS[attempt]);
        }
    }

    fn post(&self, entries: &[EventEntry]) -> Result<Vec<String>, String> {
        let body = serde_json::json!({ "events": entries });
        let response = ureq::post(&self.events_url)
            .timeout(Duration::from_secs(30))
            .send_json(body)
            .map_err(|e| e.to_string())?;
        let decoded: EventsResponse = response.into_json().map_err(|e| e.to_string())?;
        if decoded.results.len() != entries.len() {
            return Err("result count mismatch".into());
        }
        Ok(decoded.results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapse_key_matches_the_app_side_contract() {
        // PROTOCOL §8: this exact string doubles as the app's local
        // notification identifier — change it there first or not at all.
        assert_eq!(
            collapse_key("5E1C0000-0000-0000-0000-00000000D2A9", "w1:p4"),
            "herdr-5E1C0000-0000-0000-0000-00000000D2A9-w1:p4"
        );
    }
}
