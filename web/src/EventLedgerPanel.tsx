import { memo, useMemo, useState } from "react";
import type { ReactNode } from "react";
import type {
  LedgerBytesPayload,
  LedgerEvent,
  LedgerEventType,
  LedgerStatus,
} from "./api/types";

const EVENT_TYPE_OPTIONS: ReadonlyArray<{
  type: LedgerEventType;
  label: string;
}> = [
  { type: "rx", label: "RX" },
  { type: "tx", label: "TX" },
  { type: "connection", label: "連線" },
  { type: "control", label: "控制" },
  { type: "gap", label: "缺口" },
];

const BASE64_PATTERN =
  /^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/;
const MAX_SAFE_DECODE_BYTES = 1024 * 1024;
const PREVIEW_BYTES = 96;
const SENSITIVE_CONTROL_NAME =
  /(?:authorization|bearer|lease[ _-]?token|api[ _-]?token|password|secret)/i;

type EventTypeSelection = Record<LedgerEventType, boolean>;

const INITIAL_SELECTION: EventTypeSelection = {
  rx: true,
  tx: true,
  connection: true,
  control: true,
  gap: true,
};

interface EventLedgerPanelProps {
  status: LedgerStatus | null;
  events: LedgerEvent[];
  loading: boolean;
  error: string | null;
  online: boolean;
  onRefresh: () => void;
  onExport: () => void;
}

interface DecodedPreview {
  hex: string;
  text: string;
  truncated: boolean;
  error?: string;
}

function safeBase64Preview(payload: LedgerBytesPayload): DecodedPreview {
  const { data_base64: encoded, len } = payload;
  if (!Number.isSafeInteger(len) || len < 0) {
    return { hex: "", text: "", truncated: false, error: "無效的位元組長度" };
  }
  if (len > MAX_SAFE_DECODE_BYTES) {
    return {
      hex: "",
      text: "",
      truncated: true,
      error: `內容超過安全預覽上限（${formatBytes(len)}）`,
    };
  }
  if (encoded.length > Math.ceil((MAX_SAFE_DECODE_BYTES * 4) / 3) + 4) {
    return { hex: "", text: "", truncated: true, error: "Base64 內容過大" };
  }
  if (encoded.length % 4 !== 0 || !BASE64_PATTERN.test(encoded)) {
    return { hex: "", text: "", truncated: false, error: "Base64 格式無效" };
  }

  const padding = encoded.endsWith("==") ? 2 : encoded.endsWith("=") ? 1 : 0;
  const estimatedLength = encoded.length === 0 ? 0 : (encoded.length / 4) * 3 - padding;
  if (estimatedLength !== len) {
    return { hex: "", text: "", truncated: false, error: "Base64 長度與宣告不符" };
  }

  try {
    const decoded = atob(encoded);
    if (decoded.length !== len) {
      return { hex: "", text: "", truncated: false, error: "解碼長度與宣告不符" };
    }
    const previewLength = Math.min(decoded.length, PREVIEW_BYTES);
    const bytes = new Uint8Array(previewLength);
    for (let index = 0; index < previewLength; index += 1) {
      bytes[index] = decoded.charCodeAt(index);
    }
    const truncated = decoded.length > previewLength;
    const suffix = truncated ? " …" : "";
    const hex = Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join(" ");
    const text = visibleUtf8(bytes);
    return { hex: `${hex}${suffix}`, text: `${text}${suffix}`, truncated };
  } catch {
    return { hex: "", text: "", truncated: false, error: "Base64 解碼失敗" };
  }
}

function eventBytes(event: LedgerEvent): LedgerBytesPayload[] {
  if (event.type === "rx" || event.type === "tx") return [event.payload];
  if (event.type === "gap" && event.payload.bytes) return [event.payload.bytes];
  return [];
}

function eventContainsHex(event: LedgerEvent, input: string): boolean {
  const needle = input.replace(/0x/gi, "").replace(/[\s,;:_-]+/g, "").toLowerCase();
  if (!needle || !/^[0-9a-f]+$/.test(needle) || needle.length % 2 !== 0) return false;
  return eventBytes(event).some((payload) => {
    if (payload.len > MAX_SAFE_DECODE_BYTES || payload.data_base64.length % 4 !== 0) return false;
    try {
      const decoded = atob(payload.data_base64);
      const hex = Array.from(decoded, (character) => character.charCodeAt(0).toString(16).padStart(2, "0"))
        .join("");
      return hex.includes(needle);
    } catch {
      return false;
    }
  });
}

function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return "?";
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MiB`;
}

function formatTimestamp(value: string): string {
  const parsed = new Date(value);
  if (Number.isNaN(parsed.getTime())) return value;
  return parsed.toLocaleString("zh-TW", { hour12: false });
}

function visibleUtf8(bytes: Uint8Array): string {
  const decoded = new TextDecoder("utf-8", { fatal: false }).decode(bytes);
  let visible = "";
  for (const character of decoded) {
    if (character === "\r") {
      visible += "\\r";
    } else if (character === "\n") {
      visible += "\\n";
    } else if (character === "\t") {
      visible += "\\t";
    } else {
      const codePoint = character.codePointAt(0) ?? 0;
      visible += codePoint < 32 || codePoint === 127 ? "·" : character;
    }
  }
  return visible;
}

const BytesPreview = memo(function BytesPreview({ payload }: { payload: LedgerBytesPayload }) {
  const preview = useMemo(() => safeBase64Preview(payload), [payload]);
  if (preview.error) {
    return <span className="event-payload-error">{preview.error}</span>;
  }
  return (
    <span className="event-bytes">
      <span className="event-text">{preview.text || "（空）"}</span>
      <span className="event-hex">{preview.hex || "（0 bytes）"}</span>
      <span className="event-length">
        {formatBytes(payload.len)}{preview.truncated ? "，僅顯示前段" : ""}
      </span>
    </span>
  );
});

function eventDetails(event: LedgerEvent): ReactNode {
  switch (event.type) {
    case "rx":
      return <BytesPreview payload={event.payload} />;
    case "tx":
      return (
        <>
          <span className="event-meta-line">
            actor <strong>{event.payload.actor}</strong>
            {event.payload.client_id ? ` · client ${event.payload.client_id}` : ""}
          </span>
          <BytesPreview payload={event.payload} />
        </>
      );
    case "connection":
      return (
        <span className="event-meta-line">
          <strong>{event.payload.state}</strong> · {event.payload.path} @ {event.payload.baud}
          {event.payload.detail ? ` · ${event.payload.detail}` : ""}
        </span>
      );
    case "control": {
      const value = SENSITIVE_CONTROL_NAME.test(event.payload.name)
        ? "（敏感值已隱藏）"
        : event.payload.value;
      return (
        <span className="event-meta-line">
          <strong>{event.payload.name}</strong>
          {value ? ` = ${value}` : ""}
          {event.payload.actor ? ` · actor ${event.payload.actor}` : ""}
        </span>
      );
    }
    case "gap": {
      const clientIds = event.payload.client_ids ?? [];
      const clients = clientIds.length
        ? ` · clients ${clientIds.slice(0, 5).join(", ")}${clientIds.length > 5 ? " …" : ""}`
        : "";
      return (
        <>
          <span className="event-meta-line">
            <strong>{event.payload.scope}</strong> / {event.payload.certainty} · {event.payload.reason}
            {event.payload.actor ? ` · actor ${event.payload.actor}` : ""}
            {clients}
          </span>
          {event.payload.bytes ? <BytesPreview payload={event.payload.bytes} /> : null}
        </>
      );
    }
  }
}

const LedgerEventRow = memo(function LedgerEventRow({ event }: { event: LedgerEvent }) {
  return (
    <article className={`event-row ${event.type}`}>
      <div className="event-row-head">
        <span className={`event-kind ${event.type}`}>{event.type.toUpperCase()}</span>
        <strong>#{event.seq.toLocaleString()}</strong>
        <span>{formatTimestamp(event.ts_utc)}</span>
        <span>epoch {event.connection_epoch}</span>
      </div>
      <div className="event-row-body">{eventDetails(event)}</div>
    </article>
  );
});

export default function EventLedgerPanel({
  status,
  events,
  loading,
  error,
  online,
  onRefresh,
  onExport,
}: EventLedgerPanelProps) {
  const [selected, setSelected] = useState<EventTypeSelection>(() => ({
    ...INITIAL_SELECTION,
  }));
  const [actor, setActor] = useState("");
  const [epoch, setEpoch] = useState("");
  const [containsHex, setContainsHex] = useState("");
  const visibleEvents = useMemo(
    () => events.filter((event) => {
      if (!selected[event.type]) return false;
      if (actor.trim()) {
        const actorText = "actor" in event.payload ? event.payload.actor ?? "" : "";
        if (!actorText.toLowerCase().includes(actor.trim().toLowerCase())) return false;
      }
      if (epoch.trim() && String(event.connection_epoch) !== epoch.trim()) return false;
      if (containsHex.trim()) {
        if (!eventContainsHex(event, containsHex.trim())) return false;
      }
      return true;
    }),
    [actor, containsHex, epoch, events, selected],
  );

  return (
    <section className="panel ledger-panel">
      <div className="ledger-head">
        <div>
          <h2>事件帳本（唯讀）</h2>
          <p className="hint">最近事件的可稽核視圖；原始即時 WS 監控仍在「監控與收發」。</p>
        </div>
        <button
          type="button"
          className="ghost"
          disabled={!online || loading}
          onClick={onRefresh}
        >
          {loading ? "讀取中…" : "手動重新整理"}
        </button>
        <button type="button" className="ghost" disabled={!online} onClick={onExport}>
          导出 NDJSON
        </button>
      </div>

      {status ? (
        <dl className="ledger-summary">
          <div>
            <dt>Persistence</dt>
            <dd>
              <span className={`tag persistence-${status.persistence}`}>{status.persistence}</span>
              {status.sealed ? " · sealed" : ""}
            </dd>
          </div>
          <div>
            <dt>Session</dt>
            <dd title={status.session_id}>{status.session_id}</dd>
          </div>
          <div>
            <dt>Sequence</dt>
            <dd>
              {status.oldest_available_seq ?? "—"} → {status.newest_seq}
            </dd>
          </div>
          <div>
            <dt>Retained</dt>
            <dd>
              {status.retained_events.toLocaleString()} events · {formatBytes(status.retained_bytes)}
            </dd>
          </div>
          <div>
            <dt>Evicted</dt>
            <dd>{status.evicted_events.toLocaleString()}</dd>
          </div>
        </dl>
      ) : (
        <p className="muted">尚未讀取事件帳本狀態。</p>
      )}

      {status?.persistence_error ? (
        <p className="error" role="status">
          Persistence degraded：{status.persistence_error}
        </p>
      ) : null}
      {error ? (
        <p className="error" role="alert">
          {error}
        </p>
      ) : null}

      <fieldset className="event-filters">
        <legend>事件類型</legend>
        {EVENT_TYPE_OPTIONS.map(({ type, label }) => (
          <label className="check" key={type}>
            <input
              type="checkbox"
              checked={selected[type]}
              onChange={(event) =>
                setSelected((current) => ({ ...current, [type]: event.target.checked }))
              }
            />
            {label}
          </label>
        ))}
      </fieldset>

      <div className="event-query-filters">
        <label>
          Actor
          <input value={actor} onChange={(event) => setActor(event.target.value)} placeholder="例如 web-ui" />
        </label>
        <label>
          Epoch
          <input value={epoch} onChange={(event) => setEpoch(event.target.value)} inputMode="numeric" placeholder="全部" />
        </label>
        <label>
          Hex / evidence
          <input value={containsHex} onChange={(event) => setContainsHex(event.target.value)} placeholder="例如 55 aa" />
        </label>
        <button
          type="button"
          className="ghost small"
          onClick={() => {
            setActor("");
            setEpoch("");
            setContainsHex("");
          }}
        >
          清除筛选
        </button>
      </div>

      <div className="event-list-meta" aria-live="polite">
        顯示 {visibleEvents.length} / {events.length} 筆最近事件
      </div>
      <div className="event-list">
        {visibleEvents.length ? (
          visibleEvents.map((event) => (
            <LedgerEventRow key={`${event.session_id}-${event.seq}`} event={event} />
          ))
        ) : (
          <div className="muted event-empty">
            {loading ? "正在讀取…" : "目前篩選條件下沒有事件。"}
          </div>
        )}
      </div>
    </section>
  );
}
