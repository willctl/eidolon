//! The person's window for signing in to a browser facet. While it is open
//! the browser runs as the view unit, with no automation socket, which is
//! what a site sees. VNC's own authentication is weak, so both ends are
//! loopback only and the secret lives for one session.

use crate::cell::Cell;
use crate::error::{Error, Result};
use crate::facet::firefox::{stream_argv, VNC_PORT};
use crate::facet::plan;
use crate::model::RefusalCode;
use std::fmt;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use tokio::io::copy_bidirectional;
use tokio::net::{TcpListener, TcpStream};
use tokio::runtime::Handle;
use tokio::task::{JoinHandle, JoinSet};

pub const SECRET_LEN: usize = 8;

/// A marker, in the eidolon's holding, that a sign-in window is open. It
/// holds the pid of the process that opened the window, so a marker left by
/// a process that is gone counts as closed.
#[derive(Debug, Clone)]
pub struct ViewGuard {
    path: PathBuf,
}

impl ViewGuard {
    pub fn new(holding: impl AsRef<Path>) -> ViewGuard {
        ViewGuard { path: holding.as_ref().join("view.open") }
    }

    fn mark_open(&self) -> Result<()> {
        std::fs::write(&self.path, format!("{}\n", std::process::id())).map_err(Error::io("write the sign-in window's marker"))
    }

    fn clear(&self) -> Result<()> {
        match std::fs::remove_file(&self.path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(Error::Io { what: "remove the sign-in window's marker", source: e }),
            _ => Ok(()),
        }
    }

    pub fn is_open(&self) -> bool {
        let Ok(text) = std::fs::read_to_string(&self.path) else { return false };
        let alive = text.trim().parse::<u32>().is_ok_and(|pid| xenonet_os::process::running(pid, true));
        if !alive {
            let _ = std::fs::remove_file(&self.path);
        }
        alive
    }

    pub fn refuse_if_open(&self) -> Result<()> {
        if self.is_open() {
            return Err(Error::refused(
                RefusalCode::ViewOpen,
                "The sign-in window is open, so the browser is not driven now.",
                "Press Enter in the terminal running `eidolon view`, or stop that command, then try again.",
            ));
        }
        Ok(())
    }
}

pub struct View {
    addr: SocketAddr,
    secret: Option<String>,
    task: Option<JoinHandle<()>>,
    restore: Option<(Cell, ViewGuard)>,
}

impl fmt::Debug for View {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("View").field("address", &self.addr).finish_non_exhaustive()
    }
}

impl View {
    pub async fn open(cell: Cell, guard: ViewGuard) -> Result<View> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.map_err(Error::io("listen for the sign-in window"))?;
        let addr = listener.local_addr().map_err(Error::io("read the window's address"))?;
        let secret = make_secret()?;
        guard.mark_open()?;
        if let Err(e) = plan::start_view(&secret).run(&cell).await {
            let _ = plan::start_browser().run(&cell).await;
            let _ = guard.clear();
            return Err(e);
        }
        let task = tokio::spawn(accept_loop(listener, cell.clone()));
        Ok(View { addr, secret: Some(secret), task: Some(task), restore: Some((cell, guard)) })
    }

    pub fn address(&self) -> SocketAddr {
        self.addr
    }

    pub fn url(&self) -> String {
        format!("vnc://{}", self.addr)
    }

    pub fn secret(&mut self) -> Option<String> {
        self.secret.take()
    }

    pub async fn close(mut self) -> Result<()> {
        if let Some(task) = self.task.take() {
            task.abort();
            let _ = task.await;
        }
        let Some((cell, guard)) = self.restore.take() else { return Ok(()) };
        guard.clear()?;
        plan::start_browser().run(&cell).await
    }
}

impl Drop for View {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
        let Some((cell, guard)) = self.restore.take() else { return };
        if let Err(e) = guard.clear() {
            eprintln!("eidolon: {e}");
        }
        match Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    if let Err(e) = plan::start_browser().run(&cell).await {
                        eprintln!("eidolon: could not start the browser again: {e}");
                    }
                });
            }
            Err(_) => eprintln!("eidolon: the window was closed outside a runtime, so the browser was not started again; run `systemctl start eidolon-browser` in the cell."),
        }
    }
}

/// Eight random letters and digits. Bytes of 248 and over are thrown away so
/// every character is equally likely.
fn make_secret() -> Result<String> {
    const ALPHABET: &[u8; 62] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut out = String::with_capacity(SECRET_LEN);
    while out.len() < SECRET_LEN {
        let bytes = xenonet_id::codec::random::<16>().map_err(|_| Error::random())?;
        for b in bytes.into_iter().filter(|b| *b < 248) {
            if out.len() < SECRET_LEN {
                out.push(char::from(ALPHABET[usize::from(b) % ALPHABET.len()]));
            }
        }
    }
    Ok(out)
}

async fn accept_loop(listener: TcpListener, cell: Cell) {
    // Dropping the set, when this task is aborted, aborts every connection.
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((socket, _)) => {
                    connections.spawn(serve(socket, cell.clone()));
                }
                Err(e) => {
                    eprintln!("eidolon: could not accept a connection to the sign-in window: {e}");
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            },
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
}

async fn serve(mut socket: TcpStream, cell: Cell) {
    match cell.stream(&stream_argv(VNC_PORT)).await {
        Ok(mut stream) => {
            let _ = copy_bidirectional(&mut socket, &mut stream).await;
        }
        Err(e) => eprintln!("eidolon: could not reach the sign-in window in the cell: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_secret_is_eight_letters_and_digits_and_differs_each_time() {
        let (a, b) = (make_secret().unwrap(), make_secret().unwrap());
        assert_eq!(a.len(), SECRET_LEN);
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric()));
        assert_ne!(a, b);
    }
}
