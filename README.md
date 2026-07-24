# oh_myserial

**Cross-platform open-source serial hub for humans and agents.**

One real serial port. Many safe clients. Zero silent TX fights.

> Product positioning & architecture: see [`POSITIONING.md`](./POSITIONING.md).

## Why

A hardware UART can only be opened by one process. That blocks the usual workflow:

- you want a traditional serial terminal / host app open, **and**
- an AI agent / script also needs the same stream.

`ohmyserial` exclusively owns the real port, fans out RX, and arbitrates TX.

## Features (MVP)

| Feature | Notes |
|---------|--------|
| Real serial open/close | path, baud, data/parity/stop, flow |
| RX fan-out | all readable clients get device data |
| TX arbitration | default `queue_by_line`; write-lock lease |
| TCP raw stream | scripts / host tools |
| HTTP + WebSocket API | agent-first (`/v1/status`, `/v1/write`, `/v1/stream`, …) |
| Unix PTY | macOS/Linux virtual serial symlink for classic tools |
| Session log | console + optional file |
| Reconnect | optional auto-reopen of real port |
| Mock port | `path = "mock:demo"` loopback without hardware |
| Platforms | Windows / Linux / macOS (x64 & arm64) |

Windows note: native virtual COM needs a driver; v1 uses TCP/WS. Classic COM-only apps can bridge via com0com later.

## Quick start

```bash
# build
cargo build --release

# sample config
./target/release/ohmyserial init -o ohmyserial.toml

# run (mock loopback — no hardware)
./target/release/ohmyserial run -c ohmyserial.toml

# list machine serial ports
./target/release/ohmyserial list-ports
```

### Point at real hardware

```toml
[real]
path = "/dev/tty.usbmodem14101"   # or COM3 on Windows
baud = 115200
```

### Agent HTTP API

```bash
curl -s http://127.0.0.1:8787/v1/health
curl -s http://127.0.0.1:8787/v1/status | jq .
curl -s -X POST http://127.0.0.1:8787/v1/write \
  -H 'content-type: application/json' \
  -d '{"text":"AT","newline":true}'

# write lock
curl -s -X POST http://127.0.0.1:8787/v1/lock -H 'content-type: application/json' -d '{"as_client":"agent"}'
curl -s -X DELETE http://127.0.0.1:8787/v1/lock
```

WebSocket binary/text stream: `ws://127.0.0.1:8787/v1/stream`

### TCP client

```bash
# another terminal
nc 127.0.0.1 8788
```

### Unix PTY (macOS/Linux)

Uncomment in config:

```toml
[[clients]]
type = "pty"
name = "ui"
link = "/tmp/ohmyserial-ui"
can_write = true
can_read = true
```

Then open `/tmp/ohmyserial-ui` in your serial terminal (baud settings on the PTY may be ignored; real baud is from hub config).

## TX policies

| Mode | Behavior |
|------|----------|
| `queue_by_line` (default) | Buffer until `\n`, then serialize to device |
| `queue_by_frame` | Same with configurable delimiter byte |
| `exclusive` | Requires write lock (`POST /v1/lock`) |
| `primary_wins` | Prefer configured primary client |

Active write lock always blocks other writers until expiry or release.

## Config

See [`ohmyserial.example.toml`](./ohmyserial.example.toml).

## Development

```bash
cargo test
cargo run -- run -c ohmyserial.example.toml
```

## License

MIT — see [`LICENSE`](./LICENSE).
