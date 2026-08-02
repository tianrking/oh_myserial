import assert from "node:assert/strict";
import test from "node:test";

import { appendHexChecksum } from "../src/checksumUtils.ts";

test("checksum helpers normalize hex and append the expected byte order", () => {
  assert.equal(appendHexChecksum("01 02 03", "none"), "01 02 03");
  assert.equal(appendHexChecksum("01,02,03", "sum8"), "01 02 03 06");
  assert.equal(appendHexChecksum("01 02 03", "xor8"), "01 02 03 00");
  assert.equal(appendHexChecksum("01 03 00 00 00 02", "crc16-modbus"), "01 03 00 00 00 02 c4 0b");
  assert.equal(appendHexChecksum("12 34", "crc16-ccitt"), "12 34 0e c9");
});

test("checksum helpers fail closed on malformed or empty input", () => {
  assert.throws(() => appendHexChecksum("abc", "none"));
  assert.throws(() => appendHexChecksum("not-hex", "none"));
  assert.throws(() => appendHexChecksum("", "sum8"));
});
