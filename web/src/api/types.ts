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

export interface ControlResponse {
  ok: boolean;
  error?: string;
}

export interface HealthResponse {
  ok: boolean;
  service: string;
}

export type LedgerPersistenceState = "disabled" | "active" | "degraded" | "sealed";

export type LedgerEventType = "rx" | "tx" | "connection" | "control" | "gap";

export interface LedgerBytesPayload {
  data_base64: string;
  len: number;
}

interface LedgerEventBase {
  schema: "ohmyserial.event";
  version: 1;
  session_id: string;
  seq: number;
  ts_utc: string;
  mono_us: number;
  port_id: string;
  connection_epoch: number;
}

export type LedgerEvent =
  | (LedgerEventBase & {
      type: "rx";
      payload: LedgerBytesPayload;
    })
  | (LedgerEventBase & {
      type: "tx";
      payload: LedgerBytesPayload & {
        actor: string;
        client_id?: string;
      };
    })
  | (LedgerEventBase & {
      type: "connection";
      payload: {
        state: "connected" | "disconnected" | "reconnecting" | "open_failed";
        path: string;
        baud: number;
        detail?: string;
      };
    })
  | (LedgerEventBase & {
      type: "control";
      payload: {
        actor?: string;
        name: string;
        value?: string;
      };
    })
  | (LedgerEventBase & {
      type: "gap";
      payload: {
        scope: "rx_observation" | "tx_outcome" | "client_delivery" | "persistence";
        certainty: "unknown" | "partial_or_unknown" | "not_delivered";
        reason: string;
        bytes?: LedgerBytesPayload;
        actor?: string;
        client_ids?: string[];
      };
    });

export interface LedgerStatus {
  session_id: string;
  newest_seq: number;
  oldest_available_seq: number | null;
  retained_events: number;
  retained_bytes: number;
  evicted_events: number;
  persistence: LedgerPersistenceState;
  persistence_directory?: string;
  persistence_error?: string;
  sealed: boolean;
  recovery?: unknown;
  stale_recovery?: unknown;
}

export interface LedgerEventPage {
  events: LedgerEvent[];
  incomplete: boolean;
  missing_through_seq?: number;
  oldest_available_seq?: number;
  newest_seq: number;
  next_after_seq: number;
  has_more: boolean;
}

export interface LedgerEventsResponse {
  ok: true;
  session_id: string;
  page: LedgerEventPage;
}

export interface LedgerEventsQuery {
  afterSeq?: number;
  throughSeq?: number;
  limit?: number;
  types?: LedgerEventType[];
  connectionEpoch?: number;
  actor?: string;
  containsHex?: string;
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
