export type StreamProtocol = "raw" | "firewater" | "justfloat";

export type WaveSample = {
  values: number[];
  label?: string;
};

export const JUSTFLOAT_TAIL = new Uint8Array([0x00, 0x00, 0x80, 0x7f]);

function findTail(bytes: Uint8Array): number {
  outer: for (let offset = 0; offset <= bytes.length - JUSTFLOAT_TAIL.length; offset += 1) {
    for (let index = 0; index < JUSTFLOAT_TAIL.length; index += 1) {
      if (bytes[offset + index] !== JUSTFLOAT_TAIL[index]) continue outer;
    }
    return offset;
  }
  return -1;
}

/**
 * Parse complete FireWater frames from a UTF-8 text buffer. The protocol is
 * deliberately line based (`label:1.0,2.0\n`), so an incomplete final line is
 * returned for the next WebSocket chunk instead of being silently discarded.
 */
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

function concatBytes(left: Uint8Array, right: Uint8Array): Uint8Array {
  const joined = new Uint8Array(left.length + right.length);
  joined.set(left);
  joined.set(right, left.length);
  return joined;
}

/**
 * Parse one or more JustFloat frames. Each frame is a little-endian float32
 * array followed by `00 00 80 7f`. Incomplete bytes remain buffered across
 * WebSocket messages. Frames whose payload is not float-aligned are ignored
 * but do not poison the following frame.
 */
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
  // Keep only the suffix that could become a tail or a partial float on the
  // next message. This prevents an unbounded buffer when a device is noisy.
  const maxRemainder = 64 * 1024;
  return {
    samples,
    remainder: bytes.length > maxRemainder ? bytes.slice(-maxRemainder) : bytes.slice(),
  };
}

export function protocolLabel(protocol: StreamProtocol): string {
  switch (protocol) {
    case "firewater":
      return "FireWater CSV";
    case "justfloat":
      return "JustFloat LE";
    default:
      return "RawData（原始）";
  }
}
