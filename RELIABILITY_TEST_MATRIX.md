# Reliability test matrix

This document is the release gate for `ohmyserial`. It records the scenarios that
are covered by deterministic tests, the commands that reproduce them, and the
limits that can only be proven with physical serial hardware and its driver.

## What was used as the compatibility baseline

The interaction model is intentionally aligned with the mature serial tools that
users already know:

- [PuTTY serial configuration](https://www.puttyssh.org/0.70/htmldoc/Chapter4.html)
  defines the expected line, speed, data bits, stop bits, parity, and flow-control
  controls, plus logging/local echo and saved sessions.
- [VOFA+ quick start](https://www.vofa.plus/docs/learning/start/quick_start/) and
  [its protocol-development guide](https://www.vofa.plus/docs/learning/dataengines/development/)
  establish RawData, FireWater, and JustFloat as useful host-side visualization
  workflows and require parsers to tolerate chunked input.
- SSCOM workflows commonly use text/HEX input, automatic send, predefined
  commands, CRC16 helpers, timestamps, and exportable logs; the
  [SSCOM/Modbus example](https://docs.waveshare.com/RS232-RS485-CAN-DALI2/Modbus_RTU_DC_Monitor/SW-Test)
  is a representative public reference.

These references guide the UX and test cases; they are not claims that this
project implements every feature of those products.

## Deterministic release gate

Run from the repository root. The commands below are the minimum gate for a
release candidate:

```text
cargo fmt --all -- --check
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
cargo audit
cargo check --all-targets --target x86_64-unknown-linux-gnu
cargo check --all-targets --target x86_64-apple-darwin
cargo check --all-targets --target aarch64-apple-darwin

cd web
npm ci
npm test
npm run lint
npm run build
npm audit --omit=dev --audit-level=high
cd ..

powershell -ExecutionPolicy Bypass -File .\scripts\release-smoke.ps1
```

`release-smoke.ps1` starts the release binary with `mock:release-smoke` and
checks the real embedded HTTP UI, status, endpoint fan-out, Prometheus metrics,
TCP raw binary echo, event export, and process cleanup. It does not fake the
HTTP client or bypass the running binary. Graceful shutdown behavior is covered
by the hub and API integration suites; the Windows harness terminates its
short-lived smoke process after the assertions complete.

## Scenario coverage

| Area | Scenarios | Automated evidence |
| --- | --- | --- |
| Startup/config | missing device, invalid baud/data/stop/parity/flow, duplicate endpoint names, TCP port overflow, fan-out bounds | `src/config.rs` unit tests; `tests/integration_hub.rs` |
| Serial settings | 5/6/7/8 data bits, 1/1.5/2 stop bits, none/odd/even parity, none/software/hardware flow, open/read/write/flush errors | config validation tests; serial-core and mock-hub integration tests |
| Transports | TCP framed and raw, HTTP write, WebSocket RX, Unix PTY on Unix, RFC2217 negotiation, Windows COM bridge contract | `tests/rfc2217.rs`, `tests/integration_hub.rs`, client unit tests |
| Concurrency | multiple readers, bounded queues, slow client eviction, write lock/lease, reconnect, clean shutdown | hub, policy, ledger, replay, and workflow suites |
| Payloads | text/HEX, ASCII separators, Unicode rejection without panic, empty writes, line endings, checksum modes | API unit/integration tests and `web/tests/checksumUtils.test.ts` |
| Web parsers | FireWater, JustFloat, NMEA, SLIP, COBS, Modbus RTU, partial chunks, noise resynchronization, bounded remainder | `web/tests/protocolUtils.test.ts` (including deterministic chunk fuzzing) |
| Web console | embedded index and hashed asset, status/endpoint/metrics panels, command/profile storage bounds, safe download URL lifecycle | embedded asset integration test; `web/tests/commandUtils.test.ts`; TypeScript lint/build |
| Evidence | RX/TX/connection/control/gap events, query filters, NDJSON export, hash-chain verification, replay read-only boundary | `tests/ledger_api.rs`, `tests/replay.rs`, integration suites |
| Security | origin/auth policy, malformed JSON, invalid IDs, oversized input, traversal-like paths, CORS and method checks | `tests/api_security.rs` |
| Dependencies | Rust advisories and production npm dependency audit | `cargo audit`; `npm audit --omit=dev` |
| Cross-platform | Windows build/test, Linux x86_64 check, macOS x86_64 and arm64 checks | CI plus the target checks above |

## Explicit hardware-in-the-loop boundary

Mock and cross-target checks prove protocol/state-machine behavior, not the
electrical behavior of a particular adapter. Before a production deployment,
run a hardware smoke pass for each supported OS and adapter family:

1. Connect a USB-UART/RS-232/RS-485 adapter and verify `list-ports` identity.
2. Exercise every advertised baud/data/parity/stop/flow combination against a
   known loopback or test instrument.
3. Unplug/replug during RX, TX, and idle; verify the expected reconnect/error
   evidence and that no bytes are silently attributed to a new device.
4. Test RTS/CTS and DTR/RTS control lines with an instrument that exposes the
   signals. DSR/DTR electrical semantics are adapter/driver dependent.
5. If a legacy Windows program requires a COM name, validate the externally
   installed and signed virtual-COM provider (for example com0com) separately;
   ohmyserial's `bridge-com` is a user-mode bridge and does not install a kernel
   driver.
6. Repeat with a real VOFA+, PuTTY, and SSCOM client, checking text/HEX, auto
   send, logs, and the selected protocol analyzer.

UDP transport, a full terminal-emulation mode, and a programmable scripting
engine are intentionally not advertised as implemented capabilities. They can
be added later without weakening the current deterministic release gate.

## Interpreting a green result

A green gate means the checked source, embedded console, mock transport, and
cross-platform compilation paths are reproducible. It is not a claim of
mathematical perfection or of correctness for every USB chipset, OS driver,
electrical wiring, or third-party client. Hardware-in-the-loop evidence should
be attached to the release record before calling a device/adapter combination
production-certified.
