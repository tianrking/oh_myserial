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
import {
  appendLineEnding,
  CONNECTION_PROFILES_STORAGE_KEY,
  loadConnectionProfiles,
  loadQuickCommands,
  newCommandId,
  newProfileId,
  QUICK_COMMANDS_STORAGE_KEY,
  type CommandMode,
  type ConnectionProfile,
  type LineEnding,
  type QuickCommand,
} from "./commandUtils";
import EventLedgerPanel from "./EventLedgerPanel";
import MetricsPanel from "./MetricsPanel";
import ProtocolInspectorPanel from "./ProtocolInspectorPanel";
import WaveformPanel from "./WaveformPanel";
import {
  appendHexChecksum,
  checksumLabel,
  type HexChecksum,
} from "./checksumUtils";
import {
  parseFireWater,
  parseJustFloat,
  parseCobs,
  parseModbusRtu,
  parseNmea0183,
  parseSlip,
  protocolLabel,
  type ProtocolFrame,
  type StreamProtocol,
  type WaveSample,
} from "./protocolUtils";
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
const MAX_QUICK_COMMANDS = 200;

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

function downloadTextFile(filename: string, content: string, mime = "text/plain;charset=utf-8") {
  const blob = new Blob([content], { type: mime });
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = filename;
  anchor.click();
  URL.revokeObjectURL(url);
}

export default function App() {
  const [tab, setTab] = useState<Tab>("monitor");
  const [conn, setConn] = useState<ConnectionConfig>(loadConn);
  const [hostInput, setHostInput] = useState(conn.host);
  const [portInput, setPortInput] = useState(String(conn.port));
  const [connectionProfiles, setConnectionProfiles] = useState<ConnectionProfile[]>(
    loadConnectionProfiles,
  );
  const [selectedProfileId, setSelectedProfileId] = useState("");
  const [profileName, setProfileName] = useState("");
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
  const [metricsText, setMetricsText] = useState("");
  const [metricsLoading, setMetricsLoading] = useState(false);
  const [metricsError, setMetricsError] = useState<string | null>(null);
  const metricsAbortRef = useRef<AbortController | null>(null);

  const [logs, setLogs] = useState<LogLine[]>([]);
  const [paused, setPaused] = useState(false);
  const [autoScroll, setAutoScroll] = useState(true);
  const logId = useRef(0);
  const logBoxRef = useRef<HTMLDivElement>(null);
  const wsRef = useRef<WebSocket | null>(null);
  const pausedRef = useRef(false);

  const [sendText, setSendText] = useState("");
  const [sendHex, setSendHex] = useState("");
  const [hexChecksum, setHexChecksum] = useState<HexChecksum>("none");
  const [lineEnding, setLineEnding] = useState<LineEnding>("lf");
  const [autoSend, setAutoSend] = useState(false);
  const [autoIntervalMs, setAutoIntervalMs] = useState(1000);
  const [autoMode, setAutoMode] = useState<CommandMode>("text");
  const [displayMode, setDisplayMode] = useState<"text" | "hex" | "both">("both");
  const [showTimestamp, setShowTimestamp] = useState(true);
  const [streamProtocol, setStreamProtocol] = useState<StreamProtocol>("raw");
  const [waveSamples, setWaveSamples] = useState<WaveSample[]>([]);
  const [protocolFrames, setProtocolFrames] = useState<ProtocolFrame[]>([]);
  const [waveChannel, setWaveChannel] = useState(0);
  const streamProtocolRef = useRef<StreamProtocol>(streamProtocol);
  const fireWaterBufferRef = useRef("");
  const fireWaterDecoderRef = useRef(new TextDecoder());
  const justFloatBufferRef = useRef<Uint8Array<ArrayBufferLike>>(new Uint8Array());
  const nmeaBufferRef = useRef("");
  const slipBufferRef = useRef<Uint8Array<ArrayBufferLike>>(new Uint8Array());
  const cobsBufferRef = useRef<Uint8Array<ArrayBufferLike>>(new Uint8Array());
  const modbusBufferRef = useRef<Uint8Array<ArrayBufferLike>>(new Uint8Array());
  const [quickCommands, setQuickCommands] = useState<QuickCommand[]>(loadQuickCommands);
  const [editingCommandId, setEditingCommandId] = useState<string | null>(null);
  const [quickName, setQuickName] = useState("");
  const [quickMode, setQuickMode] = useState<CommandMode>("text");
  const [quickPayload, setQuickPayload] = useState("");
  const [quickLineEnding, setQuickLineEnding] = useState<LineEnding>("lf");
  const [breakDurationMs, setBreakDurationMs] = useState(100);
  const [busy, setBusy] = useState(false);
  // A write lease is a bearer credential: keep it in memory only and never log it.
  const [leaseToken, setLeaseToken] = useState<string | null>(null);

  useEffect(() => {
    pausedRef.current = paused;
  }, [paused]);

  useEffect(() => {
    try {
      localStorage.setItem(QUICK_COMMANDS_STORAGE_KEY, JSON.stringify(quickCommands));
    } catch {
      // A private browsing context may deny localStorage. The editor still works in memory.
    }
  }, [quickCommands]);

  useEffect(() => {
    try {
      localStorage.setItem(CONNECTION_PROFILES_STORAGE_KEY, JSON.stringify(connectionProfiles));
    } catch {
      // Keep profiles usable in memory when storage is unavailable.
    }
  }, [connectionProfiles]);

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

  const saveConnectionProfile = () => {
    const name = profileName.trim();
    const host = hostInput.trim();
    const port = Number(portInput);
    if (!name || !host || !Number.isInteger(port) || port < 1 || port > 65535) {
      pushLog({ kind: "err", text: "会话配置需要名称、主机和有效端口" });
      return;
    }
    const profile: ConnectionProfile = {
      id: selectedProfileId || newProfileId(),
      name,
      host,
      port,
    };
    setConnectionProfiles((previous) => [profile, ...previous.filter((item) => item.id !== profile.id)].slice(0, 50));
    setSelectedProfileId(profile.id);
    setProfileName(profile.name);
    pushLog({ kind: "sys", text: `已保存会话配置：${profile.name}` });
  };

  const loadConnectionProfile = (id: string) => {
    setSelectedProfileId(id);
    const profile = connectionProfiles.find((item) => item.id === id);
    if (!profile) return;
    setProfileName(profile.name);
    setHostInput(profile.host);
    setPortInput(String(profile.port));
    pushLog({ kind: "sys", text: `已载入会话配置：${profile.name}` });
  };

  const deleteConnectionProfile = () => {
    if (!selectedProfileId) return;
    const profile = connectionProfiles.find((item) => item.id === selectedProfileId);
    setConnectionProfiles((previous) => previous.filter((item) => item.id !== selectedProfileId));
    setSelectedProfileId("");
    setProfileName("");
    if (profile) pushLog({ kind: "sys", text: `已删除会话配置：${profile.name}` });
  };

  const consumeWaveBytes = useCallback(
    (data: Uint8Array) => {
      const currentProtocol = streamProtocolRef.current;
      if (currentProtocol === "raw") return;
      if (currentProtocol === "firewater") {
        const parsed = parseFireWater(
          `${fireWaterBufferRef.current}${fireWaterDecoderRef.current.decode(data, { stream: true })}`,
        );
        fireWaterBufferRef.current = parsed.remainder;
        if (parsed.samples.length === 0) return;
        setWaveSamples((previous) => [...previous, ...parsed.samples].slice(-800));
        return;
      }
      const parsed = parseJustFloat(justFloatBufferRef.current, data);
      justFloatBufferRef.current = parsed.remainder;
      if (parsed.samples.length === 0) return;
      setWaveSamples((previous) => [...previous, ...parsed.samples].slice(-800));
    },
    [],
  );

  const consumeProtocolBytes = useCallback((data: Uint8Array) => {
    const currentProtocol = streamProtocolRef.current;
    if (currentProtocol === "nmea0183") {
      const incoming = new TextDecoder().decode(data);
      const parsed = parseNmea0183(nmeaBufferRef.current, incoming);
      nmeaBufferRef.current = parsed.remainder;
      if (parsed.frames.length) setProtocolFrames((previous) => [...previous, ...parsed.frames].slice(-100));
      return;
    }
    if (currentProtocol === "slip") {
      const parsed = parseSlip(slipBufferRef.current, data);
      slipBufferRef.current = parsed.remainder;
      if (parsed.frames.length) setProtocolFrames((previous) => [...previous, ...parsed.frames].slice(-100));
      return;
    }
    if (currentProtocol === "cobs") {
      const parsed = parseCobs(cobsBufferRef.current, data);
      cobsBufferRef.current = parsed.remainder;
      if (parsed.frames.length) setProtocolFrames((previous) => [...previous, ...parsed.frames].slice(-100));
      return;
    }
    if (currentProtocol === "modbusrtu") {
      const parsed = parseModbusRtu(modbusBufferRef.current, data);
      modbusBufferRef.current = parsed.remainder;
      if (parsed.frames.length) setProtocolFrames((previous) => [...previous, ...parsed.frames].slice(-100));
    }
  }, []);

  useEffect(() => {
    streamProtocolRef.current = streamProtocol;
    fireWaterBufferRef.current = "";
    fireWaterDecoderRef.current = new TextDecoder();
    justFloatBufferRef.current = new Uint8Array();
    nmeaBufferRef.current = "";
    slipBufferRef.current = new Uint8Array();
    cobsBufferRef.current = new Uint8Array();
    modbusBufferRef.current = new Uint8Array();
    setWaveSamples([]);
    setProtocolFrames([]);
    setWaveChannel(0);
  }, [streamProtocol]);

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

  const refreshMetrics = useCallback(async (cfg: ConnectionConfig, bearerToken?: string) => {
    metricsAbortRef.current?.abort();
    const controller = new AbortController();
    metricsAbortRef.current = controller;
    setMetricsLoading(true);
    setMetricsError(null);
    try {
      setMetricsText(await hubApi.metrics(cfg, bearerToken, controller.signal));
    } catch (metricsFailure) {
      if (!(metricsFailure instanceof DOMException && metricsFailure.name === "AbortError")) {
        setMetricsError(metricsFailure instanceof Error ? metricsFailure.message : String(metricsFailure));
      }
    } finally {
      if (metricsAbortRef.current === controller) {
        metricsAbortRef.current = null;
        setMetricsLoading(false);
      }
    }
  }, []);

  const disconnect = useCallback(() => {
    wsRef.current?.close();
    wsRef.current = null;
    ledgerAbortRef.current?.abort();
    ledgerAbortRef.current = null;
    metricsAbortRef.current?.abort();
    metricsAbortRef.current = null;
    setWsOpen(false);
    setOnline(false);
    setLeaseToken(null);
    setLedgerStatus(null);
    setLedgerEvents([]);
    setLedgerError(null);
    setLedgerLoading(false);
    setMetricsText("");
    setMetricsError(null);
    setMetricsLoading(false);
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
        refreshMetrics(cfg, apiBearerToken),
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
            consumeWaveBytes(data);
            consumeProtocolBytes(data);
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
  }, [
    apiBearerToken,
    consumeProtocolBytes,
    consumeWaveBytes,
    hostInput,
    portInput,
    pushLog,
    refreshLedger,
    refreshMetrics,
    refreshMeta,
  ]);

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
      metricsAbortRef.current?.abort();
    },
    [],
  );

  const transmit = useCallback(
    async (
      mode: CommandMode,
      payload: string,
      ending: LineEnding,
      checksum: HexChecksum = "none",
    ): Promise<boolean> => {
      if (mode === "text" ? payload.length === 0 : payload.trim().length === 0) return false;
      setBusy(true);
      try {
        if (mode === "text") {
          const wire = appendLineEnding(payload, ending);
          const r = await hubApi.writeText(conn, wire, {
            newline: false,
            as_client: CLIENT_NAME,
            lease_token: leaseToken ?? undefined,
            bearer_token: apiBearerToken,
          });
          if (!r.ok) throw new Error(r.error || "写入失败");
          pushLog({
            kind: "tx",
            text: `HTTP 写入 text ${r.bytes} bytes：${wire
              .replace(/\r/g, "\\r")
              .replace(/\n/g, "\\n")}`,
          });
        } else {
          const wire = appendHexChecksum(payload, checksum);
          const r = await hubApi.writeHex(conn, wire, {
            as_client: CLIENT_NAME,
            lease_token: leaseToken ?? undefined,
            bearer_token: apiBearerToken,
          });
          if (!r.ok) throw new Error(r.error || "写入失败");
          pushLog({
            kind: "tx",
            text: `HTTP 写入 hex ${r.bytes} bytes：${wire}${checksum !== "none" ? `（${checksumLabel(checksum)}）` : ""}`,
          });
        }
        return true;
      } catch (e) {
        pushLog({
          kind: "err",
          text: `写入失败：${e instanceof Error ? e.message : String(e)}`,
        });
        return false;
      } finally {
        setBusy(false);
      }
    },
    [apiBearerToken, conn, leaseToken, pushLog],
  );

  const onSendText = () => void transmit("text", sendText, lineEnding);

  const onSendHex = () => void transmit("hex", sendHex, "none", hexChecksum);

  const hexPreview = useMemo(() => {
    if (!sendHex.trim()) return "";
    try {
      return appendHexChecksum(sendHex, hexChecksum);
    } catch (e) {
      return e instanceof Error ? `错误：${e.message}` : `错误：${String(e)}`;
    }
  }, [hexChecksum, sendHex]);

  useEffect(() => {
    if (!autoSend || !online || autoIntervalMs < 50) return;
    const timer = window.setInterval(() => {
      if (busy) return;
      const payload = autoMode === "text" ? sendText : sendHex;
      void transmit(
        autoMode,
        payload,
        autoMode === "text" ? lineEnding : "none",
        autoMode === "hex" ? hexChecksum : "none",
      );
    }, autoIntervalMs);
    return () => window.clearInterval(timer);
  }, [autoIntervalMs, autoMode, autoSend, busy, hexChecksum, lineEnding, online, sendHex, sendText, transmit]);

  const resetQuickEditor = () => {
    setEditingCommandId(null);
    setQuickName("");
    setQuickMode("text");
    setQuickPayload("");
    setQuickLineEnding("lf");
  };

  const saveQuickCommand = () => {
    const name = quickName.trim();
    if (!name || !quickPayload.trim()) {
      pushLog({ kind: "err", text: "快捷指令需要名称和内容" });
      return;
    }
    const next: QuickCommand = {
      id: editingCommandId ?? newCommandId(),
      name,
      mode: quickMode,
      payload: quickPayload,
      lineEnding: quickMode === "text" ? quickLineEnding : "none",
    };
    setQuickCommands((previous) => {
      const withoutCurrent = previous.filter((command) => command.id !== next.id);
      return [next, ...withoutCurrent].slice(0, MAX_QUICK_COMMANDS);
    });
    pushLog({ kind: "sys", text: `${editingCommandId ? "已更新" : "已保存"}快捷指令：${name}` });
    resetQuickEditor();
  };

  const editQuickCommand = (command: QuickCommand) => {
    setEditingCommandId(command.id);
    setQuickName(command.name);
    setQuickMode(command.mode);
    setQuickPayload(command.payload);
    setQuickLineEnding(command.lineEnding);
  };

  const deleteQuickCommand = (command: QuickCommand) => {
    setQuickCommands((previous) => previous.filter((item) => item.id !== command.id));
    if (editingCommandId === command.id) resetQuickEditor();
    pushLog({ kind: "sys", text: `已删除快捷指令：${command.name}` });
  };

  const exportVisibleLogs = () => {
    const text = logs
      .map((line) => `${line.ts}\t${line.kind.toUpperCase()}\t${line.text}${line.hex ? `\t${line.hex}` : ""}`)
      .join("\n");
    downloadTextFile(`ohmyserial-${new Date().toISOString().replace(/[:.]/g, "-")}.log`, text);
    pushLog({ kind: "sys", text: `已导出 ${logs.length} 条页面日志` });
  };

  const exportLedger = async () => {
    try {
      const ndjson = await hubApi.eventsExport(conn, apiBearerToken);
      downloadTextFile(
        `ohmyserial-events-${new Date().toISOString().replace(/[:.]/g, "-")}.ndjson`,
        ndjson,
        "application/x-ndjson;charset=utf-8",
      );
      pushLog({ kind: "sys", text: `事件证据已导出 ${ndjson.split("\n").filter(Boolean).length} 条` });
    } catch (exportFailure) {
      pushLog({ kind: "err", text: `事件导出失败：${exportFailure instanceof Error ? exportFailure.message : String(exportFailure)}` });
    }
  };

  const exportMetrics = () => {
    downloadTextFile(
      `ohmyserial-metrics-${new Date().toISOString().replace(/[:.]/g, "-")}.prom`,
      metricsText,
      "text/plain;charset=utf-8",
    );
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

  const onControl = async (
    body:
      | { op: "dtr" | "rts"; level: boolean }
      | { op: "break"; duration_ms: number },
  ) => {
    if (!leaseToken) {
      pushLog({ kind: "err", text: "控制線操作需要先取得寫鎖" });
      return;
    }
    setBusy(true);
    try {
      const r = await hubApi.control(
        conn,
        { ...body, as_client: CLIENT_NAME, lease_token: leaseToken },
        apiBearerToken,
      );
      if (!r.ok) throw new Error(r.error || "控制線操作失敗");
      pushLog({
        kind: "sys",
        text:
          body.op === "break"
            ? `BREAK ${body.duration_ms} ms 已確認`
            : `${body.op.toUpperCase()} ${body.level ? "ON" : "OFF"} 已確認`,
      });
    } catch (e) {
      pushLog({ kind: "err", text: `控制線操作失敗：${e instanceof Error ? e.message : String(e)}` });
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
        <div className="row profile-row">
          <label>
            会话配置
            <select value={selectedProfileId} onChange={(e) => loadConnectionProfile(e.target.value)}>
              <option value="">选择已保存配置</option>
              {connectionProfiles.map((profile) => (
                <option key={profile.id} value={profile.id}>
                  {profile.name} · {profile.host}:{profile.port}
                </option>
              ))}
            </select>
          </label>
          <label>
            配置名称
            <input
              value={profileName}
              onChange={(e) => setProfileName(e.target.value)}
              placeholder="例如 本机 Hub"
            />
          </label>
          <button type="button" className="ghost" onClick={saveConnectionProfile}>
            保存配置
          </button>
          <button
            type="button"
            className="ghost"
            disabled={!selectedProfileId}
            onClick={deleteConnectionProfile}
          >
            删除配置
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
            <h2>控制線</h2>
            <p className="hint">
              對應 DTR / RTS / BREAK。需要 Hub 的 <code>api.can_control</code> 與本頁寫鎖；mock
              只用來測試 API，不宣稱有實體電氣效果。
            </p>
            <div className="row control-row">
              <button
                type="button"
                className="ghost"
                disabled={!online || busy || !leaseToken}
                onClick={() => void onControl({ op: "dtr", level: true })}
              >
                DTR ON
              </button>
              <button
                type="button"
                className="ghost"
                disabled={!online || busy || !leaseToken}
                onClick={() => void onControl({ op: "dtr", level: false })}
              >
                DTR OFF
              </button>
              <button
                type="button"
                className="ghost"
                disabled={!online || busy || !leaseToken}
                onClick={() => void onControl({ op: "rts", level: true })}
              >
                RTS ON
              </button>
              <button
                type="button"
                className="ghost"
                disabled={!online || busy || !leaseToken}
                onClick={() => void onControl({ op: "rts", level: false })}
              >
                RTS OFF
              </button>
              <label>
                BREAK (ms)
                <input
                  className="interval-input"
                  type="number"
                  min={1}
                  max={1000}
                  value={breakDurationMs}
                  onChange={(e) => setBreakDurationMs(Math.min(1000, Math.max(1, Number(e.target.value) || 1)))}
                />
              </label>
              <button
                type="button"
                className="ghost"
                disabled={!online || busy || !leaseToken}
                onClick={() => void onControl({ op: "break", duration_ms: breakDurationMs })}
              >
                發送 BREAK
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
                onKeyDown={(e) => {
                  if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
                    e.preventDefault();
                    onSendText();
                  }
                }}
                placeholder="例如 AT 或任意指令"
              />
            </label>
            <div className="row composer-options">
              <label>
                行尾
                <select
                  value={lineEnding}
                  onChange={(e) => setLineEnding(e.target.value as LineEnding)}
                >
                  <option value="none">无结尾</option>
                  <option value="lf">LF (\\n)</option>
                  <option value="cr">CR (\\r)</option>
                  <option value="crlf">CRLF (\\r\\n)</option>
                </select>
              </label>
              <span className="hint inline-hint">Ctrl/⌘ + Enter 发送</span>
            </div>
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
            <div className="row composer-options">
              <label>
                校验和
                <select
                  value={hexChecksum}
                  onChange={(e) => setHexChecksum(e.target.value as HexChecksum)}
                >
                  <option value="none">不添加校验</option>
                  <option value="sum8">SUM8</option>
                  <option value="xor8">XOR8</option>
                  <option value="crc16-modbus">CRC16-Modbus (Lo/Hi)</option>
                  <option value="crc16-ccitt">CRC16-CCITT (Hi/Lo)</option>
                </select>
              </label>
              {hexPreview && (
                <span className={`hint inline-hint ${hexPreview.startsWith("错误") ? "error" : ""}`}>
                  发送预览：<code>{hexPreview}</code>
                </span>
              )}
            </div>
            <button type="button" disabled={!online || busy} onClick={() => void onSendHex()}>
              送出 Hex
            </button>
            <div className="row auto-send-row">
              <label className="check">
                <input
                  type="checkbox"
                  checked={autoSend}
                  onChange={(e) => setAutoSend(e.target.checked)}
                  disabled={!online || busy}
                />
                定时发送当前内容
              </label>
              <label>
                模式
                <select value={autoMode} onChange={(e) => setAutoMode(e.target.value as CommandMode)}>
                  <option value="text">文本</option>
                  <option value="hex">Hex</option>
                </select>
              </label>
              <label>
                间隔 (ms)
                <input
                  className="interval-input"
                  type="number"
                  min={50}
                  max={86400000}
                  step={50}
                  value={autoIntervalMs}
                  onChange={(e) => setAutoIntervalMs(Math.max(50, Number(e.target.value) || 50))}
                />
              </label>
            </div>
            <p className="hint">
              可靠回传请用 HTTP 写入。定时发送复用同一写锁和安全仲裁，不会绕过 Hub。
            </p>
          </section>

          <section className="panel quick-panel">
            <div className="log-head">
              <div>
                <h2>快捷指令</h2>
                <p className="hint">把常用 ASCII/Hex 命令保存到本浏览器，一键发送。</p>
              </div>
              {editingCommandId && (
                <button type="button" className="ghost" onClick={resetQuickEditor}>
                  取消编辑
                </button>
              )}
            </div>
            <div className="quick-editor">
              <label>
                名称
                <input
                  value={quickName}
                  onChange={(e) => setQuickName(e.target.value)}
                  placeholder="例如 读取版本"
                />
              </label>
              <label>
                类型
                <select value={quickMode} onChange={(e) => setQuickMode(e.target.value as CommandMode)}>
                  <option value="text">文本</option>
                  <option value="hex">Hex</option>
                </select>
              </label>
              <label className="quick-payload">
                内容
                <input
                  value={quickPayload}
                  onChange={(e) => setQuickPayload(e.target.value)}
                  placeholder={quickMode === "text" ? "AT+VERSION?" : "AA 55 01 00"}
                />
              </label>
              {quickMode === "text" && (
                <label>
                  行尾
                  <select
                    value={quickLineEnding}
                    onChange={(e) => setQuickLineEnding(e.target.value as LineEnding)}
                  >
                    <option value="none">无结尾</option>
                    <option value="lf">LF</option>
                    <option value="cr">CR</option>
                    <option value="crlf">CRLF</option>
                  </select>
                </label>
              )}
              <button type="button" disabled={busy} onClick={saveQuickCommand}>
                {editingCommandId ? "更新指令" : "保存指令"}
              </button>
            </div>
            <div className="quick-list">
              {quickCommands.length === 0 && <p className="muted">还没有快捷指令。</p>}
              {quickCommands.map((command) => (
                <div className="quick-item" key={command.id}>
                  <div className="quick-item-main">
                    <strong>{command.name}</strong>
                    <span className="tag">{command.mode === "text" ? "文本" : "Hex"}</span>
                    <code>{command.payload}</code>
                    {command.mode === "text" && <span className="muted">{command.lineEnding}</span>}
                  </div>
                  <div className="row">
                    <button
                      type="button"
                      className="small"
                      disabled={!online || busy}
                      onClick={() => void transmit(command.mode, command.payload, command.lineEnding)}
                    >
                      发送
                    </button>
                    <button type="button" className="ghost small" onClick={() => editQuickCommand(command)}>
                      编辑
                    </button>
                    <button type="button" className="ghost small" onClick={() => deleteQuickCommand(command)}>
                      删除
                    </button>
                  </div>
                </div>
              ))}
            </div>
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
                <label>
                  显示
                  <select value={displayMode} onChange={(e) => setDisplayMode(e.target.value as typeof displayMode)}>
                    <option value="both">文本 + Hex</option>
                    <option value="text">仅文本</option>
                    <option value="hex">仅 Hex</option>
                  </select>
                </label>
                <label>
                  解析
                  <select
                    value={streamProtocol}
                    onChange={(e) => setStreamProtocol(e.target.value as StreamProtocol)}
                  >
                    <option value="raw">RawData</option>
                    <option value="firewater">FireWater CSV</option>
                    <option value="justfloat">JustFloat LE</option>
                    <option value="nmea0183">NMEA 0183</option>
                    <option value="slip">SLIP / RFC 1055</option>
                    <option value="cobs">COBS</option>
                    <option value="modbusrtu">Modbus RTU</option>
                  </select>
                </label>
                <label className="check">
                  <input
                    type="checkbox"
                    checked={showTimestamp}
                    onChange={(e) => setShowTimestamp(e.target.checked)}
                  />
                  时间戳
                </label>
                <button type="button" className="ghost" onClick={() => setLogs([])}>
                  清空
                </button>
                <button type="button" className="ghost" onClick={exportVisibleLogs}>
                  导出日志
                </button>
              </div>
            </div>
            <div className="log" ref={logBoxRef}>
              {logs.length === 0 && <div className="muted">尚無訊息</div>}
              {logs.map((l) => (
                <div key={l.id} className={`line ${l.kind}`}>
                  <span className="ts">{showTimestamp ? l.ts : ""}</span>
                  <span className="kind">{l.kind.toUpperCase()}</span>
                  {displayMode !== "hex" && <span className="msg">{l.text}</span>}
                  {displayMode !== "text" && l.hex && <span className="hex">{l.hex}</span>}
                </div>
              ))}
            </div>
          </section>

          <WaveformPanel
            samples={waveSamples}
            channel={waveChannel}
            onChannelChange={setWaveChannel}
            onClear={() => setWaveSamples([])}
            protocolLabel={protocolLabel(streamProtocol)}
          />
          <ProtocolInspectorPanel
            protocol={streamProtocol}
            frames={protocolFrames}
            onClear={() => setProtocolFrames([])}
          />
        </div>
      )}

      {tab === "events" && (
        <>
          <EventLedgerPanel
          status={ledgerStatus}
          events={ledgerEvents}
          loading={ledgerLoading}
          error={ledgerError}
          online={online}
          onRefresh={() => void refreshLedger(conn, apiBearerToken)}
          onExport={() => void exportLedger()}
          />
          <MetricsPanel
            text={metricsText}
            loading={metricsLoading}
            error={metricsError}
            online={online}
            onRefresh={() => void refreshMetrics(conn, apiBearerToken)}
            onExport={exportMetrics}
          />
        </>
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
