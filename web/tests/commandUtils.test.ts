import assert from "node:assert/strict";
import test from "node:test";

import { appendLineEnding, loadConnectionProfiles, loadQuickCommands } from "../src/commandUtils.ts";

test("line ending helper is explicit and does not mutate payload", () => {
  assert.equal(appendLineEnding("ping", "none"), "ping");
  assert.equal(appendLineEnding("ping", "lf"), "ping\n");
  assert.equal(appendLineEnding("ping", "cr"), "ping\r");
  assert.equal(appendLineEnding("ping", "crlf"), "ping\r\n");
});

test("storage loaders ignore malformed entries and preserve valid entries", () => {
  const values = new Map<string, string>([
    [
      "ohmyserial.web.quickCommands",
      JSON.stringify([
        { id: "ok", name: "Ping", mode: "text", payload: "ping", lineEnding: "lf" },
        { id: "bad", name: "", mode: "text", payload: "x", lineEnding: "lf" },
        { id: "bad-mode", name: "X", mode: "binary", payload: "x", lineEnding: "lf" },
      ]),
    ],
    [
      "ohmyserial.web.connectionProfiles",
      JSON.stringify([
        { id: "profile", name: "Local", host: "127.0.0.1", port: 8787 },
        { id: "bad-port", name: "Bad", host: "127.0.0.1", port: 70000 },
      ]),
    ],
  ]);
  const originalWindow = globalThis.window;
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    value: { localStorage: { getItem: (key: string) => values.get(key) ?? null } },
  });
  try {
    assert.deepEqual(loadQuickCommands(), [
      { id: "ok", name: "Ping", mode: "text", payload: "ping", lineEnding: "lf" },
    ]);
    assert.deepEqual(loadConnectionProfiles(), [
      { id: "profile", name: "Local", host: "127.0.0.1", port: 8787 },
    ]);
  } finally {
    if (originalWindow === undefined) delete (globalThis as { window?: unknown }).window;
    else Object.defineProperty(globalThis, "window", { configurable: true, value: originalWindow });
  }
});
