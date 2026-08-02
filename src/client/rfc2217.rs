//! RFC2217 (Telnet COM-PORT-OPTION) client adapter.
//!
//! RFC2217 is intentionally a separate adapter from raw TCP: Telnet control
//! bytes are parsed and escaped, while serial bytes continue through the same
//! broker arbitration and evidence ledger. The listener is loopback-only in
//! configuration because RFC2217 has no bearer authentication of its own.

use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::broker::{Broker, ControlCommand};
use crate::config::SerialSettings;

const IAC: u8 = 255;
const DONT: u8 = 254;
const DO: u8 = 253;
const WONT: u8 = 252;
const WILL: u8 = 251;
const SB: u8 = 250;
const SE: u8 = 240;
const COM_PORT_OPTION: u8 = 44;

const SET_BAUDRATE: u8 = 1;
const SET_DATASIZE: u8 = 2;
const SET_PARITY: u8 = 3;
const SET_STOPSIZE: u8 = 4;
const SET_CONTROL: u8 = 5;
const SIGNATURE: u8 = 0;
const FLOWCONTROL_SUSPEND: u8 = 8;
const FLOWCONTROL_RESUME: u8 = 9;
const SET_LINESTATE_MASK: u8 = 10;
const SET_MODEMSTATE_MASK: u8 = 11;
const PURGE_DATA: u8 = 12;

/// Start one RFC2217 listener. The bind is performed before returning so a
/// profile never advertises a dead endpoint.
pub async fn spawn_rfc2217_listener(
    broker: Broker,
    name: String,
    bind: String,
    can_read: bool,
    can_write: bool,
    can_control: bool,
    initial_settings: SerialSettings,
) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    let listener = TcpListener::bind(&bind)
        .await
        .map_err(|error| anyhow::anyhow!("RFC2217 bind {bind} failed: {error}"))?;
    initial_settings
        .validate()
        .map_err(|error| anyhow::anyhow!("invalid RFC2217 initial line settings: {error}"))?;

    Ok(tokio::spawn(async move {
        tracing::info!("RFC2217 listener '{name}' listening on {bind}");
        broker
            .log()
            .event(&format!("rfc2217_listen name={name} bind={bind}"));
        loop {
            match listener.accept().await {
                Ok((stream, peer)) => {
                    let broker = broker.clone();
                    let name = name.clone();
                    let settings = initial_settings.clone();
                    tokio::spawn(async move {
                        if let Err(error) = handle_connection(
                            broker,
                            name,
                            stream,
                            can_read,
                            can_write,
                            can_control,
                            settings,
                        )
                        .await
                        {
                            tracing::debug!("RFC2217 connection {peer} closed: {error}");
                        }
                    });
                }
                Err(error) => {
                    tracing::warn!("RFC2217 accept error: {error}");
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
        }
    }))
}

async fn handle_connection(
    broker: Broker,
    name: String,
    stream: TcpStream,
    can_read: bool,
    can_write: bool,
    can_control: bool,
    mut settings: SerialSettings,
) -> anyhow::Result<()> {
    let peer = stream.peer_addr()?;
    let actor = format!("rfc2217@{peer}");
    let (id, mut from_broker) = broker.register_client(
        name.clone(),
        format!("rfc2217@{peer}"),
        can_read,
        can_write,
        None,
    );
    let _registration = broker.client_registration(id);
    let (mut reader, mut writer) = stream.into_split();

    // RFC2217 negotiation: the server offers COM-PORT-OPTION and accepts the
    // client's option. All subsequent IAC bytes are handled by the decoder.
    writer
        .write_all(&[IAC, WILL, COM_PORT_OPTION, IAC, DO, COM_PORT_OPTION])
        .await?;

    let mut decoder = TelnetDecoder::default();
    let mut read_buf = [0_u8; 4096];
    loop {
        tokio::select! {
            outbound = from_broker.recv(), if can_read => {
                match outbound {
                    Some(data) => writer.write_all(&escape_iac(&data)).await?,
                    None => break,
                }
            }
            inbound = reader.read(&mut read_buf) => {
                let n = inbound?;
                if n == 0 { break; }
                let mut payload = Vec::new();
                let mut replies = Vec::new();
                let mut subnegotiations = Vec::new();
                decoder.feed(&read_buf[..n], &mut payload, &mut replies, &mut subnegotiations);
                if !replies.is_empty() {
                    writer.write_all(&replies).await?;
                }
                if !payload.is_empty() {
                    if !can_write {
                        broker.log().event(&format!("rfc2217_tx_denied actor={actor}"));
                    } else if let Err(error) = broker.client_tx(id, Bytes::from(payload)).await {
                        broker.log().event(&format!("rfc2217_tx_rejected actor={actor} error={error}"));
                    }
                }
                for subnegotiation in subnegotiations {
                    let response = handle_subnegotiation(
                        &broker,
                        &actor,
                        can_control,
                        &mut settings,
                        &subnegotiation,
                    ).await;
                    writer.write_all(&response).await?;
                }
            }
        }
    }
    Ok(())
}

#[derive(Default)]
struct TelnetDecoder {
    state: DecoderState,
}

#[derive(Default)]
enum DecoderState {
    #[default]
    Data,
    Iac,
    Negotiation(u8),
    Subneg(Vec<u8>),
    SubnegIac(Vec<u8>),
}

impl TelnetDecoder {
    fn feed(
        &mut self,
        bytes: &[u8],
        payload: &mut Vec<u8>,
        replies: &mut Vec<u8>,
        subnegotiations: &mut Vec<Vec<u8>>,
    ) {
        for &byte in bytes {
            let state = std::mem::take(&mut self.state);
            self.state = match state {
                DecoderState::Data if byte == IAC => DecoderState::Iac,
                DecoderState::Data => {
                    payload.push(byte);
                    DecoderState::Data
                }
                DecoderState::Iac => match byte {
                    IAC => {
                        payload.push(IAC);
                        DecoderState::Data
                    }
                    WILL | WONT | DO | DONT => DecoderState::Negotiation(byte),
                    SB => DecoderState::Subneg(Vec::new()),
                    _ => DecoderState::Data,
                },
                DecoderState::Negotiation(command) => {
                    replies.extend(negotiation_reply(command, byte));
                    DecoderState::Data
                }
                DecoderState::Subneg(mut data) => {
                    if byte == IAC {
                        DecoderState::SubnegIac(data)
                    } else {
                        data.push(byte);
                        DecoderState::Subneg(data)
                    }
                }
                DecoderState::SubnegIac(mut data) => match byte {
                    IAC => {
                        data.push(IAC);
                        DecoderState::Subneg(data)
                    }
                    SE => {
                        subnegotiations.push(data);
                        DecoderState::Data
                    }
                    _ => DecoderState::Data,
                },
            };
        }
    }
}

fn negotiation_reply(command: u8, option: u8) -> Vec<u8> {
    match command {
        WILL if option == COM_PORT_OPTION => vec![IAC, DO, option],
        WILL => vec![IAC, DONT, option],
        WONT => vec![IAC, DONT, option],
        DO if option == COM_PORT_OPTION => vec![IAC, WILL, option],
        DO => vec![IAC, WONT, option],
        DONT => vec![IAC, WONT, option],
        _ => Vec::new(),
    }
}

async fn handle_subnegotiation(
    broker: &Broker,
    actor: &str,
    can_control: bool,
    settings: &mut SerialSettings,
    frame: &[u8],
) -> Vec<u8> {
    if frame.first().copied() != Some(COM_PORT_OPTION) || frame.len() < 2 {
        return Vec::new();
    }
    let command = frame[1];
    let value = &frame[2..];
    let mut response_value = value.to_vec();
    let result = match command {
        SIGNATURE => {
            response_value = b"ohmyserial-rfc2217".to_vec();
            Ok(())
        }
        SET_BAUDRATE if value.len() == 4 => {
            let baud = u32::from_be_bytes([value[0], value[1], value[2], value[3]]);
            let mut next = settings.clone();
            next.baud = baud;
            let result = configure_with_lease(broker, actor, can_control, next.clone()).await;
            if result.is_ok() {
                *settings = next;
            }
            result
        }
        SET_DATASIZE if value.len() == 1 => {
            let mut next = settings.clone();
            next.databits = value[0];
            let result = configure_with_lease(broker, actor, can_control, next.clone()).await;
            if result.is_ok() {
                *settings = next;
            }
            result
        }
        SET_PARITY if value.len() == 1 => {
            let parity = match value[0] {
                1 => "odd",
                2 => "even",
                3 | 4 => return rfc_error_reply(command, value),
                _ => "none",
            };
            let mut next = settings.clone();
            next.parity = parity.into();
            let result = configure_with_lease(broker, actor, can_control, next.clone()).await;
            if result.is_ok() {
                *settings = next;
            }
            result
        }
        SET_STOPSIZE if value.len() == 1 => {
            let stopbits = match value[0] {
                1 => 1,
                2 => 2,
                _ => return rfc_error_reply(command, value),
            };
            let mut next = settings.clone();
            next.stopbits = stopbits;
            let result = configure_with_lease(broker, actor, can_control, next.clone()).await;
            if result.is_ok() {
                *settings = next;
            }
            result
        }
        SET_CONTROL if value.len() == 1 => {
            let control = match value[0] {
                1 => Some(ControlCommand::Dtr(true)),
                2 => Some(ControlCommand::Dtr(false)),
                3 => Some(ControlCommand::Rts(true)),
                4 => Some(ControlCommand::Rts(false)),
                // 5/6 request driver-managed flow control and are represented
                // by the framing setting rather than a physical line toggle.
                5 | 6 => None,
                _ => return rfc_error_reply(command, value),
            };
            match control {
                Some(control) => control_with_lease(broker, actor, can_control, control).await,
                None => Ok(()),
            }
        }
        FLOWCONTROL_SUSPEND | FLOWCONTROL_RESUME | SET_LINESTATE_MASK | SET_MODEMSTATE_MASK
        | PURGE_DATA => Ok(()),
        _ => Err(format!("unsupported RFC2217 command {command}")),
    };

    if let Err(error) = result {
        broker.log().event(&format!(
            "rfc2217_command_rejected actor={actor} command={command} error={error}"
        ));
        response_value.clear();
    }
    rfc_reply(command, &response_value)
}

fn rfc_error_reply(command: u8, value: &[u8]) -> Vec<u8> {
    rfc_reply(command, value)
}

fn rfc_reply(command: u8, value: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(value.len() + 6);
    body.extend_from_slice(&[IAC, SB, COM_PORT_OPTION, command.saturating_add(100)]);
    body.extend(escape_iac(value));
    body.extend_from_slice(&[IAC, SE]);
    body
}

fn escape_iac(bytes: &[u8]) -> Vec<u8> {
    let mut escaped = Vec::with_capacity(bytes.len());
    for &byte in bytes {
        escaped.push(byte);
        if byte == IAC {
            escaped.push(IAC);
        }
    }
    escaped
}

async fn configure_with_lease(
    broker: &Broker,
    actor: &str,
    can_control: bool,
    settings: SerialSettings,
) -> Result<(), String> {
    if !can_control {
        return Err("RFC2217 line configuration is disabled".into());
    }
    let lease = broker.acquire_lock(actor)?;
    let result = broker
        .serial_configure(actor, Some(&lease.lease_token), settings)
        .await;
    let _ = broker.release_lock(Some(&lease.lease_token));
    result
}

async fn control_with_lease(
    broker: &Broker,
    actor: &str,
    can_control: bool,
    command: ControlCommand,
) -> Result<(), String> {
    if !can_control {
        return Err("RFC2217 control-line negotiation is disabled".into());
    }
    let lease = broker.acquire_lock(actor)?;
    let result = broker
        .serial_control(actor, Some(&lease.lease_token), command)
        .await;
    let _ = broker.release_lock(Some(&lease.lease_token));
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn telnet_decoder_preserves_iac_payload_and_subnegotiation() {
        let mut decoder = TelnetDecoder::default();
        let mut payload = Vec::new();
        let mut replies = Vec::new();
        let mut subnegotiations = Vec::new();
        decoder.feed(
            &[
                b'A',
                IAC,
                IAC,
                b'B',
                IAC,
                WILL,
                COM_PORT_OPTION,
                IAC,
                SB,
                COM_PORT_OPTION,
                SET_DATASIZE,
                8,
                IAC,
                SE,
            ],
            &mut payload,
            &mut replies,
            &mut subnegotiations,
        );
        assert_eq!(payload, b"A\xffB");
        assert_eq!(replies, vec![IAC, DO, COM_PORT_OPTION]);
        assert_eq!(
            subnegotiations,
            vec![vec![COM_PORT_OPTION, SET_DATASIZE, 8]]
        );
    }

    #[test]
    fn raw_data_escapes_iac() {
        assert_eq!(escape_iac(&[1, IAC, 2]), vec![1, IAC, IAC, 2]);
    }

    #[test]
    fn rfc_reply_uses_server_command_offset() {
        assert_eq!(
            rfc_reply(SET_DATASIZE, &[8]),
            vec![IAC, SB, COM_PORT_OPTION, 102, 8, IAC, SE]
        );
    }

    #[test]
    fn telnet_decoder_handles_negotiation_and_subnegotiation_split_at_every_boundary() {
        let input = [
            IAC,
            WILL,
            COM_PORT_OPTION,
            IAC,
            SB,
            COM_PORT_OPTION,
            SIGNATURE,
            IAC,
            SE,
        ];
        for split in 1..input.len() {
            let mut decoder = TelnetDecoder::default();
            let mut payload = Vec::new();
            let mut replies = Vec::new();
            let mut subnegotiations = Vec::new();
            decoder.feed(
                &input[..split.min(input.len())],
                &mut payload,
                &mut replies,
                &mut subnegotiations,
            );
            decoder.feed(
                &input[split.min(input.len())..],
                &mut payload,
                &mut replies,
                &mut subnegotiations,
            );
            assert!(payload.is_empty(), "split={split} payload={payload:?}");
            assert_eq!(replies, vec![IAC, DO, COM_PORT_OPTION], "split={split}");
            assert_eq!(
                subnegotiations,
                vec![vec![COM_PORT_OPTION, SIGNATURE]],
                "split={split}"
            );
        }
    }

    #[test]
    fn unsupported_telnet_options_are_refused_and_unknown_commands_do_not_leak_payload() {
        assert_eq!(negotiation_reply(WILL, 99), vec![IAC, DONT, 99]);
        assert_eq!(negotiation_reply(DO, 99), vec![IAC, WONT, 99]);

        let mut decoder = TelnetDecoder::default();
        let mut payload = Vec::new();
        let mut replies = Vec::new();
        let mut subnegotiations = Vec::new();
        decoder.feed(
            &[IAC, 123, b'x', b'y', IAC, IAC, b'z'],
            &mut payload,
            &mut replies,
            &mut subnegotiations,
        );
        assert_eq!(payload, b"xy\xffz");
        assert!(replies.is_empty());
        assert!(subnegotiations.is_empty());
    }
}
