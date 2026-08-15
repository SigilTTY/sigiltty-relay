-- Adds the readable `*_utc` projections to a database created before they
-- existed. schema.sql carries them for fresh databases, but its statements
-- are `CREATE TABLE IF NOT EXISTS` and those never alter an existing table,
-- so re-applying schema.sql to a live relay is a no-op — this file is the
-- upgrade path.
--
-- Apply once per database:
--   wrangler d1 execute sigiltty-relay --remote \
--     --file=upgrades/0001_readable_timestamps.sql
--   sqlite3 /var/lib/sigiltty-relay/relay.sqlite3 \
--     < upgrades/0001_readable_timestamps.sql
--
-- Additive and reversible: VIRTUAL generated columns store nothing and are
-- computed on read, no row is rewritten, and `ALTER TABLE … DROP COLUMN`
-- takes them back off. Re-running this file fails on "duplicate column
-- name" — that error means it was already applied, not that anything broke.
--
-- Not named `migrations/`: that is wrangler's own `d1 migrations apply`
-- directory, and its runner tracks state in a `d1_migrations` table this
-- project does not maintain. It would call this file unapplied on a fresh
-- database that already got the columns from schema.sql, then fail.

ALTER TABLE registrations ADD COLUMN created_at_utc TEXT GENERATED ALWAYS AS
  (strftime('%Y-%m-%dT%H:%M:%SZ', created_at, 'unixepoch')) VIRTUAL;

ALTER TABLE registrations ADD COLUMN last_seen_at_utc TEXT GENERATED ALWAYS AS
  (strftime('%Y-%m-%dT%H:%M:%SZ', last_seen_at, 'unixepoch')) VIRTUAL;

ALTER TABLE send_log ADD COLUMN sent_at_utc TEXT GENERATED ALWAYS AS
  (strftime('%Y-%m-%dT%H:%M:%SZ', sent_at, 'unixepoch')) VIRTUAL;

ALTER TABLE hourly_counts ADD COLUMN hour_start_utc TEXT GENERATED ALWAYS AS
  (strftime('%Y-%m-%dT%H:00:00Z', hour_bucket * 3600, 'unixepoch')) VIRTUAL;
