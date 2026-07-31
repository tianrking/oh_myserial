import type {
  ConnectionConfig,
  EndpointsResponse,
  HealthResponse,
  LockResponse,
  StatusSnapshot,
  WriteResponse,
} from "./types";
import { httpBase } from "./types";

function authHeaders(bearerToken?: string): Record<string, string> {
  return bearerToken ? { authorization: `Bearer ${bearerToken}` } : {};
}

async function getJson<T>(url: string, bearerToken?: string): Promise<T> {
  const res = await fetch(url, {
    method: "GET",
    headers: authHeaders(bearerToken),
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

export const hubApi = {
  health: (cfg: ConnectionConfig) =>
    getJson<HealthResponse>(`${httpBase(cfg)}/v1/health`),

  status: (cfg: ConnectionConfig, bearerToken?: string) =>
    getJson<StatusSnapshot>(`${httpBase(cfg)}/v1/status`, bearerToken),

  endpoints: (cfg: ConnectionConfig, bearerToken?: string) =>
    getJson<EndpointsResponse>(`${httpBase(cfg)}/v1/endpoints`, bearerToken),

  clients: (cfg: ConnectionConfig, bearerToken?: string) =>
    getJson<StatusSnapshot["clients"]>(`${httpBase(cfg)}/v1/clients`, bearerToken),

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
};
