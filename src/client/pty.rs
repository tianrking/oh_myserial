//! Unix PTY adapter for traditional serial host tools.

#![cfg(unix)]

use std::os::fd::{AsFd, BorrowedFd, FromRawFd, IntoRawFd, OwnedFd};
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use bytes::Bytes;
use nix::fcntl::{fcntl, FcntlArg, OFlag};
use nix::pty::{openpty, OpenptyResult};
use nix::unistd::{read, write};

use crate::broker::Broker;

pub fn spawn_pty_client(
    broker: Broker,
    name: String,
    link: PathBuf,
    can_read: bool,
    can_write: bool,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if let Err(e) = run_pty(broker, name, link, can_read, can_write).await {
            tracing::error!("pty client error: {e:#}");
        }
    })
}

async fn run_pty(
    broker: Broker,
    name: String,
    link: PathBuf,
    can_read: bool,
    can_write: bool,
) -> anyhow::Result<()> {
    let OpenptyResult { master, slave } = openpty(None, None)?;
    let slave_path = nix::unistd::ttyname(slave.as_fd())?
        .to_string_lossy()
        .into_owned();
    drop(slave); // host tools open the slave via symlink

    setup_link(&link, &slave_path)?;
    tracing::info!(
        "pty client '{name}' slave={slave_path} link={}",
        link.display()
    );
    broker.log().event(&format!(
        "pty_ready name={name} link={} slave={slave_path}",
        link.display()
    ));

    let master_fd = master.into_raw_fd();
    set_nonblocking(master_fd)?;

    let (id, mut from_broker) =
        broker.register_client(name.clone(), "pty", can_read, can_write, None);

    // Bridge: async mpsc -> std thread writing to master.
    let (std_tx, std_rx) = std::sync::mpsc::channel::<Bytes>();
    let master_w = master_fd;
    let writer = std::thread::Builder::new()
        .name(format!("pty-write-{name}"))
        .spawn(move || {
            while let Ok(data) = std_rx.recv() {
                let mut off = 0;
                while off < data.len() {
                    // SAFETY: master_fd is open for the lifetime of this thread.
                    let fd = unsafe { BorrowedFd::borrow_raw(master_w) };
                    match write(fd, &data[off..]) {
                        Ok(n) => off += n,
                        Err(nix::errno::Errno::EAGAIN) => {
                            std::thread::sleep(std::time::Duration::from_millis(2));
                        }
                        Err(e) => {
                            tracing::warn!("pty write: {e}");
                            return;
                        }
                    }
                }
            }
        })?;

    let pump = tokio::spawn(async move {
        while let Some(data) = from_broker.recv().await {
            if std_tx.send(data).is_err() {
                break;
            }
        }
    });

    // PTY master read -> client TX toward device.
    let broker_r = broker.clone();
    let id_r = id;
    let master_r = master_fd;
    let name_c = name.clone();
    let handle = tokio::runtime::Handle::current();
    let reader = tokio::task::spawn_blocking(move || {
        let mut buf = [0u8; 4096];
        loop {
            match read(master_r, &mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if can_write {
                        let data = Bytes::copy_from_slice(&buf[..n]);
                        let broker = broker_r.clone();
                        let id = id_r;
                        let res = handle.block_on(broker.client_tx(id, data));
                        if let Err(e) = res {
                            tracing::warn!("pty '{name_c}' tx denied: {e}");
                        }
                    }
                }
                Err(nix::errno::Errno::EAGAIN) | Err(nix::errno::Errno::EINTR) => {
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
        _ = reader => {},
        _ = pump => {},
    }

    let _ = writer.join();
    // Close master
    unsafe {
        let _ = OwnedFd::from_raw_fd(master_fd);
    }
    broker.unregister_client(id);
    let _ = std::fs::remove_file(&link);
    Ok(())
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
    if link.exists() || link.symlink_metadata().is_ok() {
        let _ = std::fs::remove_file(link);
    }
    symlink(slave_path, link)?;
    Ok(())
}
