# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

SigilTTY Relay is the offline-push pipeline for SigilTTY (sibling repo): a Rust **watcher** deployed onto users' servers over SSH observes herdr agent status and reports transitions, end-to-end encrypted, through a Cloudflare Workers **relay** to APNs. Upstream design: SigilTTY's `docs/design/herdr-offline-push.md` + ADR-0014 — read them before changing anything architectural. Consumer: the SigilTTY app (registration, config deployment, NSE decryption).

## Layout

- `docs/PROTOCOL.md` — **the normative single source**. Registration/renew/events API, HPKE envelope, APNs mapping, server config schema, watcher behavior contract, limits.
- `relay/` — Hono app (TypeScript). `src/app.ts` is runtime-agnostic; runtime specifics live ONLY in the entries: `src/index.ts` (Cloudflare Workers, D1, cron purge) and `src/node.ts` (self-hosted Node ≥ 24, `node:sqlite`, interval purge, undici `allowH2` fetch — **APNs is HTTP/2-only**, Node's built-in fetch cannot reach it). Persistence hides behind the `RelayStore` seam (`store/d1.ts` / `store/sqlite.ts`, both applying `schema.sql`); policy (TTLs, limits) stays in the app, drivers are dumb. Commands: `pnpm test` (vitest: pure pieces + endpoint flow over `:memory:` sqlite), `pnpm typecheck` (two tsconfigs — workers-types and @types/node must both pass; the two type worlds conflict, hence the entry/driver file splits in `tsconfig*.json`).
- `watcher/` — Rust, static musl binaries for `x86_64-unknown-linux-musl` / `aarch64-unknown-linux-musl`. *(pending)*

## Invariants (violation = broken devices in the field)

- **PROTOCOL.md is normative.** Implementations follow it; a breaking change bumps `/v1/` + payload `v` and lands in relay, watcher, and the SigilTTY app in one coordinated release. No end drifts unilaterally.
- **Zero-knowledge relay is a promise, not a state**: no protocol or implementation change may add a plaintext field carrying user content (server names, agent names, statuses). The relay sees routing IDs, APNs tokens, collapse keys, timing — nothing else, ever.
- **Ciphersuite is pinned** (X25519 / HKDF-SHA256 / ChaCha20-Poly1305, HPKE base mode) to CryptoKit's supported combination — the iOS NSE cannot follow arbitrary Rust-side upgrades.
- **Collapse key format (`herdr-<serverID>-<paneID>`) is an app-side contract** — it doubles as the app's local notification identifier for banner replacement. Change it in the SigilTTY repo first or not at all.
- **Debounce lives here** (watcher hysteresis + relay rate limits). The app does no client-side flap filtering — four device-verified failures stand behind that (SigilTTY design doc §1). Don't move it back.
- **The watcher never notifies about itself**: exits are silent, recovery is the app's per-connection health check.
- **Only a successful APNs send advances the per-pane rate-limit clock** — a watcher retry after `sendFailed` must never be blocked by its own failed attempt (pinned by `relay/test/app.test.ts`).

## Git Commit Message Format

Conventional Commits, same as SigilTTY: `<type>(<scope>): <subject>` — scopes `protocol | relay | watcher | bootstrap | ci`.
