-- D1 schema for the SigilTTY relay (docs/PROTOCOL.md §3, §7).
-- Apply: wrangler d1 execute sigiltty-relay --file=schema.sql [--remote]
--
-- Times are stored as unix seconds (INTEGER) and that stays the source of
-- truth: every read of one is a comparison, a range delete or a bucket key,
-- and the RelayStore seam hands the app plain numbers. Each is mirrored by a
-- VIRTUAL generated `*_utc` column purely so `SELECT *` is legible to a
-- human. VIRTUAL costs no storage and is computed on read, so the mirror can
-- never disagree with the integer beside it. The rendering matches
-- src/log.ts `isoFromUnix`, which is what makes a log line and a row here
-- comparable by eye.
--
-- `CREATE TABLE IF NOT EXISTS` never alters an existing table, so a database
-- created before these columns existed needs
-- upgrades/0001_readable_timestamps.sql once.

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
  last_seen_at INTEGER NOT NULL,
  created_at_utc TEXT GENERATED ALWAYS AS
    (strftime('%Y-%m-%dT%H:%M:%SZ', created_at, 'unixepoch')) VIRTUAL,
  last_seen_at_utc TEXT GENERATED ALWAYS AS
    (strftime('%Y-%m-%dT%H:%M:%SZ', last_seen_at, 'unixepoch')) VIRTUAL
);

-- Per-(routing, pane) collapse clock: 1 event / 60 s. Rows only advance on
-- a successful APNs send, so a failed attempt never blocks its own retry.
CREATE TABLE IF NOT EXISTS send_log (
  routing_id TEXT NOT NULL,
  collapse_key TEXT NOT NULL,
  sent_at INTEGER NOT NULL,
  sent_at_utc TEXT GENERATED ALWAYS AS
    (strftime('%Y-%m-%dT%H:%M:%SZ', sent_at, 'unixepoch')) VIRTUAL,
  PRIMARY KEY (routing_id, collapse_key)
);

-- Hourly counters, shared by the per-routing event cap and the per-IP
-- registration cap (keys prefixed "ip:"). hour_bucket is hours since the
-- epoch, so the readable form is the instant that bucket opened.
CREATE TABLE IF NOT EXISTS hourly_counts (
  routing_id TEXT NOT NULL,
  hour_bucket INTEGER NOT NULL,
  count INTEGER NOT NULL,
  hour_start_utc TEXT GENERATED ALWAYS AS
    (strftime('%Y-%m-%dT%H:00:00Z', hour_bucket * 3600, 'unixepoch')) VIRTUAL,
  PRIMARY KEY (routing_id, hour_bucket)
);
