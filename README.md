# SigilTTY Relay

Offline push pipeline for SigilTTY's herdr agent notifications: when the app is suspended, a server-side watcher observes agent status over the herdr CLI and reports transitions — end-to-end encrypted — through a hosted relay to APNs.

Design and decision record live in the SigilTTY repo: `docs/design/herdr-offline-push.md` + ADR-0014. The normative protocol both ends implement is [docs/PROTOCOL.md](docs/PROTOCOL.md).

## Components

| Component | Path | Stack | Status |
|---|---|---|---|
| Protocol v1 | `docs/PROTOCOL.md` | — | **defined** |
| Relay | `relay/` | Hono — Workers + D1, or self-hosted Node + `node:sqlite` | **implemented** (deploy pending) |
| Watcher | `watcher/` | Rust (static musl binaries) | **implemented** (CI pending) |
| Bootstrap + CI | `scripts/`, `.github/` | — | pending |

## Privacy model

The relay is zero-knowledge by construction: it holds the APNs key and routing table, but event content (server names, agent names, statuses) is HPKE-sealed from the user's server directly to the user's device — decrypted only by the app's Notification Service Extension. Registration is anonymous; pairing rides the user's own SSH access. See PROTOCOL.md §1.
