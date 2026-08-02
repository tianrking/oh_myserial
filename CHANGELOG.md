# Changelog

All notable changes to this project are documented in this file.

## [Unreleased]

### Bounded workflows

- Added a finite linear workflow DSL with lease, atomic send, incremental RX expect, explicit assertions, waits, and reserved control steps.
- Added server-generated workflow actors, idempotent `request_id` handling, bounded evidence cursors, cancellation, and fail-closed behavior for RX gaps, lag, disconnects, and epoch changes.
- Added `POST /v1/workflows/run`; physical serial control-line operations remain unavailable until the owner-side command channel is implemented.

### Event ledger and safe replay

- Added a canonical `ohmyserial.event` v1 envelope with session sequence, UTC/monotonic time, port connection epoch, typed RX/TX/connection/control/gap payloads, and standard padded Base64 byte encoding.
- Added an always-on event/byte-bounded memory ring plus read status, cursor query, filtered catch-up, NDJSON export, and read-only snapshot-then-live event WebSocket APIs.
- Added opt-in append-only NDJSON persistence with size/event rotation, SHA-256 content and cross-segment chain verification, checkpointing, per-session locking, and conservative stale `.open` recovery that preserves source files.
- Added verified offline replay in immediate, original-timing, and manual-step modes. Replay emits unchanged envelopes and has no path to the live broker, serial owner, leases, or device writes.
- Documented that hashes detect corruption but are not signatures, retention deletion is operator-owned, process-observed RX starts after the driver read boundary, host-confirmed TX is not a device acknowledgement, and mock coverage is not hardware-in-the-loop evidence.

### Trusted serial core

- Added credentialed write leases: acquire returns a random opaque `lease_token`; renew, protected writes, and release require that token. Owner names are display/audit labels and disconnect no longer releases a lease by name.
- Made HTTP writes atomic and confirmed: success follows the serial owner's host-side `write_all` + `flush`, with bounded queue/ack deadlines and explicit partial-or-unknown outcome errors.
- Bounded delimiter frames and atomic writes with `max_frame_bytes` / `max_write_bytes`; stale queued writes are rejected across serial connection epochs instead of replayed after reconnect.
- Implemented all bounded slow-reader policies: drop oldest, drop newest, immediate disconnect, and deadline-bounded block-then-disconnect.
- Removed unbounded TX bridges, bounded serial write bursts, and made shutdown reject/drain pending writes and stop owned workers.
- Put Unix PTY slaves into raw mode, keep endpoints alive before the first slave opener, and cover bidirectional PTY bytes in a real Linux test.
- Made Linux builds portable without a mandatory `libudev-dev` package by using serialport's sysfs enumeration fallback.
- Made listener startup fail atomically: bind/configuration failures abort already-started tasks instead of reporting a partially ready hub.

### API security

- Added environment-sourced Bearer authentication, API read/write permissions, exact-origin CORS, and WebSocket Origin validation.
- Browser WebSocket authentication uses the `bearer` subprotocol pair; query-string tokens are not accepted.
- Plaintext API, dedicated WebSocket, and raw TCP listeners are loopback-only even when a bearer is configured; remote use goes through SSH or a TLS reverse proxy. HTTP/WS Host validation blocks DNS rebinding aliases.

### Verification boundary

- Expanded trusted-core regression coverage for disconnect/reconnect, lease spoofing, bind failures, atomic write framing, bounded queues, and lifecycle cleanup.
- Mock loopback tests validate hub behavior only; physical serial drivers, USB/UART timing, control lines, and real-device acknowledgements still require hardware-in-the-loop verification.

## [0.0.1] — 2026-07-24

First public release.

### Features
- Exclusive real serial port ownership with reconnect
- RX fan-out to many clients (PTY / TCP / WebSocket)
- TX arbitration (`queue_by_line`, frame, exclusive, primary) + write locks
- Bulk `[fanout]` and CLI `share` (zero-config multi endpoints)
- HTTP API: health, status, endpoints, write, lock
- WebSocket stream `/v1/stream`
- Embedded React console (Traditional Chinese) at `http://API/`
- `share --ui` / `run --ui` opens browser
- Mock loopback `mock:demo` for demos without hardware
- Cross-platform: macOS, Linux, Windows (x64 & arm64)

### Docs
- README (EN / 简体中文 / Español)
- `POSITIONING.md`, `web/PROTOCOL.zh-TW.md`

### CI
- Multi-OS / multi-arch build & test
- Tagged release workflow with binary assets
