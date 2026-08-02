# Safe serial-owner handoff

Handoff is a bounded maintenance window for tools that must open the physical
serial device themselves. It is deliberately a control-plane operation, not a
second byte path through the hub.

## Contract

1. An API caller enables `api.can_control`, acquires the normal opaque write
   lease, and calls `POST /v1/handoff`.
2. The serial owner drains/rejects queued writes, closes the OS handle, and
   acknowledges only after the handle is released. The response returns the
   resolved device path, a random `handoff_token`, and the bounded TTL.
3. While the window is active, the broker rejects TX, new leases, and control
   lines. The old write lease is invalidated. The token is not written to the
   event ledger and is not returned by `/v1/status`.
4. The external tool opens the returned path for at most the TTL. It then calls
   `POST /v1/handoff/resume` with the token. If it does not, the owner resumes
   automatically at the TTL boundary and re-resolves USB identity before open.

Example:

```bash
LEASE_TOKEN="$(curl -s -X POST http://127.0.0.1:8787/v1/lock \
  -H content-type:application/json -d '{"as_client":"maintenance"}' \
  | jq -r '.lock.lease_token')"

HANDOFF="$(curl -s -X POST http://127.0.0.1:8787/v1/handoff \
  -H content-type:application/json \
  -d "{\"as_client\":\"maintenance\",\"lease_token\":\"$LEASE_TOKEN\",\"duration_ms\":30000}")"
PATH_TO_DEVICE="$(jq -r '.handoff.path' <<<"$HANDOFF")"
TOKEN="$(jq -r '.handoff.handoff_token' <<<"$HANDOFF")"
# Open PATH_TO_DEVICE in the maintenance tool, then close it.
curl -s -X POST http://127.0.0.1:8787/v1/handoff/resume \
  -H content-type:application/json -d "{\"handoff_token\":\"$TOKEN\"}"
```

The hub does not verify bytes written by the external tool and does not claim
device protocol acknowledgement. Handoff is unsupported in `mock:` mode, and
hardware-in-loop testing is required before relying on reset/bootloader timing.
