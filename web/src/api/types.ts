/** ohmyserial hub API 型別（對應 PROTOCOL.zh-TW.md） */

export type EndpointKind = "http" | "websocket" | "tcp" | "pty" | string;

export interface PortStatus {
  path: string;
  baud: number;
  connected: boolean;
  detail: string;
  epoch: number;
}

export interface Endpoint {
  kind: EndpointKind;
  name: string;
  address: string;
  can_read: boolean;
  can_write: boolean;
  note: string;
}

export interface ClientInfo {
  id: string;
  name: string;
  kind: string;
  can_read: boolean;
  can_write: boolean;
  primary_eligible: boolean;
}

export interface Stats {
  rx_bytes: number;
  tx_bytes: number;
  rx_drops: number;
  tx_denies: number;
}

export interface StatusSnapshot {
  port: PortStatus;
  tx_mode: string;
  lock_owner: string | null;
  lock_expires_ms: number | null;
  endpoints: Endpoint[];
  clients: ClientInfo[];
  stats: Stats;
}

export interface EndpointsResponse {
  real: PortStatus;
  endpoints: Endpoint[];
  connected_clients: number;
}

export interface WriteResponse {
  ok: boolean;
  error?: string;
  bytes: number;
}

export interface LockResponse {
  ok: boolean;
  error?: string;
  lock?: { owner: string; expires_ms: number; lease_token: string };
}

export interface HealthResponse {
  ok: boolean;
  service: string;
}

export interface ConnectionConfig {
  host: string;
  port: number;
}

export function httpBase(cfg: ConnectionConfig): string {
  return `http://${cfg.host}:${cfg.port}`;
}

export function wsUrl(cfg: ConnectionConfig): string {
  return `ws://${cfg.host}:${cfg.port}/v1/stream`;
}
