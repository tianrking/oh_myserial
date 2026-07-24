import type {
  ConnectionConfig,
  EndpointsResponse,
  HealthResponse,
  LockResponse,
  StatusSnapshot,
  WriteResponse,
} from "./types";
import { httpBase } from "./types";

async function getJson<T>(url: string): Promise<T> {
  const res = await fetch(url, { method: "GET" });
  if (!res.ok) throw new Error(`HTTP ${res.status} ${res.statusText}`);
  return res.json() as Promise<T>;
}

async function sendJson<T>(
  url: string,
  method: string,
  body?: unknown,
): Promise<T> {
  const res = await fetch(url, {
    method,
    headers: body ? { "content-type": "application/json" } : undefined,
    body: body ? JSON.stringify(body) : undefined,
  });
  if (!res.ok) throw new Error(`HTTP ${res.status} ${res.statusText}`);
  return res.json() as Promise<T>;
}

export const hubApi = {
  health: (cfg: ConnectionConfig) =>
    getJson<HealthResponse>(`${httpBase(cfg)}/v1/health`),

  status: (cfg: ConnectionConfig) =>
    getJson<StatusSnapshot>(`${httpBase(cfg)}/v1/status`),

  endpoints: (cfg: ConnectionConfig) =>
    getJson<EndpointsResponse>(`${httpBase(cfg)}/v1/endpoints`),

  clients: (cfg: ConnectionConfig) =>
    getJson<StatusSnapshot["clients"]>(`${httpBase(cfg)}/v1/clients`),

  writeText: (
    cfg: ConnectionConfig,
    text: string,
    opts?: { newline?: boolean; as_client?: string },
  ) =>
    sendJson<WriteResponse>(`${httpBase(cfg)}/v1/write`, "POST", {
      text,
      newline: opts?.newline ?? true,
      as_client: opts?.as_client ?? "web-ui",
    }),

  writeHex: (
    cfg: ConnectionConfig,
    hex: string,
    opts?: { as_client?: string },
  ) =>
    sendJson<WriteResponse>(`${httpBase(cfg)}/v1/write`, "POST", {
      hex,
      as_client: opts?.as_client ?? "web-ui",
    }),

  lock: (cfg: ConnectionConfig, as_client = "web-ui") =>
    sendJson<LockResponse>(`${httpBase(cfg)}/v1/lock`, "POST", { as_client }),

  unlock: (cfg: ConnectionConfig, as_client = "web-ui") =>
    sendJson<LockResponse>(`${httpBase(cfg)}/v1/lock`, "DELETE", {
      as_client,
    }),
};
