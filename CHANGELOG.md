# Changelog

All notable changes to this project are documented in this file.

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
