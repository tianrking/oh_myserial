//! User-mode bridges for legacy host applications.
//!
//! A Windows COM application can only open a COM device exposed by the OS.
//! `bridge-com` therefore connects an already-created COM endpoint (for
//! example one side of a com0com pair) to an ohmyserial raw TCP endpoint. It
//! does not install or pretend to be a kernel virtual-COM driver.

use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::config::SerialSettings;

const IO_TIMEOUT: Duration = Duration::from_millis(100);
const BUFFER_SIZE: usize = 16 * 1024;

/// Options for one raw COM-to-TCP bridge.
#[derive(Debug, Clone)]
pub struct ComBridgeOptions {
    pub device: String,
    pub tcp: String,
    pub settings: SerialSettings,
}

/// Run a blocking, bidirectional bridge until either side closes or fails.
pub fn run_com_bridge(options: ComBridgeOptions) -> anyhow::Result<()> {
    options
        .settings
        .validate()
        .map_err(|error| anyhow::anyhow!("invalid bridge serial settings: {error}"))?;

    let port = open_port(&options.device, &options.settings)?;
    let tcp = TcpStream::connect(&options.tcp)
        .map_err(|error| anyhow::anyhow!("connect TCP {} failed: {error}", options.tcp))?;
    tcp.set_nodelay(true)?;
    tcp.set_read_timeout(Some(IO_TIMEOUT))?;
    tcp.set_write_timeout(Some(IO_TIMEOUT))?;
    let tcp_to_serial = tcp.try_clone()?;
    let serial = Arc::new(parking_lot::Mutex::new(port));
    let stopping = Arc::new(AtomicBool::new(false));

    tracing::info!(device = %options.device, tcp = %options.tcp, "COM bridge connected");

    let serial_reader = serial.clone();
    let serial_stop = stopping.clone();
    let mut tcp_writer = tcp.try_clone()?;
    let serial_thread = thread::Builder::new()
        .name("ohmyserial-com-to-tcp".into())
        .spawn(move || {
            let mut buffer = [0u8; BUFFER_SIZE];
            loop {
                if serial_stop.load(Ordering::Acquire) {
                    break;
                }
                let read = {
                    let mut port = serial_reader.lock();
                    port.read(&mut buffer)
                };
                match read {
                    Ok(0) => {
                        tracing::info!("COM endpoint closed");
                        break;
                    }
                    Ok(count) => {
                        if let Err(error) = write_all_retry(&mut tcp_writer, &buffer[..count]) {
                            if !is_timeout(&error) {
                                tracing::warn!("COM-to-TCP write failed: {error}");
                                break;
                            }
                        }
                    }
                    Err(error) if is_timeout(&error) => {}
                    Err(error) => {
                        tracing::warn!("COM read failed: {error}");
                        break;
                    }
                }
            }
            serial_stop.store(true, Ordering::Release);
            let _ = tcp_writer.shutdown(Shutdown::Both);
        })?;

    let serial_writer = serial;
    let tcp_stop = stopping.clone();
    let mut tcp_reader = tcp_to_serial;
    let tcp_thread = thread::Builder::new()
        .name("ohmyserial-tcp-to-com".into())
        .spawn(move || {
            let mut buffer = [0u8; BUFFER_SIZE];
            loop {
                if tcp_stop.load(Ordering::Acquire) {
                    break;
                }
                match tcp_reader.read(&mut buffer) {
                    Ok(0) => {
                        tracing::info!("TCP peer closed");
                        break;
                    }
                    Ok(count) => {
                        let result = {
                            let mut port = serial_writer.lock();
                            port.write_all(&buffer[..count]).and_then(|_| port.flush())
                        };
                        if let Err(error) = result {
                            if !is_timeout(&error) {
                                tracing::warn!("TCP-to-COM write failed: {error}");
                                break;
                            }
                        }
                    }
                    Err(error) if is_timeout(&error) => {}
                    Err(error) => {
                        tracing::warn!("TCP read failed: {error}");
                        break;
                    }
                }
            }
            tcp_stop.store(true, Ordering::Release);
            let _ = tcp_reader.shutdown(Shutdown::Both);
        })?;

    let _ = serial_thread.join();
    stopping.store(true, Ordering::Release);
    let _ = tcp_thread.join();
    let _ = tcp.shutdown(Shutdown::Both);
    tracing::info!("COM bridge stopped");
    Ok(())
}

fn open_port(
    device: &str,
    settings: &SerialSettings,
) -> anyhow::Result<Box<dyn serialport::SerialPort>> {
    let data_bits = match settings.databits {
        5 => serialport::DataBits::Five,
        6 => serialport::DataBits::Six,
        7 => serialport::DataBits::Seven,
        8 => serialport::DataBits::Eight,
        _ => unreachable!("validated data bits"),
    };
    let parity = match settings.parity.to_ascii_lowercase().as_str() {
        "none" => serialport::Parity::None,
        "odd" => serialport::Parity::Odd,
        "even" => serialport::Parity::Even,
        _ => unreachable!("validated parity"),
    };
    let stop_bits = match settings.stopbits {
        1 => serialport::StopBits::One,
        2 => serialport::StopBits::Two,
        _ => unreachable!("validated stop bits"),
    };
    let flow = match settings.flow.to_ascii_lowercase().as_str() {
        "none" => serialport::FlowControl::None,
        "software" => serialport::FlowControl::Software,
        "hardware" => serialport::FlowControl::Hardware,
        _ => unreachable!("validated flow control"),
    };
    serialport::new(device, settings.baud)
        .data_bits(data_bits)
        .parity(parity)
        .stop_bits(stop_bits)
        .flow_control(flow)
        .timeout(IO_TIMEOUT)
        .open()
        .map_err(|error| anyhow::anyhow!("open serial device {device} failed: {error}"))
}

fn is_timeout(error: &std::io::Error) -> bool {
    matches!(error.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock)
}

fn write_all_retry(stream: &mut TcpStream, bytes: &[u8]) -> std::io::Result<()> {
    let mut written = 0;
    while written < bytes.len() {
        match stream.write(&bytes[written..]) {
            Ok(0) => return Err(std::io::Error::new(ErrorKind::WriteZero, "TCP peer closed")),
            Ok(count) => written += count,
            Err(error) if is_timeout(&error) => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_detection_is_narrow() {
        assert!(is_timeout(&std::io::Error::from(ErrorKind::TimedOut)));
        assert!(is_timeout(&std::io::Error::from(ErrorKind::WouldBlock)));
        assert!(!is_timeout(&std::io::Error::from(ErrorKind::BrokenPipe)));
    }
}
