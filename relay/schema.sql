-- D1 schema for the SigilTTY relay (docs/PROTOCOL.md §3, §7).
-- Apply: wrangler d1 execute sigiltty-relay --file=schema.sql [--remote]

-- One row per anonymous device registration. secret_hash is SHA-256 hex of
-- the secret string as issued — the secret itself is never stored.
CREATE TABLE IF NOT EXISTS registrations (
  routing_id TEXT PRIMARY KEY,
  secret_hash TEXT NOT NULL,
  platform TEXT NOT NULL,
  token_kind TEXT NOT NULL,
  apns_environment TEXT NOT NULL,
  apns_token TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  last_seen_at INTEGER NOT NULL
);

-- Per-(routing, pane) collapse clock: 1 event / 60 s. Rows only advance on
-- a successful APNs send, so a failed attempt never blocks its own retry.
CREATE TABLE IF NOT EXISTS send_log (
  routing_id TEXT NOT NULL,
  collapse_key TEXT NOT NULL,
  sent_at INTEGER NOT NULL,
  PRIMARY KEY (routing_id, collapse_key)
);

-- Hourly counters, shared by the per-routing event cap and the per-IP
-- registration cap (keys prefixed "ip:").
CREATE TABLE IF NOT EXISTS hourly_counts (
  routing_id TEXT NOT NULL,
  hour_bucket INTEGER NOT NULL,
  count INTEGER NOT NULL,
  PRIMARY KEY (routing_id, hour_bucket)
);
