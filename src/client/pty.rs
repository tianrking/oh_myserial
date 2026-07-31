//! Unix PTY adapter for traditional serial host tools.

#![cfg(unix)]

use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use bytes::Bytes;
use nix::fcntl::{fcntl, FcntlArg, OFlag};
use nix::pty::{openpty, OpenptyResult};
use nix::sys::termios::{cfmakeraw, tcgetattr, tcsetattr, SetArg};
use nix::unistd::{read, write};

use crate::broker::Broker;

struct PtyLinkGuard {
    link: PathBuf,
}

impl Drop for PtyLinkGuard {
    fn drop(&mut self) {
        if let Err(error) = remove_link_if_symlink(&self.link) {
            tracing::warn!(
                "could not clean up PTY link {}: {error:#}",
                self.link.display()
            );
        }
    }
}

struct PtyTaskCleanup {
    stopping: Arc<AtomicBool>,
    _link: PtyLinkGuard,
}

impl Drop for PtyTaskCleanup {
    fn drop(&mut self) {
        // The blocking tasks cannot be forcibly aborted once running. Signal
        // them before the link guard is dropped so their retry loops and any
        // in-flight broker TX stop promptly when the supervisor is cancelled.
        self.stopping.store(true, Ordering::Release);
    }
}

/// A PTY endpoint whose fallible operating-system setup is already complete.
///
/// Preparing and starting are deliberately separate: hub startup can prepare
/// every PTY first and return an `Err` for `openpty`, raw-mode, nonblocking, or
/// symlink failures before it launches any long-lived async task. Dropping a
/// prepared endpoint without starting it removes the symlink.
pub struct PreparedPtyClient {
    broker: Broker,
    name: String,
    link: PtyLinkGuard,
    slave_path: String,
    master: Arc<OwnedFd>,
    can_read: bool,
    can_write: bool,
}

impl PreparedPtyClient {
    pub fn start(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            if let Err(error) = run_prepared_pty(self).await {
                tracing::error!("pty client error: {error:#}");
            }
        })
    }
}

/// Perform every fallible PTY setup operation synchronously.
pub fn prepare_pty_client(
    broker: Broker,
    name: String,
    link: PathBuf,
    can_read: bool,
    can_write: bool,
) -> anyhow::Result<PreparedPtyClient> {
    let OpenptyResult { master, slave } = openpty(None, None)?;
    // A virtual serial endpoint must be a byte stream. The default PTY line
    // discipline can echo device RX back as fake client TX and translate CR/LF.
    let mut termios = tcgetattr(slave.as_fd())?;
    cfmakeraw(&mut termios);
    tcsetattr(slave.as_fd(), SetArg::TCSANOW, &termios)?;
    let slave_path = nix::unistd::ttyname(slave.as_fd())?
        .to_string_lossy()
        .into_owned();

    set_nonblocking(master.as_raw_fd())?;
    setup_link(&link, &slave_path)?;
    drop(slave); // host tools open the slave via symlink

    Ok(PreparedPtyClient {
        broker,
        name,
        link: PtyLinkGuard { link },
        slave_path,
        master: Arc::new(master),
        can_read,
        can_write,
    })
}

pub fn spawn_pty_client(
    broker: Broker,
    name: String,
    link: PathBuf,
    can_read: bool,
    can_write: bool,
) -> tokio::task::JoinHandle<()> {
    // Compatibility path for existing embedders. New hub startup code should
    // call `prepare_pty_client(...)?` and only call `.start()` after every
    // endpoint has prepared successfully, so setup errors reach the caller.
    match prepare_pty_client(broker, name, link, can_read, can_write) {
        Ok(prepared) => prepared.start(),
        Err(error) => tokio::spawn(async move {
            tracing::error!("pty client setup error: {error:#}");
        }),
    }
}

async fn run_prepared_pty(prepared: PreparedPtyClient) -> anyhow::Result<()> {
    let PreparedPtyClient {
        broker,
        name,
        link,
        slave_path,
        master,
        can_read,
        can_write,
    } = prepared;
    tracing::info!(
        "pty client '{name}' slave={slave_path} link={}",
        link.link.display()
    );
    broker.log().event(&format!(
        "pty_ready name={name} link={} slave={slave_path}",
        link.link.display()
    ));

    let (id, mut from_broker) =
        broker.register_client(name.clone(), "pty", can_read, can_write, None);
    let registration = broker.client_registration(id);
    let stopping = Arc::new(AtomicBool::new(false));
    let _cleanup = PtyTaskCleanup {
        stopping: stopping.clone(),
        _link: link,
    };

    // The blocking writer consumes the Broker's bounded Tokio receiver
    // directly. This avoids introducing an unbounded async-to-std bridge.
    let master_w = master.clone();
    let stop_w = stopping.clone();
    let mut writer = tokio::task::spawn_blocking(move || {
        while !stop_w.load(Ordering::Acquire) {
            match from_broker.blocking_recv() {
                Some(data) => {
                    let mut off = 0;
                    while off < data.len() {
                        // In particular, do not remain forever in the EIO
                        // retry loop when no slave is open and the supervisor
                        // future has been aborted.
                        if stop_w.load(Ordering::Acquire) {
                            return;
                        }
                        match write(master_w.as_fd(), &data[off..]) {
                            Ok(0) => {
                                std::thread::sleep(std::time::Duration::from_millis(2));
                            }
                            Ok(n) => off += n,
                            // Linux PTY masters report EIO while no process has
                            // the slave open yet; keep the advertised endpoint
                            // alive until a host tool connects.
                            Err(nix::errno::Errno::EAGAIN | nix::errno::Errno::EIO) => {
                                std::thread::sleep(std::time::Duration::from_millis(2));
                            }
                            Err(e) => {
                                tracing::warn!("pty write: {e}");
                                return;
                            }
                        }
                    }
                }
                None => break,
            }
        }
    });

    // PTY master read -> client TX toward device.
    let broker_r = broker.clone();
    let id_r = id;
    let master_r = master.clone();
    let name_c = name.clone();
    let handle = tokio::runtime::Handle::current();
    let stop_r = stopping.clone();
    let mut reader = tokio::task::spawn_blocking(move || {
        let mut buf = [0u8; 4096];
        while !stop_r.load(Ordering::Acquire) {
            match read(master_r.as_raw_fd(), &mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if can_write {
                        let data = Bytes::copy_from_slice(&buf[..n]);
                        let broker = broker_r.clone();
                        let id = id_r;
                        let stop_tx = stop_r.clone();
                        let result = handle.block_on(async move {
                            tokio::select! {
                                biased;
                                _ = wait_until_stopping(stop_tx) => None,
                                result = broker.client_tx(id, data) => Some(result),
                            }
                        });
                        match result {
                            Some(Ok(())) => {}
                            Some(Err(error)) => {
                                tracing::warn!("pty '{name_c}' tx denied: {error}");
                            }
                            None => break,
                        }
                    }
                }
                Err(nix::errno::Errno::EAGAIN)
                | Err(nix::errno::Errno::EINTR)
                | Err(nix::errno::Errno::EIO) => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(e) => {
                    tracing::warn!("pty read: {e}");
                    break;
                }
            }
        }
    });

    tokio::select! {
        _ = &mut reader => {},
        _ = &mut writer => {},
    }

    // Unregister closes the Broker fanout and wakes writer.blocking_recv(). The
    // reader owns a nonblocking descriptor and observes `stopping` promptly.
    stopping.store(true, Ordering::Release);
    drop(registration);
    let _ = reader.await;
    let _ = writer.await;
    Ok(())
}

async fn wait_until_stopping(stopping: Arc<AtomicBool>) {
    while !stopping.load(Ordering::Acquire) {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

fn set_nonblocking(fd: i32) -> anyhow::Result<()> {
    let flags = fcntl(fd, FcntlArg::F_GETFL)?;
    let flags = OFlag::from_bits_truncate(flags) | OFlag::O_NONBLOCK;
    fcntl(fd, FcntlArg::F_SETFL(flags))?;
    Ok(())
}

fn setup_link(link: &Path, slave_path: &str) -> anyhow::Result<()> {
    if let Some(parent) = link.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    remove_link_if_symlink(link)?;
    symlink(slave_path, link)?;
    Ok(())
}

fn remove_link_if_symlink(link: &Path) -> anyhow::Result<()> {
    match link.symlink_metadata() {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            std::fs::remove_file(link)?;
        }
        Ok(_) => anyhow::bail!(
            "refusing to replace non-symlink PTY link path {}",
            link.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::time::Duration;

    use super::*;
    use crate::broker::{BrokerSplit, PortStatus};
    use crate::observe::SessionLog;
    use crate::policy::{Policy, SlowClientPolicy, TxMode};

    fn test_broker() -> BrokerSplit {
        Broker::new(
            Policy {
                mode: TxMode::QueueByLine,
                primary: None,
                write_lock_ms: 1000,
                write_timeout_ms: 1000,
                max_frame_bytes: 1024,
                max_write_bytes: 1024,
                frame_delim: b'\n',
                slow_client: SlowClientPolicy::DropOldest,
                client_queue: 16,
                slow_block_ms: 100,
            },
            PortStatus {
                path: "mock:pty-test".into(),
                baud: 115_200,
                connected: true,
                detail: "test".into(),
            },
            SessionLog::disabled(),
            1024,
            16,
        )
    }

    #[test]
    fn setup_link_refuses_to_delete_a_regular_file() {
        let temp = tempfile::tempdir().unwrap();
        let link = temp.path().join("serial");
        std::fs::write(&link, b"keep-me").unwrap();
        let error = setup_link(&link, "/dev/pts/999").unwrap_err().to_string();
        assert!(error.contains("non-symlink"), "error={error}");
        assert_eq!(std::fs::read(&link).unwrap(), b"keep-me");
    }

    #[test]
    fn prepare_reports_setup_failure_without_registering_or_deleting_file() {
        let split = test_broker();
        let temp = tempfile::tempdir().unwrap();
        let link = temp.path().join("serial");
        std::fs::write(&link, b"keep-me").unwrap();

        let error = prepare_pty_client(
            split.broker.clone(),
            "pty-test".into(),
            link.clone(),
            true,
            true,
        )
        .err()
        .expect("a regular-file link target must fail preparation")
        .to_string();

        assert!(error.contains("non-symlink"), "error={error}");
        assert_eq!(std::fs::read(&link).unwrap(), b"keep-me");
        assert!(split.broker.snapshot().clients.is_empty());
    }

    #[test]
    fn dropping_prepared_endpoint_removes_its_link() {
        let split = test_broker();
        let temp = tempfile::tempdir().unwrap();
        let link = temp.path().join("serial");
        let prepared =
            prepare_pty_client(split.broker, "pty-test".into(), link.clone(), true, true).unwrap();

        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        drop(prepared);
        assert_eq!(
            link.symlink_metadata().unwrap_err().kind(),
            std::io::ErrorKind::NotFound
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn abort_during_no_slave_write_releases_registration_link_and_fds() {
        let split = test_broker();
        let broker = split.broker;
        let temp = tempfile::tempdir().unwrap();
        let link = temp.path().join("serial");
        let prepared = prepare_pty_client(
            broker.clone(),
            "pty-cancel".into(),
            link.clone(),
            true,
            true,
        )
        .unwrap();
        let master = Arc::downgrade(&prepared.master);
        let task = prepared.start();

        tokio::time::timeout(Duration::from_secs(1), async {
            while broker.snapshot().clients.is_empty() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("PTY client was not registered");

        // No process ever opens the slave, so Linux keeps the blocking writer
        // in its EIO retry path. Cancellation must still terminate that loop.
        broker.on_device_rx(Bytes::from_static(b"pending-device-output"));
        tokio::time::sleep(Duration::from_millis(25)).await;
        task.abort();
        let _ = task.await;

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let link_removed = matches!(
                    link.symlink_metadata(),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound
                );
                if broker.snapshot().clients.is_empty()
                    && link_removed
                    && master.upgrade().is_none()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("cancelled PTY workers did not release their resources");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pty_survives_without_an_open_slave_and_exchanges_bytes() {
        let split = test_broker();
        let broker = split.broker;
        let mut serial_rx = split.serial_tx_rx;
        let temp = tempfile::tempdir().unwrap();
        let link = temp.path().join("serial");
        let task = spawn_pty_client(broker.clone(), "pty-test".into(), link.clone(), true, true);

        tokio::time::timeout(Duration::from_secs(1), async {
            while !link.exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("PTY link was not created");
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!task.is_finished(), "PTY died before a slave was opened");

        let mut slave = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&link)
            .unwrap();
        set_nonblocking(slave.as_raw_fd()).unwrap();

        broker.on_device_rx(Bytes::from_static(b"device\n"));
        let mut read_slave = slave.try_clone().unwrap();
        let received = tokio::task::spawn_blocking(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(1);
            let mut data = Vec::new();
            let mut buf = [0u8; 64];
            while std::time::Instant::now() < deadline {
                match read_slave.read(&mut buf) {
                    Ok(n) if n > 0 => {
                        data.extend_from_slice(&buf[..n]);
                        if data.ends_with(b"device\n") {
                            return data;
                        }
                    }
                    Ok(_) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) => panic!("read PTY slave: {e}"),
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            panic!("timed out reading PTY slave")
        })
        .await
        .unwrap();
        assert!(received.ends_with(b"device\n"));

        slave.write_all(b"host\n").unwrap();
        let write = tokio::time::timeout(Duration::from_secs(1), serial_rx.recv())
            .await
            .expect("PTY host write timeout")
            .expect("serial write queue closed");
        assert_eq!(write.bytes(), &Bytes::from_static(b"host\n"));

        task.abort();
        let _ = task.await;
        tokio::time::timeout(Duration::from_secs(1), async {
            while link.exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("PTY link was not cleaned up");
    }
}
