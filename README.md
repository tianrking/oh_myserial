# ohmyserial

<p align="center">
  <img alt="ohmyserial" src="https://img.shields.io/badge/ohmyserial-serial%20hub-0ea5e9?style=for-the-badge&logo=rust&logoColor=white" />
</p>

<p align="center">
  <strong>Cross-platform open-source serial hub for humans and agents</strong><br/>
  <em>One real UART · Many safe clients · Zero silent TX fights</em>
</p>

<p align="center">
  <a href="./README.md"><img alt="English" src="https://img.shields.io/badge/lang-English-blue?style=flat-square" /></a>
  <a href="./README.zh-CN.md"><img alt="简体中文" src="https://img.shields.io/badge/lang-简体中文-red?style=flat-square" /></a>
  <a href="./README.es.md"><img alt="Español" src="https://img.shields.io/badge/lang-Español-green?style=flat-square" /></a>
</p>

<p align="center">
  <b>Languages:</b>
  <a href="./README.md">English</a> ·
  <a href="./README.zh-CN.md">简体中文</a> ·
  <a href="./README.es.md">Español</a>
</p>

<p align="center">
  <a href="https://github.com/tianrking/oh_myserial/actions/workflows/ci.yml"><img alt="CI" src="https://img.shields.io/github/actions/workflow/status/tianrking/oh_myserial/ci.yml?branch=main&style=flat-square&label=CI" /></a>
  <a href="./LICENSE"><img alt="License" src="https://img.shields.io/badge/license-MIT-blue?style=flat-square" /></a>
  <a href="https://www.rust-lang.org/"><img alt="Rust" src="https://img.shields.io/badge/rust-edition%202021-orange?style=flat-square&logo=rust" /></a>
  <img alt="Platforms" src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux%20%7C%20Windows-lightgrey?style=flat-square" />
  <img alt="Version" src="https://img.shields.io/badge/version-v0.0.1-blue?style=flat-square" />
  <img alt="Status" src="https://img.shields.io/badge/status-MVP-22c55e?style=flat-square" />
  <a href="https://github.com/tianrking/oh_myserial/releases/tag/v0.0.1"><img alt="Release" src="https://img.shields.io/badge/release-v0.0.1-0ea5e9?style=flat-square" /></a>
  <a href="https://github.com/tianrking/oh_myserial"><img alt="GitHub" src="https://img.shields.io/badge/github-tianrking%2Foh__myserial-181717?style=flat-square&logo=github" /></a>
</p>

<p align="center">
  <img alt="serial" src="https://img.shields.io/badge/serial-UART%20%2F%20COM%20%2F%20tty-0ea5e9?style=flat-square" />
  <img alt="hub" src="https://img.shields.io/badge/hub-mux%20%2F%20share-8b5cf6?style=flat-square" />
  <img alt="websocket" src="https://img.shields.io/badge/API-HTTP%20%2B%20WebSocket-06b6d4?style=flat-square" />
  <img alt="agent" src="https://img.shields.io/badge/AI-agent%20friendly-f59e0b?style=flat-square" />
  <img alt="embedded" src="https://img.shields.io/badge/domain-embedded%20debug-64748b?style=flat-square" />
  <img alt="tokio" src="https://img.shields.io/badge/async-tokio-c026d3?style=flat-square" />
  <img alt="axum" src="https://img.shields.io/badge/web-axum-7c3aed?style=flat-square" />
  <img alt="pty" src="https://img.shields.io/badge/Unix-PTY-14b8a6?style=flat-square" />
  <img alt="tcp" src="https://img.shields.io/badge/stream-TCP-3b82f6?style=flat-square" />
  <img alt="toml" src="https://img.shields.io/badge/config-TOML-e11d48?style=flat-square" />
</p>

---

## Table of contents

- [What is ohmyserial?](#what-is-ohmyserial)
- [Problem & solution](#problem--solution)
- [Features](#features)
- [How it works](#how-it-works)
- [Platform support](#platform-support)
- [Install & build](#install--build)
- [Quick start](#quick-start)
- [How to use (scenarios)](#how-to-use-scenarios)
- [Configuration](#configuration)
- [CLI reference](#cli-reference)
- [HTTP & WebSocket API](#http--websocket-api)
- [TX policies](#tx-policies)
- [Unix PTY](#unix-pty-macos--linux)
- [Windows notes](#windows-notes)
- [Security](#security)
- [Project structure](#project-structure)
- [Development](#development)
- [Roadmap](#roadmap)
- [FAQ](#faq)
- [Contributing](#contributing)
- [License](#license)
- [Tech tags](#tech-tags)

---

## What is ohmyserial?

**ohmyserial** is a small, open-source **serial port sharing hub** written in Rust.

It:

1. Opens the **real serial device** once (exclusive ownership)
2. **Fans out RX** (device → host) to many clients
3. **Arbitrates TX** (host → device) so concurrent writers do not silently corrupt frames
4. Exposes clients as **TCP**, **HTTP/WebSocket** (agent-first), and **PTY** (macOS/Linux host tools)

Ideal for embedded debug when **a human terminal and an AI agent/script must share one UART**.

| Item | Value |
|------|--------|
| Binary name | `ohmyserial` |
| Repository | [github.com/tianrking/oh_myserial](https://github.com/tianrking/oh_myserial) |
| Language | Rust (edition 2021) |
| License | MIT |
| Default docs | **English** · [中文](./README.zh-CN.md) · [Español](./README.es.md) |
| Architecture deep-dive | [`POSITIONING.md`](./POSITIONING.md) |

---

## Problem & solution

### The problem

| Goal | Reality |
|------|---------|
| Keep a serial monitor open | Port is busy |
| Let an agent/script read the same log | Second open fails |
| Let both send commands | Bytes interleave → broken protocol |

### The solution

```text
Device (UART/COM)
        │
        ▼
   ┌──────────┐
   │ ohmyserial│  ← only process that opens the real port
   └────┬─────┘
        │
   ┌────┴─────────────────────────────┐
   ▼                ▼                 ▼
  PTY            TCP stream      HTTP + WebSocket
 (host UI)       (scripts)         (agents)
```

---

## Features

### Functional features

| Feature | Description | Status |
|---------|-------------|--------|
| Exclusive real port | Single owner of hardware UART | ✅ |
| Port parameters | baud, data bits, parity, stop bits, flow control | ✅ |
| RX fan-out | All readable clients receive device data | ✅ |
| TX arbitration | Line/frame queue, exclusive, primary preference | ✅ |
| Write-lock lease | Time-bounded TX ownership | ✅ |
| Auto reconnect | Optional reopen after disconnect | ✅ |
| TCP client | Raw bidirectional byte stream | ✅ |
| HTTP API | health / status / write / lock | ✅ |
| WebSocket stream | Live RX (+ optional history on connect) | ✅ |
| Unix PTY | Symlinked virtual serial for classic tools | ✅ (macOS/Linux) |
| Session log | Console + file; text / hex / hex+text | ✅ |
| Mock port | `mock:demo` loopback without hardware | ✅ |
| TOML config + CLI | `run` / `init` / `list-ports` / `status` | ✅ |
| Multi-port single process | Multiple real profiles in one process | 🔜 |
| RFC2217 | Telnet serial control over network | 🔜 |
| Native Windows virtual COM | Kernel/driver-level COM clone | 🔜 / external bridge |

### Technical features

| Area | Stack / design |
|------|----------------|
| Runtime | Tokio async |
| HTTP/WS | Axum |
| Serial I/O | `serialport` + dedicated reader thread |
| Config | Serde + TOML |
| Logging | `tracing` + session blog |
| Unix PTY | `nix` openpty + symlink |
| Tests | Unit + integration (mock hub) |
| CI | GitHub Actions: Ubuntu · macOS · Windows |

---

## How it works

### Data plane

```text
Device ──RX──► Serial Core ──► Broker.broadcast ──► clients
Client ──TX──► Broker.admit(policy/lock) ──► Serial Core ──► Device
```

### Control plane

- `GET /v1/status` — connected?, baud, clients, lock owner, counters  
- `POST /v1/lock` / `DELETE /v1/lock` — write lease  
- `POST /v1/write` — inject TX as a named client  

### Architecture modules

```text
CLI / Config
    └── Hub supervisor
            ├── Serial core (open, reconnect, mock)
            ├── Broker (registry, fan-out, TX queue)
            ├── Policy (queue_by_line / exclusive / …)
            ├── Clients: PTY · TCP · HTTP/WS
            └── Observe (session log, tracing)
```

---

## Platform support

| Capability | macOS | Linux / Ubuntu | Windows |
|------------|:-----:|:--------------:|:-------:|
| Real serial (`/dev/cu.*`, `/dev/ttyUSB*`, `COM3`) | ✅ | ✅ | ✅ |
| TCP raw stream | ✅ | ✅ | ✅ |
| HTTP + WebSocket API | ✅ | ✅ | ✅ |
| PTY virtual serial | ✅ | ✅ | — |
| Mock loopback | ✅ | ✅ | ✅ |
| Architectures | arm64 / x64 | x64 / arm64 | x64 / arm64 |

**Ubuntu tip:** install `build-essential pkg-config libudev-dev` before building.

**Windows tip:** apps that only list COM ports need TCP/WS or an external COM bridge; PTY is Unix-only.

---

## Install & build

### Prebuilt binaries (v0.0.1+)

Download from [GitHub Releases](https://github.com/tianrking/oh_myserial/releases):

| Platform | Artifact |
|----------|----------|
| Linux x86_64 | `…-x86_64-unknown-linux-gnu.tar.gz` |
| Linux arm64 | `…-aarch64-unknown-linux-gnu.tar.gz` |
| macOS Apple Silicon | `…-aarch64-apple-darwin.tar.gz` |
| macOS Intel | `…-x86_64-apple-darwin.tar.gz` |
| Windows x64 | `…-x86_64-pc-windows-msvc.zip` |
| Windows ARM64 | `…-aarch64-pc-windows-msvc.zip` |

```bash
# example (Linux/macOS)
tar -xzf ohmyserial-v0.0.1-<target>.tar.gz
./ohmyserial-v0.0.1-<target>/ohmyserial share mock:demo --ui
```

### Requirements (from source)

- [Rust](https://rustup.rs/) stable  
- **Ubuntu/Debian:** `build-essential pkg-config libudev-dev`  
- **macOS:** Xcode CLT · **Windows:** MSVC via rustup  

### Build from source

```bash
git clone https://github.com/tianrking/oh_myserial.git
cd oh_myserial
./scripts/build-all.sh   # web + cargo release
# or: cargo build --release   # uses committed web/dist
```

| OS | Binary |
|----|--------|
| Unix | `./target/release/ohmyserial` |
| Windows | `.\target\release\ohmyserial.exe` |

### Verify

```bash
cargo test
./target/release/ohmyserial --help
./target/release/ohmyserial share mock:demo --ui
```

### CI matrix

GitHub Actions builds & tests on:

- Linux x64 / arm64  
- macOS arm64 / x64  
- Windows x64 / arm64  

Tagged releases (`v*`) publish archives automatically.

---

## Quick start (easiest)

**No config file needed** — use `share`:

```bash
# 1) see ports
./target/release/ohmyserial list-ports

# 2) share one (macOS/Linux: 2 virtual serials by default + TCP + WebSocket)
./target/release/ohmyserial share /dev/cu.usbmodem14101 --baud 115200

# more virtual serials for more serial GUIs
./target/release/ohmyserial share /dev/ttyUSB0 --pty 3 --tcp 1

# Windows (no PTY): multi TCP + multi WS clients
./target/release/ohmyserial share COM3 --tcp 2

# no hardware (demo)
./target/release/ohmyserial share mock:demo
```

On start, ohmyserial prints a **connect card**, e.g.:

```text
SERIAL  /tmp/ohmyserial-v0     ← open in serial app #1
SERIAL  /tmp/ohmyserial-v1     ← open in serial app #2
TCP     127.0.0.1:8788         ← nc / scripts (many clients OK)
WS      ws://127.0.0.1:8787/v1/stream
HTTP    http://127.0.0.1:8787
```

### Flags people use most

| Flag | Meaning | Default |
|------|---------|---------|
| `--pty N` | N virtual serial ports (Unix) | `2` on macOS/Linux, `0` on Windows |
| `--tcp N` | N TCP ports | `1` |
| `--tcp-base P` | first TCP port | `8788` |
| `--api ADDR` | HTTP/WS bind | `127.0.0.1:8787` |
| `-b/--baud` | baud rate | `115200` |

### Optional: config file

```bash
./target/release/ohmyserial init -o ohmyserial.toml
./target/release/ohmyserial run -c ohmyserial.toml
# overrides without editing file:
./target/release/ohmyserial run -c ohmyserial.toml -d /dev/ttyUSB0 --pty 3
```

---

## How to use (scenarios)

### Core idea: one real port → many parallel endpoints

```text
                 ┌─ PTY /tmp/ohmyserial-v0  → serial GUI #1
                 ├─ PTY /tmp/ohmyserial-v1  → serial GUI #2  (or agent via serial lib)
 Real UART ──► hub ┼─ TCP :8788            → many nc/scripts at once
                 ├─ TCP :8789            → more tools
                 └─ WS  /v1/stream        → many agents at once
```

All endpoints receive the **same live RX**. TX is shared under policy/lock so writers do not silently interleave.

Configure bulk expansion with **`[fanout]`** (see below), or list individual `[[clients]]`.

Discover live endpoints:

```bash
curl -s http://127.0.0.1:8787/v1/endpoints | jq .
```

### Scenario A — Multiple host programs + agent

```toml
[fanout]
pty_count = 2                 # macOS/Linux virtual serials
pty_link_prefix = "/tmp/ohmyserial-v"
tcp_count = 1
tcp_base_port = 8788

[api]
bind = "127.0.0.1:8787"
```

- Open `/tmp/ohmyserial-v0` and `/tmp/ohmyserial-v1` in two serial apps  
- Agent connects to `ws://127.0.0.1:8787/v1/stream` (many agents OK)  
- Scripts: `nc 127.0.0.1 8788` (many connections OK)

### Scenario B — Scripts / CI only

`[fanout] tcp_count = 2` + API; skip PTY. Use `nc`, Python, or CI against `8787`/`8788`/`8789`.

### Scenario C — Demo without hardware

Keep `path = "mock:demo"`. Writes loop back as RX (great for tests).

### Scenario D — Agent-only write with lock

```bash
curl -s -X POST http://127.0.0.1:8787/v1/lock \
  -H 'content-type: application/json' \
  -d '{"as_client":"agent"}'
# ... exclusive window ...
curl -s -X DELETE http://127.0.0.1:8787/v1/lock
```

---

## Configuration

Example file: [`ohmyserial.example.toml`](./ohmyserial.example.toml)

```bash
ohmyserial init -o ohmyserial.toml
```

### Bulk fan-out `[fanout]`

| Field | Effect |
|-------|--------|
| `pty_count` | N Unix virtual serials (`{prefix}0` …) for multiple serial GUIs |
| `tcp_count` + `tcp_base_port` | N TCP listeners; **each** accepts many concurrent clients |
| `ws_binds` | Extra HTTP/WS servers (primary `[api]` already multi-client) |

```toml
[real]
path = "mock:demo"          # or /dev/ttyUSB0, COM3, …
baud = 115200
reconnect = true

[tx]
mode = "queue_by_line"
write_lock_ms = 3000
primary = "ui"

[api]
bind = "127.0.0.1:8787"
enabled = true

[fanout]
pty_count = 0               # set 2+ on macOS/Linux for multi serial GUI
pty_link_prefix = "/tmp/ohmyserial-v"
tcp_count = 2
tcp_host = "127.0.0.1"
tcp_base_port = 8788
# ws_binds = ["127.0.0.1:8790"]

# Optional explicit endpoints (merged with fanout):
# [[clients]]
# type = "pty"
# name = "ui"
# link = "/tmp/ohmyserial-ui"

[log]
mirror_console = true
format = "hex+text"
```

| Field | Meaning |
|-------|---------|
| `real.path` | Device path or `mock:name` |
| `tx.mode` | Concurrent write policy |
| `fanout.*` | Auto-create many parallel endpoints |
| `api.bind` | HTTP/WS (prefer localhost); unlimited WS clients on `/v1/stream` |
| `clients[]` | Explicit endpoints (optional) |

---

## CLI reference

```bash
ohmyserial run -c ohmyserial.toml    # start hub
ohmyserial init [-o file]           # sample config to stdout/file
ohmyserial list-ports               # list serial devices
ohmyserial status [--api URL]       # GET /v1/status
```

```bash
RUST_LOG=debug ohmyserial run -c ohmyserial.toml
```

---

## HTTP & WebSocket API

**Base:** `http://127.0.0.1:8787` (default)

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/v1/health` | Liveness JSON |
| `GET` | `/v1/status` | Port, endpoints, clients, lock, stats |
| `GET` | `/v1/endpoints` | Configured fan-out endpoints catalog |
| `GET` | `/v1/clients` | Connected client list |
| `POST` | `/v1/write` | Send text or hex to device |
| `POST` | `/v1/lock` | Acquire write lock |
| `DELETE` | `/v1/lock` | Release write lock |
| `WS` | `/v1/stream` | Live RX stream |

### Write

```bash
# text (newline appended by default)
curl -s -X POST http://127.0.0.1:8787/v1/write \
  -H 'content-type: application/json' \
  -d '{"text":"AT","newline":true,"as_client":"agent"}'

# hex
curl -s -X POST http://127.0.0.1:8787/v1/write \
  -H 'content-type: application/json' \
  -d '{"hex":"41 54 0d 0a","as_client":"agent"}'
```

### WebSocket

```text
ws://127.0.0.1:8787/v1/stream
```

- Server → client: binary RX (history may be sent first)  
- Client → server: text/binary TX (policy + lock apply)

### TCP

```bash
nc 127.0.0.1 8788
```

### Minimal Python agent

```python
import json, urllib.request

req = urllib.request.Request(
    "http://127.0.0.1:8787/v1/write",
    data=json.dumps({"text": "status", "newline": True}).encode(),
    headers={"content-type": "application/json"},
    method="POST",
)
print(urllib.request.urlopen(req).read().decode())
```

---

## TX policies

| Mode | Behavior | Typical use |
|------|----------|-------------|
| `queue_by_line` **(default)** | Wait for `\n`, then send whole line | Text / AT / CLI |
| `queue_by_frame` | Wait for delimiter byte | Simple binary frames |
| `exclusive` | TX only with active write lock | Flash / critical ops |
| `primary_wins` | Prefer `tx.primary` client | Human-in-the-loop |

While a **write lock** is held, only the owner may TX. Lease ends on timeout, release, or disconnect.

`slow_client = drop_oldest` (default) protects real-time device reading.

---

## Unix PTY (macOS / Linux)

```toml
[[clients]]
type = "pty"
name = "ui"
link = "/tmp/ohmyserial-ui"
can_write = true
can_read = true
```

Open `/tmp/ohmyserial-ui` in minicom, screen, Serial Studio, etc.

> Real baud/framing is owned by the hub `[real]` section. Some apps may fail PTY baud ioctls; the data path still works.

---

## Windows notes

| Need | Use |
|------|-----|
| Agent / automation | HTTP + WebSocket ✅ |
| Simple stream | TCP `127.0.0.1:8788` ✅ |
| Hardware | `path = "COM3"` ✅ |
| COM-only legacy UI | External bridge (e.g. com0com) — not built-in yet |
| `type = "pty"` | Not supported (config rejected) |

---

## Security

- Default binds are **localhost only** (`127.0.0.1`)  
- Serial TX can reset boards / send dangerous commands — treat as privileged  
- Do not bind `0.0.0.0` on untrusted networks without auth (auth is post-MVP)  
- Logs may contain secrets from the device stream  

---

## Project structure

```text
oh_myserial/
├── README.md                 # English (default)
├── README.zh-CN.md
├── README.es.md
├── POSITIONING.md
├── ohmyserial.example.toml
├── web/                      # Optional React console (Traditional Chinese)
│   ├── PROTOCOL.zh-TW.md     # Full HTTP/WS protocol
│   ├── README.zh-TW.md
│   └── src/
├── src/                      # Rust hub + CLI
└── tests/
```

### Web console (embedded + optional Vercel)

The React UI is **embedded** into the hub binary (from `web/dist`):

```bash
cd web && npm ci && npm run build && cd ..
cargo build --release
./target/release/ohmyserial share mock:demo --ui
# open http://127.0.0.1:8787/
```

| Mode | URL |
|------|-----|
| **Recommended** | `http://127.0.0.1:8787/` same-origin UI + API + WS |
| Dev hot-reload | `cd web && npm run dev` → localhost:5173 |
| Optional CDN/Vercel | `web/vercel.json` (HTTPS→local WS may be blocked) |

Protocol (zh-TW): [`web/PROTOCOL.zh-TW.md`](./web/PROTOCOL.zh-TW.md).

---

## Development

```bash
cargo test
cargo run -- run -c ohmyserial.example.toml
cargo fmt
cargo clippy
```

CI: [`.github/workflows/ci.yml`](./.github/workflows/ci.yml) — Ubuntu, macOS, Windows.

---

## Roadmap

| Phase | Scope |
|-------|--------|
| ✅ MVP | Hub core, policies, TCP, HTTP/WS, Unix PTY, mock, logs, CLI |
| 🔜 Next | Multi-port, richer history, Windows COM bridge guide, hardening |
| 🧭 Later | RFC2217, record/replay, light web monitor, metrics |

Not core goals: cloud SaaS, heavy GUI installer, kernel virtual-COM driver (unless demand is clear).

---

## FAQ

**Can two clients write at once?**  
Not as interleaved bytes. Default mode queues complete lines; locks grant exclusive windows.

**Must the agent use a virtual COM port?**  
No. Prefer WebSocket + HTTP.

**Why does my terminal’s baud setting on PTY not change the device?**  
The hub owns the real port settings.

**Is this only a sniffer?**  
No. It is an interactive **share hub** with TX control, not passive capture only.

**Does mock mode need hardware?**  
No. `mock:demo` loops TX back as RX.

---

## Contributing

Issues & PRs: https://github.com/tianrking/oh_myserial  

Please stay aligned with [`POSITIONING.md`](./POSITIONING.md).

---

## License

[MIT](./LICENSE) © ohmyserial contributors

---

## Tech tags

`serial` · `uart` · `com-port` · `tty` · `serial-hub` · `serial-mux` · `port-sharing` · `embedded` · `debugging` · `ai-agent` · `websocket` · `http-api` · `tcp` · `pty` · `tokio` · `axum` · `rust` · `cross-platform` · `macos` · `linux` · `windows` · `toml` · `mit-license` · `ohmyserial`

---

<p align="center">
  <a href="./README.md">English</a> ·
  <a href="./README.zh-CN.md">简体中文</a> ·
  <a href="./README.es.md">Español</a>
  <br/>
  <sub>One port. Many clients. Zero fights.</sub>
</p>
