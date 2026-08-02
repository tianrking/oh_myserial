import assert from "node:assert/strict";
import test from "node:test";

import {
  parseCobs,
  parseFireWater,
  parseJustFloat,
  parseModbusRtu,
  parseNmea0183,
  parseSlip,
} from "../src/protocolUtils.ts";

function bytes(...values: number[]): Uint8Array {
  return new Uint8Array(values);
}

function nmeaSentence(body: string, checksumOverride?: string): string {
  let checksum = 0;
  for (const character of body) checksum ^= character.charCodeAt(0);
  return `$${body}*${checksumOverride ?? checksum.toString(16).padStart(2, "0").toUpperCase()}\r\n`;
}

test("FireWater keeps partial UTF-8 text across chunks", () => {
  const first = parseFireWater("imu:1,2\npartial");
  assert.deepEqual(first.samples, [{ label: "imu", values: [1, 2] }]);
  const second = parseFireWater(`${first.remainder},3\n`);
  assert.deepEqual(second.samples, [{ values: [3] }]);
  assert.equal(second.remainder, "");
});

test("JustFloat decodes little-endian values and preserves split tails", () => {
  const payload = new Uint8Array(8);
  const view = new DataView(payload.buffer);
  view.setFloat32(0, 1.5, true);
  view.setFloat32(4, -2.25, true);
  const tail = bytes(0x00, 0x00, 0x80, 0x7f);
  const first = parseJustFloat(new Uint8Array(), new Uint8Array([...payload, ...tail.slice(0, 2)]));
  assert.deepEqual(first.samples, []);
  const second = parseJustFloat(first.remainder, tail.slice(2));
  assert.equal(second.samples.length, 1);
  assert.ok(Math.abs(second.samples[0].values[0] - 1.5) < 1e-6);
  assert.ok(Math.abs(second.samples[0].values[1] + 2.25) < 1e-6);
});

test("NMEA verifies checksums and handles line fragments", () => {
  const valid = nmeaSentence("GPGLL,4916.45,N,12311.12,W,225444,A");
  const first = parseNmea0183("", valid.slice(0, 9));
  assert.equal(first.frames.length, 0);
  const second = parseNmea0183(first.remainder, valid.slice(9));
  assert.equal(second.frames.length, 1);
  assert.equal(second.frames[0].valid, true);
  assert.equal(second.frames[0].fields?.checksum, "ok");

  const invalid = parseNmea0183("", nmeaSentence("GPGLL,broken", "00"));
  assert.equal(invalid.frames[0].valid, false);
  assert.equal(invalid.frames[0].fields?.checksum, "mismatch");
});

test("SLIP decodes escaped END and ESC bytes while retaining a partial frame", () => {
  const encoded = bytes(0xc0, 0x01, 0xdb, 0xdc, 0xdb, 0xdd, 0xc0, 0x02);
  const first = parseSlip(new Uint8Array(), encoded.slice(0, 4));
  assert.equal(first.frames.length, 0);
  const second = parseSlip(first.remainder, encoded.slice(4));
  assert.equal(second.frames.length, 1);
  assert.deepEqual([...second.frames[0].bytes], [0x01, 0xc0, 0xdb]);
  assert.equal(second.frames[0].valid, true);
  assert.deepEqual([...second.remainder], [0x02]);
});

test("COBS decodes zero-containing payloads across chunks", () => {
  const encoded = bytes(0x02, 0x11, 0x02, 0x22, 0x00, 0x01, 0x01, 0x00);
  const first = parseCobs(new Uint8Array(), encoded.slice(0, 3));
  assert.equal(first.frames.length, 0);
  const second = parseCobs(first.remainder, encoded.slice(3));
  assert.equal(second.frames.length, 2);
  assert.deepEqual([...second.frames[0].bytes], [0x11, 0x00, 0x22]);
  assert.deepEqual([...second.frames[1].bytes], [0x00]);
  assert.equal(second.frames.every((frame) => frame.valid), true);
});

test("Modbus RTU resynchronizes after noise and validates CRC", () => {
  const frame = bytes(0x01, 0x03, 0x00, 0x00, 0x00, 0x02, 0xc4, 0x0b);
  const partial = parseModbusRtu(new Uint8Array(), new Uint8Array([0xff, ...frame.slice(0, 4)]));
  assert.equal(partial.frames.length, 0);
  const complete = parseModbusRtu(partial.remainder, frame.slice(4));
  assert.equal(complete.frames.length, 1);
  assert.equal(complete.frames[0].valid, true);
  assert.equal(complete.frames[0].fields?.unit, 1);
  assert.equal(complete.frames[0].fields?.function, "0x03");

  const invalid = parseModbusRtu(new Uint8Array(), bytes(1, 3, 0, 0, 0, 2, 0, 0));
  assert.equal(invalid.frames.length, 1);
  assert.equal(invalid.frames[0].valid, false);
});

test("protocol remainders stay bounded under an unrecognized stream", () => {
  const noisy = new Uint8Array(70 * 1024).fill(0xff);
  const parsed = parseModbusRtu(new Uint8Array(), noisy);
  assert.equal(parsed.frames.length, 0);
  assert.ok(parsed.remainder.length <= 64 * 1024);
});

test("all binary analyzers remain total over arbitrary chunk boundaries", () => {
  let seed = 0x12345678;
  let slip = new Uint8Array();
  let cobs = new Uint8Array();
  let modbus = new Uint8Array();
  let justFloat = new Uint8Array();
  let nmea = "";
  for (let iteration = 0; iteration < 2_000; iteration += 1) {
    seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0;
    const length = 1 + (seed % 31);
    const incoming = new Uint8Array(length);
    for (let index = 0; index < incoming.length; index += 1) {
      seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0;
      incoming[index] = seed & 0xff;
    }
    slip = parseSlip(slip, incoming).remainder;
    cobs = parseCobs(cobs, incoming).remainder;
    modbus = parseModbusRtu(modbus, incoming).remainder;
    justFloat = parseJustFloat(justFloat, incoming).remainder;
    nmea = parseNmea0183(nmea, new TextDecoder().decode(incoming)).remainder;
    assert.ok(slip.length <= 64 * 1024);
    assert.ok(cobs.length <= 64 * 1024);
    assert.ok(modbus.length <= 64 * 1024);
    assert.ok(justFloat.length <= 64 * 1024);
    assert.ok(nmea.length <= 64 * 1024);
  }
});
