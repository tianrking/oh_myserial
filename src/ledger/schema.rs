use std::{collections::BTreeSet, fmt};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const EVENT_SCHEMA: &str = "ohmyserial.event";
pub const EVENT_VERSION: u16 = 1;
pub const DEFAULT_PORT_ID: &str = "default";

/// A canonical v1 ledger event.
///
/// Sequence numbers are scoped to `session_id`, start at one, and are assigned
/// by [`crate::ledger::Ledger`]. `mono_us` is measured from the creation of the
/// ledger process and is therefore useful for ordering and durations, not as a
/// wall-clock timestamp.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub schema: String,
    pub version: u16,
    pub session_id: Uuid,
    pub seq: u64,
    pub ts_utc: String,
    pub mono_us: u64,
    pub port_id: String,
    pub connection_epoch: u64,
    #[serde(flatten)]
    pub event: EventPayload,
}

impl EventEnvelope {
    pub(crate) fn new(
        session_id: Uuid,
        seq: u64,
        now: DateTime<Utc>,
        mono_us: u64,
        epoch: u64,
        event: EventPayload,
    ) -> Self {
        Self {
            schema: EVENT_SCHEMA.to_owned(),
            version: EVENT_VERSION,
            session_id,
            seq,
            ts_utc: now.to_rfc3339_opts(SecondsFormat::Micros, true),
            mono_us,
            port_id: DEFAULT_PORT_ID.to_owned(),
            connection_epoch: epoch,
            event,
        }
    }

    pub fn event_type(&self) -> EventType {
        self.event.event_type()
    }

    pub fn validate(&self) -> Result<(), SchemaError> {
        if self.schema != EVENT_SCHEMA {
            return Err(SchemaError::Schema(self.schema.clone()));
        }
        if self.version != EVENT_VERSION {
            return Err(SchemaError::Version(self.version));
        }
        if self.port_id != DEFAULT_PORT_ID {
            return Err(SchemaError::PortId(self.port_id.clone()));
        }
        if self.seq == 0 {
            return Err(SchemaError::ZeroSequence);
        }
        DateTime::parse_from_rfc3339(&self.ts_utc)
            .map_err(|_| SchemaError::Timestamp(self.ts_utc.clone()))?;
        self.event.validate()
    }

    /// Serialized byte size used for bounded in-memory accounting.
    pub(crate) fn encoded_len(&self) -> usize {
        serde_json::to_vec(self).map_or(0, |bytes| bytes.len())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum EventPayload {
    Rx(BytesPayload),
    Tx(TxPayload),
    Connection(ConnectionPayload),
    Control(ControlPayload),
    Gap(GapPayload),
}

impl EventPayload {
    pub fn rx(bytes: impl AsRef<[u8]>) -> Self {
        Self::Rx(BytesPayload::from_bytes(bytes))
    }

    /// Construct a TX event after a serial write has been acknowledged.
    ///
    /// The schema deliberately accepts only the public actor label and bytes;
    /// lease bearer tokens cannot enter the ledger model.
    pub fn tx(actor: impl Into<String>, bytes: impl AsRef<[u8]>) -> Self {
        Self::tx_from(actor, None, bytes)
    }

    pub fn tx_from(
        actor: impl Into<String>,
        client_id: Option<String>,
        bytes: impl AsRef<[u8]>,
    ) -> Self {
        Self::Tx(TxPayload {
            actor: actor.into(),
            client_id,
            bytes: BytesPayload::from_bytes(bytes),
        })
    }

    pub fn event_type(&self) -> EventType {
        match self {
            Self::Rx(_) => EventType::Rx,
            Self::Tx(_) => EventType::Tx,
            Self::Connection(_) => EventType::Connection,
            Self::Control(_) => EventType::Control,
            Self::Gap(_) => EventType::Gap,
        }
    }

    fn validate(&self) -> Result<(), SchemaError> {
        match self {
            Self::Rx(bytes) => bytes.validate(),
            Self::Tx(tx) => {
                if tx.actor.is_empty()
                    || tx.actor.len() > 128
                    || tx.actor.chars().any(char::is_control)
                {
                    return Err(SchemaError::Actor);
                }
                if tx.client_id.as_ref().is_some_and(|v| {
                    v.is_empty() || v.len() > 128 || v.chars().any(char::is_control)
                }) {
                    return Err(SchemaError::ClientId);
                }
                tx.bytes.validate()
            }
            Self::Connection(connection) => {
                if connection.path.is_empty()
                    || connection.path.len() > 1024
                    || connection.baud == 0
                {
                    return Err(SchemaError::Connection);
                }
                if connection.detail.as_ref().is_some_and(|v| v.len() > 1024) {
                    return Err(SchemaError::DetailTooLong);
                }
                Ok(())
            }
            Self::Control(control) => {
                if control.actor.as_ref().is_some_and(|v| {
                    v.is_empty() || v.len() > 128 || v.chars().any(char::is_control)
                }) {
                    return Err(SchemaError::Actor);
                }
                if control.name.is_empty()
                    || control.name.len() > 128
                    || control.name.chars().any(char::is_control)
                {
                    return Err(SchemaError::ControlName);
                }
                if control.value.as_ref().is_some_and(|v| v.len() > 1024) {
                    return Err(SchemaError::DetailTooLong);
                }
                Ok(())
            }
            Self::Gap(gap) => {
                if gap.reason.is_empty()
                    || gap.reason.len() > 1024
                    || gap.actor.as_ref().is_some_and(|v| {
                        v.is_empty() || v.len() > 128 || v.chars().any(char::is_control)
                    })
                    || gap
                        .client_ids
                        .iter()
                        .any(|v| v.is_empty() || v.len() > 128 || v.chars().any(char::is_control))
                {
                    return Err(SchemaError::Gap);
                }
                if let Some(bytes) = &gap.bytes {
                    bytes.validate()?;
                }
                Ok(())
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BytesPayload {
    pub data_base64: String,
    pub len: u64,
}

impl BytesPayload {
    pub fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        let bytes = bytes.as_ref();
        Self {
            data_base64: STANDARD.encode(bytes),
            len: bytes.len() as u64,
        }
    }

    pub fn decode(&self) -> Result<Vec<u8>, SchemaError> {
        let bytes = STANDARD
            .decode(&self.data_base64)
            .map_err(|_| SchemaError::Base64)?;
        if bytes.len() as u64 != self.len {
            return Err(SchemaError::ByteLength {
                declared: self.len,
                decoded: bytes.len() as u64,
            });
        }
        Ok(bytes)
    }

    fn validate(&self) -> Result<(), SchemaError> {
        self.decode().map(|_| ())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TxPayload {
    pub actor: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(flatten)]
    pub bytes: BytesPayload,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionState {
    Connected,
    Disconnected,
    Reconnecting,
    OpenFailed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionPayload {
    pub state: ConnectionState,
    pub path: String,
    pub baud: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlPayload {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GapPayload {
    pub scope: GapScope,
    pub certainty: GapCertainty,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<BytesPayload>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub client_ids: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GapScope {
    RxObservation,
    TxOutcome,
    ClientDelivery,
    Persistence,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GapCertainty {
    Unknown,
    PartialOrUnknown,
    NotDelivered,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    Rx,
    Tx,
    Connection,
    Control,
    Gap,
}

impl fmt::Display for EventType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Rx => "rx",
            Self::Tx => "tx",
            Self::Connection => "connection",
            Self::Control => "control",
            Self::Gap => "gap",
        };
        f.write_str(value)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EventFilter {
    pub event_types: BTreeSet<EventType>,
    pub connection_epoch: Option<u64>,
    pub actor: Option<String>,
    pub contains_bytes: Option<Vec<u8>>,
}

impl EventFilter {
    pub(crate) fn matches(&self, event: &EventEnvelope) -> bool {
        (self.event_types.is_empty() || self.event_types.contains(&event.event_type()))
            && self
                .connection_epoch
                .is_none_or(|epoch| event.connection_epoch == epoch)
            && self.actor.as_ref().is_none_or(|actor| match &event.event {
                EventPayload::Tx(tx) => &tx.actor == actor,
                EventPayload::Control(control) => control.actor.as_ref() == Some(actor),
                EventPayload::Gap(gap) => gap.actor.as_ref() == Some(actor),
                EventPayload::Rx(_) | EventPayload::Connection(_) => false,
            })
            && self.contains_bytes.as_ref().is_none_or(|needle| {
                let haystack = match &event.event {
                    EventPayload::Rx(bytes) => bytes.decode().ok(),
                    EventPayload::Tx(tx) => tx.bytes.decode().ok(),
                    EventPayload::Gap(gap) => gap.bytes.as_ref().and_then(|v| v.decode().ok()),
                    EventPayload::Connection(_) | EventPayload::Control(_) => None,
                };
                haystack.is_some_and(|bytes| {
                    needle.is_empty() || bytes.windows(needle.len()).any(|window| window == needle)
                })
            })
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SchemaError {
    #[error("unsupported event schema {0:?}")]
    Schema(String),
    #[error("unsupported event version {0}")]
    Version(u16),
    #[error("v1 port_id must be default, got {0:?}")]
    PortId(String),
    #[error("event sequence must be greater than zero")]
    ZeroSequence,
    #[error("invalid RFC 3339 UTC timestamp {0:?}")]
    Timestamp(String),
    #[error("invalid standard base64 payload")]
    Base64,
    #[error("byte length mismatch: declared {declared}, decoded {decoded}")]
    ByteLength { declared: u64, decoded: u64 },
    #[error("actor must be 1..=128 non-control characters")]
    Actor,
    #[error("client_id must be 1..=128 non-control characters when present")]
    ClientId,
    #[error("control name must be 1..=128 non-control characters")]
    ControlName,
    #[error("event detail exceeds 1024 bytes")]
    DetailTooLong,
    #[error("connection path must be 1..=1024 bytes and baud must be nonzero")]
    Connection,
    #[error("invalid gap range or reason")]
    Gap,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_are_standard_padded_base64() {
        let payload = BytesPayload::from_bytes([0xfb, 0xff]);
        assert_eq!(payload.data_base64, "+/8=");
        assert_eq!(payload.decode().unwrap(), [0xfb, 0xff]);
    }

    #[test]
    fn canonical_envelope_has_top_level_type_and_payload() {
        let event = EventEnvelope::new(
            Uuid::nil(),
            1,
            DateTime::parse_from_rfc3339("2026-08-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            7,
            3,
            EventPayload::rx(b"ok"),
        );
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["schema"], EVENT_SCHEMA);
        assert_eq!(value["version"], EVENT_VERSION);
        assert_eq!(value["port_id"], DEFAULT_PORT_ID);
        assert_eq!(value["type"], "rx");
        assert_eq!(value["payload"]["data_base64"], "b2s=");
        assert!(value.get("token").is_none());
        event.validate().unwrap();
    }

    #[test]
    fn malformed_bytes_are_rejected() {
        let bad = BytesPayload {
            data_base64: "-_8".to_owned(),
            len: 2,
        };
        assert_eq!(bad.decode().unwrap_err(), SchemaError::Base64);
    }
}
