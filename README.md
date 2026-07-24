# ohmyserial

<p align="center">
  <strong>Cross-platform open-source serial hub for humans and agents</strong>
</p>

<p align="center">
  One real UART · Many safe clients · Zero silent TX fights
</p>

<p align="center">
  <a href="https://github.com/tianrking/oh_myserial/actions"><img alt="CI" src="https://img.shields.io/github/actions/workflow/status/tianrking/oh_myserial/ci.yml?branch=main&style=flat-square&label=CI" /></a>
  <a href="./LICENSE"><img alt="License" src="https://img.shields.io/badge/license-MIT-blue?style=flat-square" /></a>
  <a href="./Cargo.toml"><img alt="Rust" src="https://img.shields.io/badge/rust-1.70%2B-orange?style=flat-square" /></a>
  <img alt="Platforms" src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux%20%7C%20Windows-lightgrey?style=flat-square" />
  <img alt="Status" src="https://img.shields.io/badge/status-MVP-green?style=flat-square" />
</p>

---

## Why ohmyserial?

A hardware serial port can only be opened by **one process**. That breaks the modern debug loop:

| You want… | But… |
|-----------|------|
| A classic serial terminal / host app open | The port is already taken |
| An AI agent / script reading the same log | Can’t open the same COM/tty |
| Both sending commands safely | Bytes interleave and protocols break |

**ohmyserial** solves this by **exclusively owning the real port**, then:

1. **Broadcasting RX** to every client  
2. **Arbitrating TX** so writers don’t silently corrupt the stream  
3. Offering **dual access**: virtual serial (Unix) + TCP / HTTP / WebSocket (all platforms, agent-friendly)

> Deep product & architecture notes: [`POSITIONING.md`](./POSITIONING.md)

---

## Architecture

```text
                    ┌─────────────────────────────────────┐
                    │            ohmyserial hub           │
                    │  (single process, owns real port)   │
                    └──────────────────┬──────────────────┘
                                       │
              ┌────────────────────────┼────────────────────────┐
              │                        │                        │
              ▼                        ▼                        ▼
     ┌────────────────┐      ┌─────────────────┐      ┌─────────────────┐
     │  Serial Core   │      │  Broker         │      │  Observability  │
     │  open/reconnect│◄────►│  RX fan-out     │─────►│  session log    │
     │  baud / flow   │      │  TX policy/lock │      │  tracing        │
     └────────────────┘      └────────┬────────┘      └─────────────────┘
                                      │
                 ┌────────────────────┼────────────────────┐
                 │                    │                    │
                 ▼                    ▼                    ▼
          ┌────────────┐       ┌────────────┐       ┌────────────┐
          │ PTY (Unix) │       │ TCP stream │       │ HTTP + WS  │
          │ host tools │       │ scripts    │       │ agents     │
          └────────────┘       └────────────┘       └────────────┘
```

**Data path**

```text
Device ──RX──► Hub ──broadcast──► all clients
Client ──TX──► Hub ──admit/queue/lock──► Device
```

---

## Features

### Core

| Feature | Description |
|---------|-------------|
| **Exclusive real port** | Only the hub opens the hardware UART |
| **RX fan-out** | Every readable client gets the same device data |
| **TX arbitration** | Line/frame queue, exclusive mode, primary preference |
| **Write-lock lease** | Time-bounded ownership for safe multi-writer control |
| **Auto reconnect** | Optional reopen when the device drops |
| **Session logging** | Console + file, text / hex / hex+text |
| **Mock port** | `path = "mock:demo"` loopback without hardware |

### Clients

| Client | Platforms | Best for |
|--------|-----------|----------|
| **HTTP + WebSocket API** | All | AI agents, automation, status/control |
| **TCP raw stream** | All | `nc`, scripts, simple tools |
| **PTY virtual serial** | macOS / Linux | Classic serial terminals & host apps |

### Platform matrix

| | macOS | Linux / Ubuntu | Windows |
|--|:-----:|:--------------:|:-------:|
| Real serial | ✅ | ✅ | ✅ |
| TCP / HTTP / WS | ✅ | ✅ | ✅ |
| PTY virtual port | ✅ | ✅ | — |
| Mock loopback | ✅ | ✅ | ✅ |
| Native virtual COM | — | — | planned* |

\* Windows COM-only host apps: use TCP/WS today, or bridge later (e.g. com0com). See [Windows notes](#windows-notes).

---

## Install & build

### Requirements

- [Rust](https://rustup.rs/) stable (1.70+ recommended)
- **Ubuntu / Debian:**

  ```bash
  sudo apt update
  sudo apt install -y build-essential pkg-config libudev-dev
  ```

- **Windows:** MSVC toolchain (`rustup default stable-msvc`) or MinGW as preferred  
- **macOS:** Xcode CLT usually enough

### From source

```bash
git clone https://github.com/tianrking/oh_myserial.git
cd oh_myserial
cargo build --release
```

Binary: `./target/release/ohmyserial`  
(Windows: `.\target\release\ohmyserial.exe`)

### Verify

```bash
cargo test
./target/release/ohmyserial --help
```

---

## Quick start (2 minutes)

### 1. Create a config

```bash
./target/release/ohmyserial init -o ohmyserial.toml
```

Default sample uses **`mock:demo`** (no hardware).

### 2. Run the hub

```bash
./target/release/ohmyserial run -c ohmyserial.toml
```

You should see the API on `http://127.0.0.1:8787` and TCP on `127.0.0.1:8788`.

### 3. Talk to it

```bash
# health
curl -s http://127.0.0.1:8787/v1/health

# send a line (mock loops it back as RX)
curl -s -X POST http://127.0.0.1:8787/v1/write \
  -H 'content-type: application/json' \
  -d '{"text":"hello","newline":true}'

# raw TCP
nc 127.0.0.1 8788
```

### 4. Attach real hardware

Edit `ohmyserial.toml`:

```toml
[real]
# macOS example
path = "/dev/cu.usbmodem14101"
# Linux example
# path = "/dev/ttyUSB0"
# Windows example
# path = "COM3"
baud = 115200
```

List ports:

```bash
./target/release/ohmyserial list-ports
```

---

## Usage patterns

### A. Human + Agent (recommended)

```text
┌──────────────┐     PTY / TCP      ┌────────────┐
│ Serial app   │◄──────────────────►│            │
└──────────────┘                    │ ohmyserial │◄──► Device
┌──────────────┐   WS + HTTP API    │            │
│ AI Agent     │◄──────────────────►│            │
└──────────────┘                    └────────────┘
```

- **Human:** Unix PTY (`/tmp/ohmyserial-ui`) or TCP  
- **Agent:** `WS /v1/stream` for logs + `POST /v1/write` for commands  

### B. Scripts only

Enable TCP + API; skip PTY. Pipe with `nc`, Python, or CI jobs.

### C. No hardware / CI

Keep `path = "mock:demo"` for loopback demos and automated tests.

---

## Configuration

Full example: [`ohmyserial.example.toml`](./ohmyserial.example.toml)

Generate one anytime:

```bash
ohmyserial init -o ohmyserial.toml
```

### Important sections

```toml
[real]
path = "mock:demo"          # or /dev/ttyUSB0, COM3, …
baud = 115200
reconnect = true

[tx]
mode = "queue_by_line"      # see TX policies below
write_lock_ms = 3000
primary = "ui"

[api]
bind = "127.0.0.1:8787"     # default: localhost only
enabled = true

[[clients]]
type = "tcp"
name = "tcp"
bind = "127.0.0.1:8788"

[[clients]]
type = "websocket"
name = "agent"
history_bytes = 65536

# macOS / Linux only
# [[clients]]
# type = "pty"
# name = "ui"
# link = "/tmp/ohmyserial-ui"
```

| Key | Meaning |
|-----|---------|
| `real.path` | Device path, or `mock:…` for loopback |
| `tx.mode` | How concurrent writes are serialized |
| `tx.write_lock_ms` | Lock lease duration |
| `api.bind` | HTTP/WS listen address (**prefer 127.0.0.1**) |
| `clients[].can_write` | Whether that client may TX |

---

## CLI

```bash
ohmyserial run -c ohmyserial.toml     # start hub
ohmyserial init [-o file]            # print/write sample config
ohmyserial list-ports                # enumerate serial devices
ohmyserial status [--api URL]        # fetch /v1/status from a running hub
```

Logging verbosity:

```bash
RUST_LOG=debug ohmyserial run -c ohmyserial.toml
```

---

## HTTP & WebSocket API

Base URL (default): `http://127.0.0.1:8787`

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/v1/health` | Liveness |
| `GET` | `/v1/status` | Port state, clients, lock, stats |
| `GET` | `/v1/clients` | Connected clients |
| `POST` | `/v1/write` | Send bytes/text to the device |
| `POST` | `/v1/lock` | Acquire write lock |
| `DELETE` | `/v1/lock` | Release write lock |
| `WS` | `/v1/stream` | Binary/text RX stream (+ optional history) |

### Write body

```bash
# text (appends \n by default)
curl -s -X POST http://127.0.0.1:8787/v1/write \
  -H 'content-type: application/json' \
  -d '{"text":"AT","newline":true,"as_client":"agent"}'

# hex
curl -s -X POST http://127.0.0.1:8787/v1/write \
  -H 'content-type: application/json' \
  -d '{"hex":"41 54 0d 0a","as_client":"agent"}'
```

### Lock

```bash
curl -s -X POST http://127.0.0.1:8787/v1/lock \
  -H 'content-type: application/json' \
  -d '{"as_client":"agent"}'

curl -s -X DELETE http://127.0.0.1:8787/v1/lock
```

### WebSocket

```text
ws://127.0.0.1:8787/v1/stream
```

- Server → client: binary frames (device RX), history on connect when configured  
- Client → server: text or binary becomes TX (subject to policy / lock)

### Example: Python agent sketch

```python
import json, urllib.request, websocket  # pip install websocket-client

# write a command
req = urllib.request.Request(
    "http://127.0.0.1:8787/v1/write",
    data=json.dumps({"text": "status", "newline": True}).encode(),
    headers={"content-type": "application/json"},
    method="POST",
)
print(urllib.request.urlopen(req).read().decode())

# stream RX
ws = websocket.create_connection("ws://127.0.0.1:8787/v1/stream")
while True:
    print(ws.recv())
```

---

## TX policies

| Mode | Behavior | When to use |
|------|----------|-------------|
| `queue_by_line` **(default)** | Buffer until `\n`, then send whole line | Text / AT / CLI devices |
| `queue_by_frame` | Buffer until delimiter byte | Simple framed binary |
| `exclusive` | Must hold write lock to TX | Flash / dangerous sessions |
| `primary_wins` | Prefer configured `primary` client | Human-in-the-loop priority |

**Write lock** (any mode): while held, only the lock owner may TX.  
Lease expires after `write_lock_ms`, or on client disconnect / explicit release.

**Slow clients** (`tx.slow_client`): `drop_oldest` (default) keeps the real port realtime; never block the device reader.

---

## Unix PTY (macOS / Linux)

Enable in config:

```toml
[[clients]]
type = "pty"
name = "ui"
link = "/tmp/ohmyserial-ui"
can_write = true
can_read = true
```

Then open `/tmp/ohmyserial-ui` in your favorite terminal (minicom, screen, Serial Studio, …).

> Real baud rate / framing come from the hub `[real]` section.  
> Some apps may fail ioctl baud on PTY — that’s a host-tool limitation; data path still works.

---

## Windows notes

| You need | Use |
|----------|-----|
| Agent / scripts | HTTP + WebSocket ✅ |
| Simple stream | TCP `127.0.0.1:8788` ✅ |
| Real device | `path = "COM3"` ✅ |
| App that **only** lists COM ports | Not built-in yet — use TCP if possible, or external COM bridge (com0com, etc.) |

`type = "pty"` is rejected on Windows by design (no Unix PTY).

---

## Security

- Default binds are **`127.0.0.1`** — writing the serial port is equivalent to touching hardware.
- Do **not** expose `0.0.0.0` on untrusted networks without auth (auth is not in MVP).
- Session logs may contain secrets from the device stream — treat log files carefully.

---

## Project layout

```text
oh_myserial/
├── POSITIONING.md          # product & architecture SSOT
├── ohmyserial.example.toml # sample config
├── src/
│   ├── main.rs             # CLI
│   ├── lib.rs
│   ├── hub.rs              # wiring
│   ├── broker.rs           # RX fan-out / TX admit
│   ├── serial.rs           # real + mock port
│   ├── policy.rs           # TX modes & locks
│   ├── config.rs           # TOML schema
│   ├── observe.rs          # session log
│   └── client/             # tcp, http/ws, pty
└── tests/                  # integration tests
```

---

## Development

```bash
# unit + integration tests
cargo test

# run example mock hub
cargo run -- run -c ohmyserial.example.toml

# format / lint (optional)
cargo fmt
cargo clippy
```

CI builds/tests on **Ubuntu, macOS, and Windows** (see [`.github/workflows/ci.yml`](./.github/workflows/ci.yml)).

---

## Roadmap

| Phase | Focus |
|-------|--------|
| ✅ MVP | Hub core, TX policy, TCP, HTTP/WS, Unix PTY, mock, logs, CLI |
| 🔜 Next | Multi-port profiles, richer history, Windows COM bridge docs, hardening |
| 🧭 Later | RFC2217, record/replay, light web monitor, metrics export |

Not planned as core: proprietary GUI suite, cloud accounts, kernel virtual-COM driver (unless demand is clear).

---

## FAQ

**Q: Can two writers send at the same time?**  
A: Bytes are never silently interleaved. Default mode queues **complete lines**; locks can grant exclusive windows.

**Q: Does the agent need a virtual COM port?**  
A: No. Prefer WebSocket + HTTP — more reliable for automation.

**Q: Why is my host app’s baud setting ignored on PTY?**  
A: The real port is configured by the hub. PTY is a software endpoint.

**Q: Is this a serial sniffer?**  
A: It can log traffic, but the primary goal is **shared interactive access**, not passive capture only.

---

## Contributing

Issues and PRs welcome:  
https://github.com/tianrking/oh_myserial

Please keep changes aligned with [`POSITIONING.md`](./POSITIONING.md) (small, cross-platform, agent-friendly).

---

## License

[MIT](./LICENSE) © ohmyserial contributors

---

<p align="center">
  <sub>One port. Many clients. Zero fights.</sub>
</p>
