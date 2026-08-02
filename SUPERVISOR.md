# Multi-profile supervision

Run one process with independent serial hubs:

```bash
ohmyserial supervise -c board-a.toml -c board-b.toml
```

Each profile retains its own real serial owner, listeners, clients, policy, and
event ledger. The supervisor does not merge byte streams or leases.

Before opening anything it validates every profile and rejects collisions in:

- API, raw TCP, and dedicated WebSocket listen addresses;
- Unix PTY links;
- non-mock real paths and exact USB selectors;
- persisted ledger directories.

Startup is transactional at the process level: if a later profile fails to
bind/configure, already-started hubs are gracefully shut down before the error
is returned. Ctrl+C shuts down all profiles and seals each ledger. Remote
access remains per-profile through the existing loopback API plus SSH tunnel or
TLS reverse proxy; the supervisor does not add a plaintext network boundary.
