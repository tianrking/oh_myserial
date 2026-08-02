# Bounded device workflows

ohmyserial can run a small, linear workflow against the live broker. This is
an automation primitive for agents, not a general scripting engine.

## Contract

The JSON definition contains an `id`, optional display `name`, and a non-empty
`steps` list. Every step is one of:

```json
{
  "id": "identify-board",
  "steps": [
    { "op": "lease" },
    { "op": "send", "bytes": { "text": "ATI\r\n" } },
    { "op": "expect", "pattern": { "text": "OK" }, "timeout_ms": 2000, "capture": "reply" },
    { "op": "assert", "assertion": { "kind": "port_connected" } },
    { "op": "wait", "duration_ms": 50 }
  ]
}
```

`bytes` accepts exactly one of `{ "text": "..." }`, `{ "hex": "00 ff" }`,
or `{ "base64": "AP8=" }`. Hex accepts whitespace and uses byte pairs;
Base64 is standard padded Base64. The canonical ledger remains the source of
truth for bytes.

There are deliberately no loops, branches, retries, variables, arbitrary
expressions, network calls, or file operations. The service generates the
workflow actor (`workflow:<uuid>`); a definition cannot impersonate a client,
primary, or lease owner.

The default limits are 32 steps, 30 seconds total duration, 4096-byte expect
patterns, 64 KiB total captures, and 256 evidence items. A deployment can use a
smaller `WorkflowLimits` value, but cannot make a request exceed its configured
limits.

## HTTP API

`POST /v1/workflows/run` accepts:

```json
{
  "request_id": "board-2026-08-02-001",
  "lease_token": "optional opaque bearer returned by /v1/lock",
  "workflow": { "id": "identify-board", "steps": [] }
}
```

`request_id` is an idempotency key. A completed key returns the exact previous
result; a concurrent duplicate is rejected rather than running a second set of
side effects. The bearer is consumed only by the runtime and never appears in
the result or ledger evidence.

`lease` acquires a new broker lease (or renews the supplied token), and `send`
requires read/write permission plus the resulting lease. A send uses the
broker's confirmed host-write path once; the runner never silently retries it.
`expect` requires read permission and matches canonical RX events incrementally
across arbitrary read chunks. `client_delivery` gaps do not fail the matcher,
but RX observation gaps, cursor eviction/lag, disconnects, and connection-epoch
changes fail closed. `assert` supports only the explicit `port_connected` and
`connection_epoch` forms. `control` is schema-reserved but currently returns a
clear unavailable error until the serial-owner control channel is implemented.

The result includes a server-generated actor, final evidence cursor
(`session_id`, `port_id`, `connection_epoch`, `seq`, `byte_offset`), and bounded
per-step evidence with sequence ranges and optional Base64 capture. It contains
no lease token.

Cancellation and the total deadline stop waiting; no hidden background retry or
device write continues after cancellation. A runtime may still report a
host-side TX outcome gap separately if the serial owner had already accepted a
write before cancellation.

## Evidence boundary

Workflow evidence begins at the same boundary as `EVENTS.md`: RX bytes are what
the process successfully read, and a TX is host-side `write_all` + `flush`
success. It is not a proof of UART electrical delivery, driver behavior before
the read call, device parsing, or command acknowledgement. Use hardware-in-loop
tests before making physical claims.
