# ohmyserial Web Console

The `web/` directory contains the React + Vite + TypeScript console embedded
into the Rust binary as `web/dist`. It connects to the hub's HTTP and
WebSocket endpoints; it does not open a serial device directly.

## Run the embedded console

Build the web assets first, then rebuild the Rust binary so `rust-embed` picks
up the new `dist` files:

```bash
cd web
npm ci
npm run build
cd ..
cargo build --release
./target/release/ohmyserial share mock:demo --ui
```

Open `http://127.0.0.1:8787/`. For a real device, replace `mock:demo` with a
serial path such as `/dev/ttyUSB0`, `/dev/cu.usbmodemXXXX`, or `COM3` and set
the line parameters before opening the device:

```bash
ohmyserial share COM3 --baud 115200 --data-bits 8 --parity none \
  --stop-bits 1 --flow-control none --ui
```

The browser console does not silently reconfigure an already-open UART. Baud,
data bits, parity, stop bits, and flow control remain Hub CLI/TOML settings.

## Console capabilities

- Hub host/port connection profiles stored in the current browser; Bearer
  tokens remain memory-only.
- Text and Hex writes with `none`, `LF`, `CR`, or `CRLF` endings and
  Ctrl/⌘+Enter.
- Browser-local quick commands and bounded timed sending (50 ms minimum).
- Optional SUM8, XOR8, CRC16-Modbus, and CRC16-CCITT Hex preprocessing with a
  visible wire preview.
- Pause/auto-scroll, timestamp toggle, text/Hex/both log modes, and export.
- RawData, FireWater CSV, and JustFloat little-endian parsing across chunk
  boundaries, plus a bounded SVG waveform view.
- NMEA 0183 checksum validation, SLIP and COBS frame decoding, and bounded
  Modbus RTU shape/CRC inspection across WebSocket chunks.
- Event-ledger Actor/Epoch/Hex filters, complete NDJSON export, and a
  Prometheus metrics panel with raw `.prom` download.
- DTR/RTS/BREAK controls through `POST /v1/control`; the Hub must grant
  `api.can_control` and the page must hold the opaque write lease.

All writes use `POST /v1/write`, so the Hub's TX policy, lease checks, size
limits, and host-side write confirmation remain in force. The UI is therefore
an observer/client of the Hub rather than a second serial owner.

## Local hot-reload development

Run the hub and Vite separately:

```bash
# terminal 1
cargo run --release -- share mock:demo --pty 2

# terminal 2
cd web
npm ci
npm run dev
```

Vite normally serves `http://localhost:5173`; the console connects to the Hub
at `127.0.0.1:8787` by default. Use `npm run build` to validate the embedded
asset before committing.

## Protocol and security references

- [`PROTOCOL.zh-TW.md`](./PROTOCOL.zh-TW.md) — HTTP/WS contract and browser
  mapping (Traditional Chinese).
- [`../EVENTS.md`](../EVENTS.md) — canonical event ledger and event stream.
- [`../WORKFLOWS.md`](../WORKFLOWS.md) — bounded Agent workflow contract.
- [`README.zh-TW.md`](./README.zh-TW.md) — Traditional Chinese UI guide.

Plain HTTP/WS/TCP listeners are loopback-only. For remote use, follow the Hub
documentation for an SSH tunnel or TLS reverse proxy; do not put API tokens in
URLs.

## Checks

```bash
npm run build
npm run lint
```
