import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { hubApi } from "./api/client";
import type {
  ConnectionConfig,
  Endpoint,
  LedgerEvent,
  LedgerStatus,
  StatusSnapshot,
} from "./api/types";
import { httpBase, wsUrl } from "./api/types";
import { bytesToHex, bytesToText, connectStream } from "./api/wsStream";
import EventLedgerPanel from "./EventLedgerPanel";
import "./App.css";

type Tab = "monitor" | "events" | "endpoints" | "protocol";

type LogLine = {
  id: number;
  ts: string;
  kind: "rx" | "sys" | "tx" | "err";
  text: string;
  hex?: string;
};

const STORAGE_KEY = "ohmyserial.web.conn";
const CLIENT_NAME = "web-ui";
const RECENT_EVENT_LIMIT = 200;

function defaultConn(): ConnectionConfig {
  // 同源：由 hub 提供 http://127.0.0.1:8787/ 時自動連同一主機埠
  if (typeof window !== "undefined") {
    const host = window.location.hostname || "127.0.0.1";
    const portStr = window.location.port;
    const port = portStr
      ? Number(portStr)
      : window.location.protocol === "https:"
        ? 443
        : 80;
    // Vite 開發伺服器 → 仍連預設 hub
    if (port === 5173 || port === 4173) {
      return { host: "127.0.0.1", port: 8787 };
    }
    if (host) {
      return { host, port: port || 8787 };
    }
  }
  return { host: "127.0.0.1", port: 8787 };
}

function loadConn(): ConnectionConfig {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw) return JSON.parse(raw) as ConnectionConfig;
  } catch {
    /* ignore */
  }
  return defaultConn();
}

function nowTs(): string {
  return new Date().toLocaleTimeString("zh-TW", { hour12: false });
}

export default function App() {
  const [tab, setTab] = useState<Tab>("monitor");
  const [conn, setConn] = useState<ConnectionConfig>(loadConn);
  const [hostInput, setHostInput] = useState(conn.host);
  const [portInput, setPortInput] = useState(String(conn.port));
  // Optional remote-API bearer. It is deliberately never persisted.
  const [apiTokenInput, setApiTokenInput] = useState("");
  const apiBearerToken = apiTokenInput.trim() || undefined;

  const [online, setOnline] = useState(false);
  const [wsOpen, setWsOpen] = useState(false);
  const [status, setStatus] = useState<StatusSnapshot | null>(null);
  const [endpoints, setEndpoints] = useState<Endpoint[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [ledgerStatus, setLedgerStatus] = useState<LedgerStatus | null>(null);
  const [ledgerEvents, setLedgerEvents] = useState<LedgerEvent[]>([]);
  const [ledgerLoading, setLedgerLoading] = useState(false);
  const [ledgerError, setLedgerError] = useState<string | null>(null);
  const ledgerAbortRef = useRef<AbortController | null>(null);

  const [logs, setLogs] = useState<LogLine[]>([]);
  const [paused, setPaused] = useState(false);
  const [autoScroll, setAutoScroll] = useState(true);
  const logId = useRef(0);
  const logBoxRef = useRef<HTMLDivElement>(null);
  const wsRef = useRef<WebSocket | null>(null);
  const pausedRef = useRef(false);

  const [sendText, setSendText] = useState("");
  const [sendHex, setSendHex] = useState("");
  const [newline, setNewline] = useState(true);
  const [busy, setBusy] = useState(false);
  // A write lease is a bearer credential: keep it in memory only and never log it.
  const [leaseToken, setLeaseToken] = useState<string | null>(null);

  useEffect(() => {
    pausedRef.current = paused;
  }, [paused]);

  const pushLog = useCallback((line: Omit<LogLine, "id" | "ts"> & { ts?: string }) => {
    setLogs((prev) => {
      const next: LogLine = {
        id: ++logId.current,
        ts: line.ts ?? nowTs(),
        kind: line.kind,
        text: line.text,
        hex: line.hex,
      };
      const merged = [...prev, next];
      // 上限避免記憶體爆
      return merged.length > 2000 ? merged.slice(-1500) : merged;
    });
  }, []);

  const refreshMeta = useCallback(async (cfg: ConnectionConfig, bearerToken?: string) => {
    const [st, ep] = await Promise.all([
      hubApi.status(cfg, bearerToken),
      hubApi.endpoints(cfg, bearerToken),
    ]);
    setStatus(st);
    setEndpoints(ep.endpoints);
    setOnline(true);
    setError(null);
  }, []);

  const refreshLedger = useCallback(async (cfg: ConnectionConfig, bearerToken?: string) => {
    ledgerAbortRef.current?.abort();
    const controller = new AbortController();
    ledgerAbortRef.current = controller;
    setLedgerLoading(true);
    setLedgerError(null);
    try {
      // The status high-water mark is required before asking for a stable
      // recent window, so this dependency is intentionally sequential.
      const nextStatus = await hubApi.eventsStatus(cfg, bearerToken, controller.signal);
      if (!Number.isSafeInteger(nextStatus.newest_seq) || nextStatus.newest_seq < 0) {
        throw new Error("事件序號超出瀏覽器可安全表示的範圍");
      }
      const oldestCursor = Math.max(0, (nextStatus.oldest_available_seq ?? 1) - 1);
      const recentCursor = Math.max(0, nextStatus.newest_seq - RECENT_EVENT_LIMIT);
      const response = await hubApi.events(
        cfg,
        {
          afterSeq: Math.max(oldestCursor, recentCursor),
          throughSeq: nextStatus.newest_seq,
          limit: RECENT_EVENT_LIMIT,
        },
        bearerToken,
        controller.signal,
      );
      if (response.session_id !== nextStatus.session_id) {
        throw new Error("事件帳本 session 已切換，請重新整理");
      }
      setLedgerStatus(nextStatus);
      setLedgerEvents(response.page.events);
    } catch (ledgerFailure) {
      if (!(ledgerFailure instanceof DOMException && ledgerFailure.name === "AbortError")) {
        setLedgerError(
          `事件帳本讀取失敗：${
            ledgerFailure instanceof Error ? ledgerFailure.message : String(ledgerFailure)
          }`,
        );
      }
    } finally {
      if (ledgerAbortRef.current === controller) {
        ledgerAbortRef.current = null;
        setLedgerLoading(false);
      }
    }
  }, []);

  const disconnect = useCallback(() => {
    wsRef.current?.close();
    wsRef.current = null;
    ledgerAbortRef.current?.abort();
    ledgerAbortRef.current = null;
    setWsOpen(false);
    setOnline(false);
    setLeaseToken(null);
    setLedgerStatus(null);
    setLedgerEvents([]);
    setLedgerError(null);
    setLedgerLoading(false);
  }, []);

  const connect = useCallback(async () => {
    const cfg: ConnectionConfig = {
      host: hostInput.trim() || "127.0.0.1",
      port: Number(portInput) || 8787,
    };
    setConn(cfg);
    localStorage.setItem(STORAGE_KEY, JSON.stringify(cfg));
    setError(null);
    setBusy(true);
    try {
      await hubApi.health(cfg);
      await Promise.all([
        refreshMeta(cfg, apiBearerToken),
        refreshLedger(cfg, apiBearerToken),
      ]);

      wsRef.current?.close();
      const ws = connectStream(
        cfg,
        {
          onOpen: () => {
            setWsOpen(true);
            pushLog({ kind: "sys", text: `WebSocket 已連線 ${wsUrl(cfg)}` });
          },
          onClose: () => {
            setWsOpen(false);
            pushLog({ kind: "sys", text: "WebSocket 已關閉" });
          },
          onError: () => {
            pushLog({
              kind: "err",
              text: "WebSocket 錯誤（若頁面為 HTTPS，瀏覽器可能封鎖 ws://127.0.0.1）",
            });
          },
          onBytes: (data, meta) => {
            if (pausedRef.current) return;
            const text = bytesToText(data);
            const hex = bytesToHex(data);
            pushLog({
              kind: "rx",
              text: meta.isHistoryHint
                ? `[歷史?] ${text.replace(/\n/g, "\\n")}`
                : text.replace(/\n/g, "\\n"),
              hex,
            });
          },
        },
        apiBearerToken,
      );
      wsRef.current = ws;
    } catch (e) {
      ledgerAbortRef.current?.abort();
      setOnline(false);
      setError(e instanceof Error ? e.message : String(e));
      pushLog({
        kind: "err",
        text: `連線失敗：${e instanceof Error ? e.message : String(e)}。請先在本機執行 ohmyserial share ...`,
      });
    } finally {
      setBusy(false);
    }
  }, [apiBearerToken, hostInput, portInput, pushLog, refreshLedger, refreshMeta]);

  // 定時刷新狀態
  useEffect(() => {
    if (!online) return;
    const t = setInterval(() => {
      refreshMeta(conn, apiBearerToken).catch(() => setOnline(false));
    }, 2000);
    return () => clearInterval(t);
  }, [apiBearerToken, online, conn, refreshMeta]);

  useEffect(() => {
    if (!autoScroll || !logBoxRef.current) return;
    logBoxRef.current.scrollTop = logBoxRef.current.scrollHeight;
  }, [logs, autoScroll]);

  useEffect(
    () => () => {
      wsRef.current?.close();
      ledgerAbortRef.current?.abort();
    },
    [],
  );

  const onSendText = async () => {
    if (!sendText) return;
    setBusy(true);
    try {
      const r = await hubApi.writeText(conn, sendText, {
        newline,
        as_client: CLIENT_NAME,
        lease_token: leaseToken ?? undefined,
        bearer_token: apiBearerToken,
      });
      if (!r.ok) throw new Error(r.error || "寫入失敗");
      pushLog({
        kind: "tx",
        text: `HTTP 寫入 text ${r.bytes} bytes：${sendText.replace(/\n/g, "\\n")}`,
      });
    } catch (e) {
      pushLog({
        kind: "err",
        text: `寫入失敗：${e instanceof Error ? e.message : String(e)}`,
      });
    } finally {
      setBusy(false);
    }
  };

  const onSendHex = async () => {
    if (!sendHex.trim()) return;
    setBusy(true);
    try {
      const r = await hubApi.writeHex(conn, sendHex, {
        as_client: CLIENT_NAME,
        lease_token: leaseToken ?? undefined,
        bearer_token: apiBearerToken,
      });
      if (!r.ok) throw new Error(r.error || "寫入失敗");
      pushLog({ kind: "tx", text: `HTTP 寫入 hex ${r.bytes} bytes：${sendHex}` });
    } catch (e) {
      pushLog({
        kind: "err",
        text: `Hex 寫入失敗：${e instanceof Error ? e.message : String(e)}`,
      });
    } finally {
      setBusy(false);
    }
  };

  const onLock = async () => {
    setBusy(true);
    try {
      const r = await hubApi.lock(
        conn,
        CLIENT_NAME,
        leaseToken ?? undefined,
        apiBearerToken,
      );
      if (!r.ok) throw new Error(r.error || "鎖失敗");
      if (!r.lock) throw new Error("伺服器未回傳租約");
      setLeaseToken(r.lock.lease_token);
      pushLog({
        kind: "sys",
        text: `已取得寫鎖：${r.lock.owner}（${r.lock.expires_ms} ms）`,
      });
      await refreshMeta(conn, apiBearerToken);
    } catch (e) {
      setLeaseToken(null);
      pushLog({
        kind: "err",
        text: `寫鎖失敗：${e instanceof Error ? e.message : String(e)}`,
      });
    } finally {
      setBusy(false);
    }
  };

  const onUnlock = async () => {
    if (!leaseToken) {
      pushLog({ kind: "err", text: "本頁沒有可釋放的租約令牌" });
      return;
    }
    setBusy(true);
    try {
      const r = await hubApi.unlock(conn, leaseToken, apiBearerToken);
      if (!r.ok) throw new Error(r.error || "解鎖失敗");
      setLeaseToken(null);
      pushLog({ kind: "sys", text: "已釋放寫鎖" });
      await refreshMeta(conn, apiBearerToken);
    } catch (e) {
      pushLog({
        kind: "err",
        text: `解鎖失敗：${e instanceof Error ? e.message : String(e)}`,
      });
    } finally {
      setBusy(false);
    }
  };

  const copy = async (text: string) => {
    try {
      await navigator.clipboard.writeText(text);
      pushLog({ kind: "sys", text: `已複製：${text}` });
    } catch {
      pushLog({ kind: "err", text: "複製失敗" });
    }
  };

  const statusLamp = useMemo(() => {
    if (online && wsOpen) return { cls: "ok", label: "已連線（HTTP + WS）" };
    if (online) return { cls: "warn", label: "HTTP 通，WS 未連" };
    return { cls: "off", label: "未連線" };
  }, [online, wsOpen]);

  return (
    <div className="app">
      <header className="header">
        <div>
          <h1>ohmyserial 控制台</h1>
          <p className="sub">
            本機串口共享中樞 · 即時監控 · 繁體中文 · 協定見{" "}
            <code>web/PROTOCOL.zh-TW.md</code>
          </p>
        </div>
        <div className={`lamp ${statusLamp.cls}`}>{statusLamp.label}</div>
      </header>

      <section className="panel connect">
        <h2>連線本機 hub</h2>
        <div className="row">
          <label>
            主機
            <input
              value={hostInput}
              onChange={(e) => setHostInput(e.target.value)}
              placeholder="127.0.0.1"
            />
          </label>
          <label>
            埠
            <input
              value={portInput}
              onChange={(e) => setPortInput(e.target.value)}
              placeholder="8787"
              className="port"
            />
          </label>
          <label>
            API Token（遠端選填）
            <input
              type="password"
              autoComplete="off"
              value={apiTokenInput}
              onChange={(e) => setApiTokenInput(e.target.value)}
              placeholder="只保存在本頁記憶體"
            />
          </label>
          <button type="button" disabled={busy} onClick={() => void connect()}>
            連線
          </button>
          <button type="button" className="ghost" onClick={disconnect}>
            斷開
          </button>
        </div>
        <p className="hint">
          請先在本機執行：
          <code>ohmyserial share mock:demo</code> 或真實裝置。 HTTP：
          <code>{httpBase(conn)}</code> · WS：
          <code>{wsUrl(conn)}</code>
        </p>
        {error && <p className="error">{error}</p>}
      </section>

      <nav className="tabs">
        {(
          [
            ["monitor", "監控與收發"],
            ["events", "事件帳本"],
            ["endpoints", "並聯端點"],
            ["protocol", "協定說明"],
          ] as const
        ).map(([id, label]) => (
          <button
            key={id}
            type="button"
            className={tab === id ? "active" : ""}
            onClick={() => setTab(id)}
          >
            {label}
          </button>
        ))}
      </nav>

      {tab === "monitor" && (
        <div className="grid">
          <section className="panel">
            <h2>狀態</h2>
            {status ? (
              <dl className="kv">
                <dt>真實埠</dt>
                <dd>
                  {status.port.path}{" "}
                  <span className={status.port.connected ? "tag ok" : "tag off"}>
                    {status.port.connected ? "已開啟" : "未連"}
                  </span>
                </dd>
                <dt>鮑率</dt>
                <dd>{status.port.baud}</dd>
                <dt>詳情</dt>
                <dd>{status.port.detail}</dd>
                <dt>TX 模式</dt>
                <dd>
                  <code>{status.tx_mode}</code>
                </dd>
                <dt>寫鎖</dt>
                <dd>
                  {status.lock_owner
                    ? `${status.lock_owner}（剩餘 ${status.lock_expires_ms ?? "?"} ms）`
                    : "無"}
                </dd>
                <dt>統計</dt>
                <dd>
                  RX {status.stats.rx_bytes} · TX {status.stats.tx_bytes} · drop{" "}
                  {status.stats.rx_drops} · deny {status.stats.tx_denies}
                </dd>
                <dt>客戶端數</dt>
                <dd>{status.clients.length}</dd>
              </dl>
            ) : (
              <p className="muted">尚未連線</p>
            )}
            <div className="row">
              <button type="button" disabled={!online || busy} onClick={() => void onLock()}>
                取得寫鎖
              </button>
              <button
                type="button"
                className="ghost"
                disabled={!online || busy}
                onClick={() => void onUnlock()}
              >
                釋放寫鎖
              </button>
              <button
                type="button"
                className="ghost"
                disabled={!online || busy}
                onClick={() => void refreshMeta(conn, apiBearerToken)}
              >
                重新整理
              </button>
            </div>
          </section>

          <section className="panel">
            <h2>送出（HTTP /v1/write）</h2>
            <label className="block">
              文字
              <textarea
                rows={3}
                value={sendText}
                onChange={(e) => setSendText(e.target.value)}
                placeholder="例如 AT 或任意指令"
              />
            </label>
            <label className="check">
              <input
                type="checkbox"
                checked={newline}
                onChange={(e) => setNewline(e.target.checked)}
              />
              自動補換行 \\n
            </label>
            <button type="button" disabled={!online || busy} onClick={() => void onSendText()}>
              送出文字
            </button>
            <label className="block">
              十六進位
              <input
                value={sendHex}
                onChange={(e) => setSendHex(e.target.value)}
                placeholder="41 54 0d 0a"
              />
            </label>
            <button type="button" disabled={!online || busy} onClick={() => void onSendHex()}>
              送出 Hex
            </button>
            <p className="hint">
              可靠回傳請用 HTTP 寫入。WS 也可 TX，但錯誤僅記在 hub 日誌。
            </p>
          </section>

          <section className="panel log-panel">
            <div className="log-head">
              <h2>即時日誌（WS /v1/stream）</h2>
              <div className="row">
                <label className="check">
                  <input
                    type="checkbox"
                    checked={paused}
                    onChange={(e) => setPaused(e.target.checked)}
                  />
                  暫停顯示
                </label>
                <label className="check">
                  <input
                    type="checkbox"
                    checked={autoScroll}
                    onChange={(e) => setAutoScroll(e.target.checked)}
                  />
                  自動捲動
                </label>
                <button type="button" className="ghost" onClick={() => setLogs([])}>
                  清空
                </button>
              </div>
            </div>
            <div className="log" ref={logBoxRef}>
              {logs.length === 0 && <div className="muted">尚無訊息</div>}
              {logs.map((l) => (
                <div key={l.id} className={`line ${l.kind}`}>
                  <span className="ts">{l.ts}</span>
                  <span className="kind">{l.kind.toUpperCase()}</span>
                  <span className="msg">{l.text}</span>
                  {l.hex && <span className="hex">{l.hex}</span>}
                </div>
              ))}
            </div>
          </section>
        </div>
      )}

      {tab === "events" && (
        <EventLedgerPanel
          status={ledgerStatus}
          events={ledgerEvents}
          loading={ledgerLoading}
          error={ledgerError}
          online={online}
          onRefresh={() => void refreshLedger(conn, apiBearerToken)}
        />
      )}

      {tab === "endpoints" && (
        <section className="panel">
          <h2>並聯端點（一個真串口 → 多路出口）</h2>
          <p className="hint">
            上位機開 SERIAL（PTY）路徑；腳本連 TCP；Agent / 本頁用 WebSocket。
          </p>
          <table>
            <thead>
              <tr>
                <th>類型</th>
                <th>名稱</th>
                <th>位址</th>
                <th>讀/寫</th>
                <th>說明</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {endpoints.length === 0 && (
                <tr>
                  <td colSpan={6} className="muted">
                    無資料 — 請先連線
                  </td>
                </tr>
              )}
              {endpoints.map((ep) => (
                <tr key={`${ep.kind}-${ep.name}-${ep.address}`}>
                  <td>
                    <span className="tag">{ep.kind}</span>
                  </td>
                  <td>{ep.name}</td>
                  <td>
                    <code>{ep.address}</code>
                  </td>
                  <td>
                    {ep.can_read ? "R" : "-"}/{ep.can_write ? "W" : "-"}
                  </td>
                  <td className="note">{ep.note}</td>
                  <td>
                    <button
                      type="button"
                      className="ghost small"
                      onClick={() => void copy(ep.address)}
                    >
                      複製
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>

          <h3>目前 fan-out 客戶端</h3>
          <table>
            <thead>
              <tr>
                <th>名稱</th>
                <th>類型</th>
                <th>讀/寫</th>
                <th>id</th>
              </tr>
            </thead>
            <tbody>
              {(status?.clients ?? []).map((c) => (
                <tr key={c.id}>
                  <td>{c.name}</td>
                  <td>{c.kind}</td>
                  <td>
                    {c.can_read ? "R" : "-"}/{c.can_write ? "W" : "-"}
                  </td>
                  <td>
                    <code className="small">{c.id}</code>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </section>
      )}

      {tab === "protocol" && (
        <section className="panel protocol">
          <h2>協定精要（繁中）</h2>
          <p>
            完整文件：倉庫內 <code>web/PROTOCOL.zh-TW.md</code>
          </p>
          <h3>HTTP</h3>
          <ul>
            <li>
              <code>GET /v1/health</code> — 探活
            </li>
            <li>
              <code>GET /v1/status</code> — 埠、鎖、統計、客戶端
            </li>
            <li>
              <code>GET /v1/endpoints</code> — 並聯端點清單
            </li>
            <li>
              <code>POST /v1/write</code> —{" "}
              <code>{`{"text":"...","newline":true,"as_client":"web-ui"}`}</code> 或{" "}
              <code>hex</code>
            </li>
            <li>
              <code>POST /v1/lock</code> · <code>DELETE /v1/lock</code>
            </li>
          </ul>
          <h3>WebSocket <code>/v1/stream</code></h3>
          <ul>
            <li>
              <strong>下行 Binary</strong>：裝置 RX 原始位元組（可能含歷史首包）
            </li>
            <li>
              <strong>上行 Text</strong>：當 TX，無 <code>\n</code> 時 hub 自動補行
            </li>
            <li>
              <strong>上行 Binary</strong>：原樣進入 TX 策略
            </li>
            <li>可靠寫入請優先 HTTP <code>/v1/write</code>（有 ok/error）</li>
          </ul>
          <h3>使用提醒</h3>
          <ul>
            <li>一個真串口可並聯多 PTY / 多 TCP / 多 WS，RX 全廣播</li>
            <li>TX 預設按行排隊，避免位元組交錯</li>
            <li>HTTPS 網頁連本機 ws:// 可能被瀏覽器封鎖</li>
          </ul>
        </section>
      )}

      <footer className="footer">
        ohmyserial · CLI + 本機 HTTP/WS · 可選網頁監控 · MIT
      </footer>
    </div>
  );
}
