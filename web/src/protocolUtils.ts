export type StreamProtocol =
  | "raw"
  | "firewater"
  | "justfloat"
  | "nmea0183"
  | "slip"
  | "cobs"
  | "modbusrtu";

export type WaveSample = {
  values: number[];
  label?: string;
};

export type ProtocolFrame = {
  protocol: Exclude<StreamProtocol, "raw" | "firewater" | "justfloat">;
  summary: string;
  bytes: Uint8Array;
  valid?: boolean;
  fields?: Record<string, string | number | boolean>;
};

export const JUSTFLOAT_TAIL = new Uint8Array([0x00, 0x00, 0x80, 0x7f]);

const MAX_REMAINDER = 64 * 1024;

function findTail(bytes: Uint8Array): number {
  outer: for (let offset = 0; offset <= bytes.length - JUSTFLOAT_TAIL.length; offset += 1) {
    for (let index = 0; index < JUSTFLOAT_TAIL.length; index += 1) {
      if (bytes[offset + index] !== JUSTFLOAT_TAIL[index]) continue outer;
    }
    return offset;
  }
  return -1;
}

function concatBytes(left: Uint8Array, right: Uint8Array): Uint8Array {
  const joined = new Uint8Array(left.length + right.length);
  joined.set(left);
  joined.set(right, left.length);
  return joined;
}

function boundedRemainder(bytes: Uint8Array): Uint8Array {
  return bytes.length > MAX_REMAINDER ? bytes.slice(-MAX_REMAINDER) : bytes.slice();
}

/** Parse complete FireWater CSV frames from a UTF-8 text buffer. */
export function parseFireWater(buffer: string): { samples: WaveSample[]; remainder: string } {
  const samples: WaveSample[] = [];
  let consumed = 0;
  while (true) {
    const newline = buffer.indexOf("\n", consumed);
    if (newline < 0) break;
    const line = buffer.slice(consumed, newline).replace(/\r$/, "").trim();
    consumed = newline + 1;
    if (!line) continue;
    const separator = line.indexOf(":");
    const valuesText = separator >= 0 ? line.slice(separator + 1) : line;
    const values = valuesText
      .split(",")
      .map((value) => Number(value.trim()))
      .filter((value) => Number.isFinite(value));
    if (values.length === 0) continue;
    samples.push({
      values,
      ...(separator >= 0 && line.slice(0, separator).trim()
        ? { label: line.slice(0, separator).trim() }
        : {}),
    });
  }
  return { samples, remainder: buffer.slice(consumed) };
}

/** Parse one or more JustFloat frames across arbitrary WebSocket chunks. */
export function parseJustFloat(
  buffer: Uint8Array,
  incoming: Uint8Array,
): { samples: WaveSample[]; remainder: Uint8Array } {
  let bytes = concatBytes(buffer, incoming);
  const samples: WaveSample[] = [];
  while (true) {
    const tail = findTail(bytes);
    if (tail < 0) break;
    const payload = bytes.subarray(0, tail);
    if (payload.length > 0 && payload.length % 4 === 0) {
      const view = new DataView(payload.buffer, payload.byteOffset, payload.byteLength);
      const values: number[] = [];
      for (let offset = 0; offset < payload.length; offset += 4) {
        const value = view.getFloat32(offset, true);
        if (Number.isFinite(value)) values.push(value);
      }
      if (values.length > 0) samples.push({ values });
    }
    bytes = bytes.subarray(tail + JUSTFLOAT_TAIL.length);
  }
  return { samples, remainder: boundedRemainder(bytes) };
}

function frameBytes(text: string): Uint8Array {
  return new TextEncoder().encode(text);
}

/** Parse NMEA 0183 sentences and verify the optional XOR checksum. */
export function parseNmea0183(
  buffer: string,
  incoming: string,
): { frames: ProtocolFrame[]; remainder: string } {
  const text = `${buffer}${incoming}`;
  const frames: ProtocolFrame[] = [];
  let consumed = 0;
  while (true) {
    const newline = text.indexOf("\n", consumed);
    if (newline < 0) break;
    const line = text.slice(consumed, newline).replace(/\r$/, "").trim();
    consumed = newline + 1;
    if (!line) continue;
    const sentence = line.startsWith("$") ? line.slice(1) : line;
    const star = sentence.indexOf("*");
    const body = star >= 0 ? sentence.slice(0, star) : sentence;
    const checksumText = star >= 0 ? sentence.slice(star + 1, star + 3) : "";
    let computed = 0;
    for (const character of body) computed ^= character.charCodeAt(0);
    const expected = checksumText.length === 2 && /^[0-9a-f]{2}$/i.test(checksumText)
      ? Number.parseInt(checksumText, 16)
      : undefined;
    const valid = expected !== undefined && computed === expected;
    const kind = body.slice(0, 5) || "NMEA";
    const summary = expected === undefined
      ? `${kind} (checksum missing)`
      : `${kind} checksum ${valid ? "OK" : "FAIL"}`;
    frames.push({
      protocol: "nmea0183",
      summary,
      bytes: frameBytes(line),
      valid,
      fields: {
        sentence: kind,
        checksum: expected === undefined ? "missing" : valid ? "ok" : "mismatch",
      },
    });
  }
  return { frames, remainder: text.slice(consumed).slice(-MAX_REMAINDER) };
}

function decodeSlip(encoded: Uint8Array): Uint8Array | null {
  const decoded: number[] = [];
  for (let index = 0; index < encoded.length; index += 1) {
    const byte = encoded[index];
    if (byte === 0xdb) {
      if (index + 1 >= encoded.length) return null;
      const escaped = encoded[++index];
      if (escaped === 0xdc) decoded.push(0xc0);
      else if (escaped === 0xdd) decoded.push(0xdb);
      else return null;
    } else {
      decoded.push(byte);
    }
  }
  return new Uint8Array(decoded);
}

/** Parse SLIP (RFC 1055) frames. END (0xc0) delimits frames. */
export function parseSlip(
  buffer: Uint8Array,
  incoming: Uint8Array,
): { frames: ProtocolFrame[]; remainder: Uint8Array } {
  const bytes = concatBytes(buffer, incoming);
  const frames: ProtocolFrame[] = [];
  let start = 0;
  for (let index = 0; index < bytes.length; index += 1) {
    if (bytes[index] !== 0xc0) continue;
    const encoded = bytes.subarray(start, index);
    if (encoded.length > 0) {
      const decoded = decodeSlip(encoded);
      frames.push({
        protocol: "slip",
        summary: decoded ? `SLIP frame ${decoded.length} bytes` : "SLIP escape error",
        bytes: decoded ?? encoded.slice(),
        valid: decoded !== null,
      });
    }
    start = index + 1;
  }
  return { frames, remainder: boundedRemainder(bytes.subarray(start)) };
}

function decodeCobs(encoded: Uint8Array): Uint8Array | null {
  if (encoded.length === 0) return new Uint8Array();
  const decoded: number[] = [];
  let index = 0;
  while (index < encoded.length) {
    const code = encoded[index++];
    if (code === 0 || index + code - 1 > encoded.length) return null;
    for (let offset = 0; offset < code - 1; offset += 1) decoded.push(encoded[index++]);
    if (code < 0xff && index < encoded.length) decoded.push(0);
  }
  return new Uint8Array(decoded);
}

/** Parse COBS frames delimited by zero bytes. */
export function parseCobs(
  buffer: Uint8Array,
  incoming: Uint8Array,
): { frames: ProtocolFrame[]; remainder: Uint8Array } {
  const bytes = concatBytes(buffer, incoming);
  const frames: ProtocolFrame[] = [];
  let start = 0;
  for (let index = 0; index < bytes.length; index += 1) {
    if (bytes[index] !== 0) continue;
    const encoded = bytes.subarray(start, index);
    if (encoded.length > 0) {
      const decoded = decodeCobs(encoded);
      frames.push({
        protocol: "cobs",
        summary: decoded ? `COBS frame ${decoded.length} bytes` : "COBS code error",
        bytes: decoded ?? encoded.slice(),
        valid: decoded !== null,
      });
    }
    start = index + 1;
  }
  return { frames, remainder: boundedRemainder(bytes.subarray(start)) };
}

function crc16Modbus(bytes: Uint8Array): number {
  let crc = 0xffff;
  for (const byte of bytes) {
    crc ^= byte;
    for (let bit = 0; bit < 8; bit += 1) crc = (crc & 1) ? (crc >>> 1) ^ 0xa001 : crc >>> 1;
  }
  return crc & 0xffff;
}

function hasModbusCrc(frame: Uint8Array): boolean {
  if (frame.length < 4) return false;
  const crc = crc16Modbus(frame.subarray(0, frame.length - 2));
  return frame[frame.length - 2] === (crc & 0xff) && frame[frame.length - 1] === (crc >>> 8);
}

function modbusLength(bytes: Uint8Array): number | null {
  if (bytes.length < 2) return null;
  const functionCode = bytes[1];
  if (functionCode >= 0x80) return 5;
  if (functionCode === 1 || functionCode === 2 || functionCode === 3 || functionCode === 4) {
    if (bytes.length >= 8 && hasModbusCrc(bytes.subarray(0, 8))) return 8;
    if (bytes.length >= 3) {
      const responseLength = 5 + bytes[2];
      if (responseLength <= 261 && bytes.length >= responseLength) return responseLength;
    }
    return bytes.length >= 8 ? 8 : null;
  }
  if (functionCode === 5 || functionCode === 6) return 8;
  if (functionCode === 15 || functionCode === 16) {
    if (bytes.length >= 8 && hasModbusCrc(bytes.subarray(0, 8))) return 8;
    if (bytes.length >= 7) return 9 + bytes[6];
  }
  return null;
}

/** Parse common Modbus RTU request/response shapes and verify CRC-16. */
export function parseModbusRtu(
  buffer: Uint8Array,
  incoming: Uint8Array,
): { frames: ProtocolFrame[]; remainder: Uint8Array } {
  let bytes = concatBytes(buffer, incoming);
  const frames: ProtocolFrame[] = [];
  while (bytes.length >= 5) {
    const length = modbusLength(bytes);
    if (length === null || bytes.length < length) break;
    const frame = bytes.subarray(0, length).slice();
    const functionCode = frame[1];
    const valid = hasModbusCrc(frame);
    frames.push({
      protocol: "modbusrtu",
      summary: `Modbus ${valid ? "CRC OK" : "CRC FAIL"} unit ${frame[0]} function 0x${functionCode.toString(16).padStart(2, "0")}`,
      bytes: frame,
      valid,
      fields: {
        unit: frame[0],
        function: `0x${functionCode.toString(16).padStart(2, "0")}`,
        crc: valid ? "ok" : "mismatch",
      },
    });
    bytes = bytes.subarray(length);
  }
  // Unknown/noisy bytes must not grow forever while waiting for a recognizable
  // function code. Preserve a small suffix so a split frame can recover.
  if (bytes.length > MAX_REMAINDER) bytes = bytes.slice(-MAX_REMAINDER);
  return { frames, remainder: bytes.slice() };
}

export function protocolLabel(protocol: StreamProtocol): string {
  switch (protocol) {
    case "firewater":
      return "FireWater CSV";
    case "justfloat":
      return "JustFloat LE";
    case "nmea0183":
      return "NMEA 0183";
    case "slip":
      return "SLIP / RFC 1055";
    case "cobs":
      return "COBS";
    case "modbusrtu":
      return "Modbus RTU";
    default:
      return "RawData";
  }
}
