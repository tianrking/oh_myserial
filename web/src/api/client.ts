import type {
  ConnectionConfig,
  ControlResponse,
  EndpointsResponse,
  HealthResponse,
  LedgerEventsQuery,
  LedgerEventsResponse,
  LedgerStatus,
  LockResponse,
  StatusSnapshot,
  WriteResponse,
} from "./types";
import { httpBase } from "./types";

function authHeaders(bearerToken?: string): Record<string, string> {
  return bearerToken ? { authorization: `Bearer ${bearerToken}` } : {};
}

async function getJson<T>(
  url: string,
  bearerToken?: string,
  signal?: AbortSignal,
): Promise<T> {
  const res = await fetch(url, {
    method: "GET",
    headers: authHeaders(bearerToken),
    signal,
  });
  if (!res.ok) throw new Error(`HTTP ${res.status} ${res.statusText}`);
  return res.json() as Promise<T>;
}

async function sendJson<T>(
  url: string,
  method: string,
  body?: unknown,
  bearerToken?: string,
): Promise<T> {
  const res = await fetch(url, {
    method,
    headers: {
      ...authHeaders(bearerToken),
      ...(body ? { "content-type": "application/json" } : {}),
    },
    body: body ? JSON.stringify(body) : undefined,
  });
  if (!res.ok) throw new Error(`HTTP ${res.status} ${res.statusText}`);
  return res.json() as Promise<T>;
}

function eventQueryString(query: LedgerEventsQuery): string {
  const params = new URLSearchParams();
  if (query.afterSeq !== undefined) params.set("after_seq", String(query.afterSeq));
  if (query.throughSeq !== undefined) params.set("through_seq", String(query.throughSeq));
  if (query.limit !== undefined) params.set("limit", String(query.limit));
  if (query.types?.length) params.set("type", query.types.join(","));
  if (query.connectionEpoch !== undefined) {
    params.set("connection_epoch", String(query.connectionEpoch));
  }
  if (query.actor) params.set("actor", query.actor);
  if (query.containsHex) params.set("contains_hex", query.containsHex);
  const encoded = params.toString();
  return encoded ? `?${encoded}` : "";
}

export const hubApi = {
  health: (cfg: ConnectionConfig) =>
    getJson<HealthResponse>(`${httpBase(cfg)}/v1/health`),

  status: (cfg: ConnectionConfig, bearerToken?: string) =>
    getJson<StatusSnapshot>(`${httpBase(cfg)}/v1/status`, bearerToken),

  endpoints: (cfg: ConnectionConfig, bearerToken?: string) =>
    getJson<EndpointsResponse>(`${httpBase(cfg)}/v1/endpoints`, bearerToken),

  clients: (cfg: ConnectionConfig, bearerToken?: string) =>
    getJson<StatusSnapshot["clients"]>(`${httpBase(cfg)}/v1/clients`, bearerToken),

  eventsStatus: (
    cfg: ConnectionConfig,
    bearerToken?: string,
    signal?: AbortSignal,
  ) =>
    getJson<LedgerStatus>(
      `${httpBase(cfg)}/v1/events/status`,
      bearerToken,
      signal,
    ),

  events: (
    cfg: ConnectionConfig,
    query: LedgerEventsQuery,
    bearerToken?: string,
    signal?: AbortSignal,
  ) =>
    getJson<LedgerEventsResponse>(
      `${httpBase(cfg)}/v1/events${eventQueryString(query)}`,
      bearerToken,
      signal,
    ),

  writeText: (
    cfg: ConnectionConfig,
    text: string,
    opts?: {
      newline?: boolean;
      as_client?: string;
      lease_token?: string;
      bearer_token?: string;
    },
  ) =>
    sendJson<WriteResponse>(
      `${httpBase(cfg)}/v1/write`,
      "POST",
      {
        text,
        newline: opts?.newline ?? true,
        as_client: opts?.as_client ?? "web-ui",
        lease_token: opts?.lease_token,
      },
      opts?.bearer_token,
    ),

  writeHex: (
    cfg: ConnectionConfig,
    hex: string,
    opts?: { as_client?: string; lease_token?: string; bearer_token?: string },
  ) =>
    sendJson<WriteResponse>(
      `${httpBase(cfg)}/v1/write`,
      "POST",
      {
        hex,
        as_client: opts?.as_client ?? "web-ui",
        lease_token: opts?.lease_token,
      },
      opts?.bearer_token,
    ),

  lock: (
    cfg: ConnectionConfig,
    as_client = "web-ui",
    lease_token?: string,
    bearerToken?: string,
  ) =>
    sendJson<LockResponse>(
      `${httpBase(cfg)}/v1/lock`,
      "POST",
      { as_client, lease_token },
      bearerToken,
    ),

  unlock: (cfg: ConnectionConfig, lease_token: string, bearerToken?: string) =>
    sendJson<LockResponse>(
      `${httpBase(cfg)}/v1/lock`,
      "DELETE",
      { lease_token },
      bearerToken,
    ),

  control: (
    cfg: ConnectionConfig,
    body:
      | { op: "dtr" | "rts"; level: boolean; as_client?: string; lease_token?: string }
      | { op: "break"; duration_ms: number; as_client?: string; lease_token?: string },
    bearerToken?: string,
  ) =>
    sendJson<ControlResponse>(`${httpBase(cfg)}/v1/control`, "POST", body, bearerToken),
};
