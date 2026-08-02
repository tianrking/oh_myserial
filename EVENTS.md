# Event ledger and safe replay

ohmyserial maintains a canonical, versioned event ledger alongside its human-readable session log. The ledger is intended for byte-exact investigation, agent evidence, and deterministic offline replay. It does not turn a serial connection into a cryptographically attested or lossless hardware trace.

This document specifies the v1 envelope, retention and persistence behavior, event APIs, and the isolated replay command.

## Quick start

The bounded in-memory ledger is always enabled. Disk persistence is opt-in:

```toml
[ledger]
memory_events = 16384
memory_bytes = 33554432
stream_capacity = 1024
# directory = "./ohmyserial-ledger"
rotate_bytes = 67108864
fsync_each_event = false
```

Start a hub and inspect its current session:

```bash
curl -s http://127.0.0.1:8787/v1/events/status
curl -s 'http://127.0.0.1:8787/v1/events?after_seq=0&limit=100'
curl -s http://127.0.0.1:8787/v1/events/export > events.ndjson
```

When `api.token_env` is configured, add `Authorization: Bearer <token>` to these requests. Every event route also requires that API listener's `can_read` permission.

## Evidence boundary

The ledger records what the running hub can prove:

- An `rx` event means the ohmyserial process successfully received those bytes from its serial read call.
- A `tx` event is created only after the serial-owner thread completed host-side `write_all` and `flush` for those bytes.
- A `gap` event states a known evidence or delivery limitation without inventing missing bytes.
- Connection and control events record relevant hub state transitions and actions.

The ledger cannot prove events outside that boundary:

- A UART, USB adapter, kernel driver, or OS buffer may lose bytes before the process reads them. Some drivers do not report an overrun, so the ledger cannot always create a corresponding gap.
- Host-side write success does not prove electrical transmission, device parsing, command execution, or a device protocol acknowledgement.
- Wall-clock timestamps can move when the system clock changes. Use `seq` for canonical order and `mono_us` for intervals inside one process session.
- `mock:demo` verifies the hub, API, policies, ledger, and replay path. It is not hardware-in-the-loop verification of a real driver, USB/UART timing, control lines, or target device.

## Canonical v1 envelope

Each event is one JSON object:

```json
{
  "schema": "ohmyserial.event",
  "version": 1,
  "session_id": "4c83a281-82c3-4b6e-84c1-ded149087fc1",
  "seq": 42,
  "ts_utc": "2026-08-01T08:15:30.123456Z",
  "mono_us": 3187654,
  "port_id": "default",
  "connection_epoch": 3,
  "type": "tx",
  "payload": {
    "actor": "agent",
    "client_id": "0ca5f377-6201-41f6-920e-ecf3568d3e73",
    "data_base64": "QVQNCg==",
    "len": 4
  }
}
```

| Field | Contract |
|-------|----------|
| `schema` | Always `ohmyserial.event` for this document. |
| `version` | `1`. Consumers must reject unsupported versions rather than guess. |
| `session_id` | UUID for one ledger session. A normal hub start creates a new session. |
| `seq` | Session-scoped canonical sequence, starting at 1 and increasing by one. |
| `ts_utc` | RFC 3339 UTC observation timestamp with microsecond precision. |
| `mono_us` | Microseconds since this ledger process started; useful for durations and replay pacing. |
| `port_id` | `default` in v1. The field reserves a stable identity slot for later multi-port support. |
| `connection_epoch` | Serial connection generation. A successful disconnected-to-connected transition increments it. |
| `type` | `rx`, `tx`, `connection`, `control`, or `gap`. |
| `payload` | Type-specific object described below. |

`seq` orders all event types in one session. `connection_epoch` separates device connection generations; it is not a replacement for `seq`.

### Byte encoding

All byte payloads use standard, padded Base64 and include the decoded length:

```json
{ "data_base64": "+/8=", "len": 2 }
```

This is RFC 4648 standard Base64, not the URL-safe alphabet. Readers must decode `data_base64` and verify that its byte length equals `len`. Text rendering is a presentation choice and is never the canonical byte representation.

## Event types

### `rx`

Bytes successfully returned to ohmyserial by the serial read path:

```json
{
  "type": "rx",
  "payload": { "data_base64": "T0sNCg==", "len": 4 }
}
```

The canonical `rx` event is appended before fan-out to individual clients. A later per-client delivery failure therefore does not erase or alter the original `rx` evidence.

### `tx`

Bytes for which the serial-owner thread completed host-side `write_all` and `flush`:

```json
{
  "type": "tx",
  "payload": {
    "actor": "agent",
    "client_id": "0ca5f377-6201-41f6-920e-ecf3568d3e73",
    "data_base64": "QVQNCg==",
    "len": 4
  }
}
```

`actor` is the configured or supplied audit label, not necessarily an authenticated human identity. `client_id` identifies the exact live client registration when available. Lease bearer tokens and API bearer secrets have no field in the schema and are never intentionally written to the ledger.

A rejected write has no `tx` event. A write rejected before a host write attempt can produce a `control` event such as `write_rejected`; a failed or timed-out host write attempt produces a `gap` with an uncertain TX outcome.

### `connection`

Serial lifecycle evidence:

```json
{
  "type": "connection",
  "payload": {
    "state": "connected",
    "path": "/dev/ttyUSB0",
    "baud": 115200,
    "detail": "open"
  }
}
```

`state` is one of `connected`, `disconnected`, `reconnecting`, or `open_failed`. `detail` is optional diagnostic text.

### `control`

A hub control-plane or lifecycle action:

```json
{
  "type": "control",
  "payload": {
    "actor": "agent",
    "name": "lease_acquired",
    "value": "ttl_ms=3000"
  }
}
```

`actor` and `value` are optional. Control names include hub lifecycle, client join/leave, lease lifecycle, and pre-write rejection events. Consumers should accept new control names within schema v1.

### `gap`

An explicit limitation in observation, write outcome, client delivery, or durable persistence:

```json
{
  "type": "gap",
  "payload": {
    "scope": "tx_outcome",
    "certainty": "partial_or_unknown",
    "reason": "serial write timed out",
    "data_base64": "QVQNCg==",
    "len": 4,
    "actor": "agent",
    "client_ids": ["0ca5f377-6201-41f6-920e-ecf3568d3e73"]
  }
}
```

The optional byte fields are flattened into the payload. `actor` is optional and `client_ids` defaults to an empty list.

| `scope` | Meaning |
|---------|---------|
| `rx_observation` | The serial read path could not prove continuous RX observation. The hub does not invent a missing byte range. |
| `tx_outcome` | A host write was attempted, but its result may be partial or unknown. Do not blindly retry. |
| `client_delivery` | Canonical RX existed, but one or more slow clients did not receive all of it. |
| `persistence` | The event remained available in memory/live delivery, but durable storage failed or degraded. |

| `certainty` | Meaning |
|-------------|---------|
| `unknown` | The affected external observation cannot be quantified. |
| `partial_or_unknown` | Some or all bytes may have crossed the host write boundary. |
| `not_delivered` | The named delivery or persistence destination did not receive the data. |

A `gap` consumes a normal canonical `seq`; it does not mean the ledger's own sequence numbering skipped a value.

## Memory retention

The ledger always keeps a bounded ring controlled by both `memory_events` and `memory_bytes`. When either bound is exceeded, the oldest retained events are evicted. This does not delete optional disk segments.

`GET /v1/events/status` reports:

- `session_id`, `newest_seq`, and `oldest_available_seq`
- `retained_events`, `retained_bytes`, and `evicted_events`
- persistence state: `disabled`, `active`, `degraded`, or `sealed`
- the persistence directory and last persistence error when present
- crash-recovery reports when startup found stale sessions

If an HTTP query asks for an evicted cursor and disk persistence is disabled, the server returns `410 Gone` with an incomplete page and the available range. Live event WebSocket subscribers are independently bounded by `stream_capacity`.

## Optional hashed NDJSON persistence

Set `ledger.directory` to enable an append-only segmented store. Each segment is NDJSON containing:

1. one `ohmyserial.segment` v1 header;
2. zero or more wrapped canonical event records;
3. one footer with the event count, sequence range, and SHA-256 content hash.

The next segment links to the SHA-256 hash of the complete previous segment. Active files end in `.open`; checkpointed, rotated, recovered, or normally closed files end in `.omslog`:

```text
session-<uuid>-segment-00000000000000000000.omslog
session-<uuid>-segment-00000000000000000001.open
session-<uuid>.lock
```

Rotation happens before a new event would exceed `rotate_bytes`, or at the internal 100,000-event segment ceiling. A checkpoint seals the current non-empty `.open` segment without ending the session; the next append lazily creates a linked segment. API queries that need evicted disk history and `GET /v1/events/export` checkpoint the active session so independent readers see a sealed prefix.

By default, each event is flushed from the userspace writer, but `fsync_each_event = false` does not force every event through the OS storage cache. Set it to `true` for a `sync_data` on every event, accepting the latency and write-amplification cost. Segment sealing performs a full sync before rename.

### Hash trust model

The SHA-256 values detect corruption and inconsistent segment chains. They are **not signatures**:

- there is no secret key, signer identity, trusted timestamp, or hardware attestation;
- an attacker who can rewrite the files can also recompute the hashes;
- copying only the flat API export removes the segment wrappers and their hash chain.

Treat a verified hash chain as self-consistent local evidence, not proof of origin, non-repudiation, or device behavior.

### Crash recovery

On startup with persistence enabled, a new hub session scans the directory for stale `.open` segments. Recovery:

- takes an advisory per-session lock and skips sessions still held by a live process;
- validates the header, schema, session, sequence continuity, and complete NDJSON lines;
- preserves the original source by renaming it to a unique `.recovery-source-<uuid>` file;
- seals the complete valid prefix into a new `.omslog` segment;
- reports discarded tail byte count and the first parse/ordering error;
- reports ambiguous or malformed cases without deleting their source files.

Recovery is conservative and does not silently repair invented data.

### Retention is operator-owned

ohmyserial does **not** automatically delete old sessions or sealed segments. `rotate_bytes` limits individual segment size; it is not a total-disk quota. Monitor the persistence directory and archive or remove old, inactive sessions according to your own retention policy. Do not modify files belonging to an active locked session.

If persistence fails during a live session, the ledger becomes `degraded`. New canonical events continue in the bounded memory ring and live stream, and the first failure also creates an in-memory persistence `gap`. Because a physical TX may already have succeeded when its evidence write fails, callers receive an explicit error and must not blindly retry.

## HTTP event API

All routes below use the normal API bearer, Host/Origin protections, and `can_read` permission.

### `GET /v1/events/status`

Returns the current `LedgerStatus` object described under memory retention.

### `GET /v1/events`

Returns a cursor page:

```bash
curl -s 'http://127.0.0.1:8787/v1/events?after_seq=41&limit=100&type=rx,tx&connection_epoch=3&actor=agent&contains_hex=0d0a'
```

| Query parameter | Semantics |
|-----------------|-----------|
| `after_seq` | Exclusive cursor; default `0`. |
| `through_seq` | Optional inclusive upper sequence bound. |
| `limit` | Matching events per page, `1..1000`; default `1000`. |
| `type` | Comma-separated set of `rx`, `tx`, `connection`, `control`, `gap`. |
| `connection_epoch` | Exact epoch match. |
| `actor` | Exact actor match for TX, control, and gap events. |
| `contains_hex` | Match decoded bytes in RX, TX, or byte-carrying gap events. |

Successful responses have this shape:

```json
{
  "ok": true,
  "session_id": "4c83a281-82c3-4b6e-84c1-ded149087fc1",
  "page": {
    "events": [],
    "incomplete": false,
    "oldest_available_seq": 1,
    "newest_seq": 42,
    "next_after_seq": 42,
    "has_more": false
  }
}
```

Continue with `after_seq=<page.next_after_seq>`. The cursor advances over examined nonmatching events as well, so filtering cannot trap a client on one page. If the memory ring no longer contains the cursor and persistence is active, the server checkpoints and queries the verified sealed session. Without an authoritative prefix it returns `410 Gone`; storage/checkpoint failure returns `503 Service Unavailable`.

### `GET /v1/events/export`

Returns `application/x-ndjson`, one canonical `EventEnvelope` per line. With persistence enabled it checkpoints and exports the verified sealed session. In memory-only mode a complete export is available only while the full session still fits in the ring; otherwise the route returns `410 Gone`.

The export is convenient data interchange, but it is not an `.omslog` segment and carries no segment hash chain. The replay CLI intentionally accepts verified `.omslog` input, not this flat export.

### `WS /v1/events/stream`

This is a read-only stream of canonical envelopes as JSON **text** frames:

```text
ws://127.0.0.1:8787/v1/events/stream?after_seq=41&type=rx,tx
```

It takes the same cursor, upper-bound, and filter parameters as the query endpoint (the HTTP page `limit` is not used). The server subscribes first, sends the in-memory snapshot after `after_seq`, then continues with live events without a query/subscribe race. If `through_seq` is set, it closes after reaching that bound.

The event stream does not perform disk backfill during upgrade. If its requested cursor has left the ring, it returns `410 Gone`; catch up with paginated `GET /v1/events`, then reconnect using the returned `next_after_seq`.

If this particular WebSocket receiver falls behind `stream_capacity`, the server sends one noncanonical diagnostic text frame and closes:

```json
{
  "schema": "ohmyserial.stream-gap",
  "version": 1,
  "after_seq": 41,
  "earliest_available_seq": 57,
  "latest_seq": 93
}
```

`ohmyserial.stream-gap` is a subscriber transport warning, not a canonical event and does not consume a ledger `seq`. Reconnect through HTTP catch-up.

Do not confuse the two WebSocket paths:

| Path | Frames | Direction | Purpose |
|------|--------|-----------|---------|
| `/v1/stream` | Binary RX plus TX/error frames | Bidirectional | Low-latency raw serial data plane. |
| `/v1/events/stream` | JSON text envelopes | Read-only | Ordered, typed evidence stream. |

## Safe offline replay

Replay loads and verifies sealed ledger data before emitting any event:

```bash
# One verified segment
ohmyserial replay ./ohmyserial-ledger/session-<uuid>-segment-00000000000000000000.omslog

# A directory containing sealed segments for exactly one session
ohmyserial replay ./one-session-directory --mode original --speed 2

# Human-controlled batches
ohmyserial replay ./one-session-directory --mode manual --step 10
```

If a persistence root contains more than one session, copy or select only the desired session's sealed `.omslog` files into a separate read-only directory before replaying the complete session.

| Mode | Behavior |
|------|----------|
| `immediate` | Emit every verified envelope without added delay. This is the default. |
| `original` | Preserve recorded `mono_us` intervals, divided by `--speed` (`0.01` through `100`). |
| `manual` | Wait for Enter, then emit up to `--step` events; `q` stops. |

The command writes each original envelope unchanged as one JSON line on stdout; verification/session information and manual prompts go to stderr. Loading rejects bad schema, hash mismatch, mixed sessions, non-contiguous sequence numbers, and a regressing monotonic clock.

Replay is structurally isolated from the live data plane: it does not construct a `Broker`, open a serial device, enqueue TX, acquire a lease, or feed events back into `/v1/stream`. `tx` events are evidence to print, never commands to execute. There is no automatic retry or hidden device side effect.

## Consumer checklist

- Pin support to `schema = "ohmyserial.event"` and an understood `version`.
- Key cursors by `session_id`; never carry `after_seq` into a different session.
- Order by `seq`, use `connection_epoch` to split reconnect generations, and use `mono_us` for replay intervals.
- Decode standard padded Base64 and verify `len`.
- Treat every `gap` according to its scope and certainty.
- On HTTP `410` or `ohmyserial.stream-gap`, perform authoritative catch-up instead of assuming continuity.
- Never treat `actor` as proof of a human identity or hashes as signatures.
- Keep ledger files and exports protected: raw device traffic can contain credentials, keys, personal data, or proprietary firmware output.
- Use real hardware-in-the-loop tests before making claims about physical serial behavior.
