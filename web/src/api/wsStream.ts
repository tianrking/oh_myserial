import type { ConnectionConfig } from "./types";
import { wsUrl } from "./types";

export type StreamHandlers = {
  onOpen?: () => void;
  onClose?: () => void;
  onError?: (ev: Event) => void;
  /** 原始位元組（裝置 RX） */
  onBytes?: (data: Uint8Array, meta: { isHistoryHint: boolean }) => void;
};

/**
 * 連線至 hub WebSocket `/v1/stream`。
 * - 伺服器 → 客戶端：Binary = 裝置 RX（連線後可能先推歷史）
 * - 客戶端 → 伺服器：Text/Binary = TX（建議寫入改走 HTTP /v1/write）
 */
export function connectStream(
  cfg: ConnectionConfig,
  handlers: StreamHandlers,
): WebSocket {
  const url = wsUrl(cfg);
  const ws = new WebSocket(url);
  ws.binaryType = "arraybuffer";

  let firstBinary = true;

  ws.onopen = () => handlers.onOpen?.();
  ws.onclose = () => handlers.onClose?.();
  ws.onerror = (ev) => handlers.onError?.(ev);

  ws.onmessage = (ev) => {
    if (typeof ev.data === "string") {
      const enc = new TextEncoder();
      handlers.onBytes?.(enc.encode(ev.data), { isHistoryHint: false });
      return;
    }
    const buf = new Uint8Array(ev.data as ArrayBuffer);
    // 協定未標記歷史幀；實務上連線後第一包 binary 常為 history
    const isHistoryHint = firstBinary;
    firstBinary = false;
    handlers.onBytes?.(buf, { isHistoryHint });
  };

  return ws;
}

export function bytesToHex(data: Uint8Array, max = 64): string {
  const slice = data.length > max ? data.subarray(0, max) : data;
  let s = [...slice].map((b) => b.toString(16).padStart(2, "0")).join(" ");
  if (data.length > max) s += " …";
  return s;
}

export function bytesToText(data: Uint8Array): string {
  try {
    return new TextDecoder("utf-8", { fatal: false }).decode(data);
  } catch {
    return "";
  }
}
