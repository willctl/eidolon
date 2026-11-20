//! The window's rules, against folder cells whose streams reach a tiny fake
//! VNC server.
#![expect(clippy::unwrap_used, reason = "a failing test should say where; helpers here are not #[test] fns")]

use adytum::testing::{duplex, ok, DriverCall, FakeDriver};
use eidolon::testing::running;
use eidolon::view::{View, ViewGuard};
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Sends the RFB banner, then echoes what it is sent.
async fn fake_vnc(mut end: tokio::io::DuplexStream) {
    if end.write_all(b"RFB 003.008\n").await.is_err() {
        return;
    }
    let mut buf = [0u8; 256];
    while let Ok(n) = end.read(&mut buf).await {
        if n == 0 || end.write_all(&buf[..n]).await.is_err() {
            return;
        }
    }
}

struct Exec {
    argv: Vec<String>,
    stdin: String,
}

struct Fake(Arc<FakeDriver>);

impl Fake {
    fn new() -> Fake {
        let driver = FakeDriver::new();
        driver.on_exec(|_| ok(""));
        driver.on_stream(|_| {
            let (ours, theirs) = duplex();
            tokio::spawn(fake_vnc(theirs));
            Ok(ours)
        });
        Fake(Arc::new(driver))
    }

    fn execs(&self) -> Vec<Exec> {
        self.0
            .calls()
            .into_iter()
            .filter_map(|c| match c {
                DriverCall::Exec(e) => Some(Exec { argv: e.argv, stdin: String::from_utf8_lossy(&e.stdin).into_owned() }),
                _ => None,
            })
            .collect()
    }

    fn stream_argvs(&self) -> Vec<Vec<String>> {
        self.0.calls().into_iter().filter_map(|c| if let DriverCall::Stream { argv, .. } = c { Some(argv) } else { None }).collect()
    }
}

/// A scratch folder that removes itself.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Scratch {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!("eidolon-view-test-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn open(driver: &Fake, scratch: &Scratch) -> View {
    View::open(running(&driver.0, &"adytum-test-view".parse().unwrap()), ViewGuard::new(&scratch.0)).await.unwrap()
}

async fn banner(addr: std::net::SocketAddr) -> (TcpStream, String) {
    let mut sock = TcpStream::connect(addr).await.unwrap();
    let mut got = [0u8; 12];
    sock.read_exact(&mut got).await.unwrap();
    (sock, String::from_utf8(got.to_vec()).unwrap())
}

#[tokio::test]
async fn the_listener_is_on_127_0_0_1_only() {
    let driver = Fake::new();
    let scratch = Scratch::new();
    let view = open(&driver, &scratch).await;
    assert_eq!(view.address().ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));
    assert_ne!(view.address().port(), 0);
    assert_eq!(view.url(), format!("vnc://127.0.0.1:{}", view.address().port()));
    view.close().await.unwrap();
}

#[tokio::test]
async fn a_client_gets_the_rfb_banner_through_the_stream_and_the_port_is_not_forwarded() {
    let driver = Fake::new();
    let scratch = Scratch::new();
    let view = open(&driver, &scratch).await;
    let (mut sock, got) = banner(view.address()).await;
    assert_eq!(got, "RFB 003.008\n");
    sock.write_all(b"hello").await.unwrap();
    let mut echo = [0u8; 5];
    sock.read_exact(&mut echo).await.unwrap();
    assert_eq!(&echo, b"hello");
    // The only way to the guest's VNC port is socat over the stream.
    assert_eq!(driver.stream_argvs(), vec![vec!["socat".to_owned(), "-".to_owned(), "TCP:127.0.0.1:5900".to_owned()]]);
    view.close().await.unwrap();
}

#[tokio::test]
async fn two_clients_each_get_their_own_stream() {
    let driver = Fake::new();
    let scratch = Scratch::new();
    let view = open(&driver, &scratch).await;
    let (mut a, _) = banner(view.address()).await;
    let (mut b, _) = banner(view.address()).await;
    a.write_all(b"aaa").await.unwrap();
    b.write_all(b"bbb").await.unwrap();
    let (mut ea, mut eb) = ([0u8; 3], [0u8; 3]);
    a.read_exact(&mut ea).await.unwrap();
    b.read_exact(&mut eb).await.unwrap();
    assert_eq!((&ea, &eb), (b"aaa", b"bbb"));
    assert_eq!(driver.stream_argvs().len(), 2);
    view.close().await.unwrap();
}

#[tokio::test]
async fn the_plans_run_in_order_and_the_secret_is_only_ever_stdin() {
    let driver = Fake::new();
    let scratch = Scratch::new();
    let mut view = open(&driver, &scratch).await;
    let secret = view.secret().unwrap();
    assert_eq!(secret.len(), 8);
    assert!(secret.chars().all(|c| c.is_ascii_alphanumeric()));
    assert!(view.secret().is_none(), "the secret is given out once");

    let opened = driver.execs();
    let last = opened.last().unwrap();
    assert!(last.argv.join(" ").contains("systemctl start eidolon-view"), "{:?}", last.argv);
    let carriers: Vec<&Exec> = opened.iter().filter(|e| e.stdin == secret).collect();
    assert_eq!(carriers.len(), 1, "exactly one command gets the secret, as stdin");
    assert!(carriers[0].argv.iter().any(|a| a.starts_with("of=")));

    let (mut sock, _) = banner(view.address()).await;
    sock.write_all(b"x").await.unwrap();

    let url = view.url();
    let shown = format!("{view:?}");
    view.close().await.unwrap();

    let all = driver.execs();
    assert!(all.len() > opened.len());
    assert!(all.last().unwrap().argv.join(" ").contains("systemctl start eidolon-browser"));
    for argv in all.iter().map(|e| &e.argv).chain(driver.stream_argvs().iter()) {
        assert!(argv.iter().all(|a| !a.contains(&secret)), "secret in argv {argv:?}");
    }
    assert!(!shown.contains(&secret) && !url.contains(&secret));
    assert!(!format!("{:?}", all.first().map(|e| e.argv.clone())).contains(&secret));
}

#[tokio::test]
async fn closing_stops_the_listener_and_drops_connections() {
    let driver = Fake::new();
    let scratch = Scratch::new();
    let view = open(&driver, &scratch).await;
    let addr = view.address();
    let (mut sock, _) = banner(addr).await;
    view.close().await.unwrap();
    let mut rest = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(5), sock.read_to_end(&mut rest)).await.unwrap();
    assert!(read.is_err() || rest.is_empty(), "the connection should be closed");
    assert!(TcpStream::connect(addr).await.is_err(), "nothing listens after close");
}

#[tokio::test]
async fn dropping_a_view_starts_the_browser_again() {
    let driver = Fake::new();
    let scratch = Scratch::new();
    let view = open(&driver, &scratch).await;
    drop(view);
    for _ in 0..100 {
        if driver.execs().last().is_some_and(|e| e.argv.join(" ").contains("systemctl start eidolon-browser")) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the browser plan did not run after the drop");
}

#[tokio::test]
async fn the_marker_appears_while_the_window_is_open_and_goes_when_it_closes() {
    let scratch = Scratch::new();
    let guard = ViewGuard::new(&scratch.0);
    assert!(!guard.is_open());
    assert!(guard.refuse_if_open().is_ok());

    let driver = Fake::new();
    let view = open(&driver, &scratch).await;
    assert!(guard.is_open());
    assert!(scratch.0.join("view.open").is_file());
    let err = guard.refuse_if_open().unwrap_err();
    assert_eq!(err.code(), "view_open");

    view.close().await.unwrap();
    assert!(!guard.is_open());
    assert!(!scratch.0.join("view.open").exists());
}

#[tokio::test]
async fn a_marker_whose_process_is_gone_counts_as_closed() {
    let scratch = Scratch::new();
    let guard = ViewGuard::new(&scratch.0);
    for pid in ["0", "4294967295", "not a pid", ""] {
        std::fs::write(scratch.0.join("view.open"), format!("{pid}\n")).unwrap();
        assert!(!guard.is_open(), "pid {pid:?}");
        assert!(guard.refuse_if_open().is_ok());
    }
}
