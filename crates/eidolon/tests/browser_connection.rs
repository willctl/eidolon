//! Reaching the browser as a stream, and reconnecting when it restarts.
#![expect(clippy::unwrap_used, reason = "a failing test should say where; helpers here are not #[test] fns")]

use adytum::testing::{duplex, DriverCall, FakeDriver};
use adytum::Error as AdytumError;
use eidolon::browser::{Browser, ConnectOptions};
use eidolon::cell::Cell;
use eidolon::testing::{running, FakeBidi, SCREENSHOT_BASE64};
use eidolon::Error;
use serde_json::json;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// What the next stream to the cell does.
enum Next {
    Serve(FakeBidi),
    /// `socat` ran and failed, as it does while nothing listens.
    Tool,
    /// A stream that ends at once.
    Closed,
    Unsupported,
}

struct Fake {
    cells: Arc<FakeDriver>,
    cell: Cell,
}

impl Fake {
    fn new(next: Vec<Next>) -> Fake {
        let next = Mutex::new(next.into_iter().collect::<VecDeque<_>>());
        let cells = Arc::new(FakeDriver::new());
        cells.on_stream(move |_| match next.lock().unwrap().pop_front() {
            Some(Next::Serve(fake)) => {
                let (ours, theirs) = duplex();
                tokio::spawn(fake.serve(theirs));
                Ok(ours)
            }
            Some(Next::Closed) => Ok(duplex().0),
            Some(Next::Tool) => Err(AdytumError::Tool { what: "limactl shell".into(), status: Some(1), stderr: "connection refused".into() }),
            Some(Next::Unsupported) | None => Err(AdytumError::Unsupported { what: "streams".into() }),
        });
        let cell = running(&cells, &"adytum-test-browser".parse().unwrap());
        Fake { cells, cell }
    }

    fn streams(&self) -> Vec<Vec<String>> {
        self.cells.calls().into_iter().filter_map(|c| if let DriverCall::Stream { argv, .. } = c { Some(argv) } else { None }).collect()
    }

    fn stream_count(&self) -> usize {
        self.streams().len()
    }
}

fn quick() -> ConnectOptions {
    ConnectOptions { attempts: 5, gap: Duration::ZERO, ready_within: Duration::from_secs(5) }
}

#[tokio::test]
async fn the_browser_is_reached_as_a_stream_and_ready_means_the_protocol_answered() {
    let fake = FakeBidi::new();
    let page = fake.page();
    let driver = Fake::new(vec![Next::Serve(fake)]);
    let browser = Browser::new(driver.cell.clone(), quick());
    browser.send("session.status", json!({})).await.unwrap();
    // The only way in is socat on the guest's own loopback.
    assert_eq!(driver.streams(), vec![vec!["socat".to_owned(), "-".into(), "TCP:127.0.0.1:9222".into()]]);
    assert!(driver.cells.calls().iter().all(|c| matches!(c, DriverCall::Stream { .. })), "nothing but the stream ran in the cell");
    // Ready meant the browser answered, then it was given a session.
    assert_eq!(page.methods(), ["session.status", "session.new", "session.status"]);
}

#[tokio::test]
async fn the_typed_commands_reach_the_page() {
    let fake = FakeBidi::new();
    let page = fake.page();
    let driver = Fake::new(vec![Next::Serve(fake)]);
    let browser = Browser::new(driver.cell.clone(), quick());
    let tab = browser.new_tab().await.unwrap();
    assert_eq!(browser.contexts().await.unwrap().iter().map(|c| c.context.as_str()).collect::<Vec<_>>(), ["ctx-1", tab.as_str()]);
    browser.close(&tab).await.unwrap();
    assert_eq!(browser.contexts().await.unwrap().len(), 1);

    assert_eq!(browser.navigate("ctx-1", "https://example.test/form").await.unwrap(), "https://example.test/form");
    assert_eq!(browser.title("ctx-1").await.unwrap(), "Page at https://example.test/form");
    browser.point_at("ctx-1", "node-2", true).await.unwrap();
    browser.type_text("ctx-1", "hello").await.unwrap();
    assert_eq!(page.field_value("#name").as_deref(), Some("hello"));
    browser.point_at("ctx-1", "node-1", false).await.unwrap();
    assert_eq!(page.clicks_on("#go"), 0, "a hover is not a click");
    browser.point_at("ctx-1", "node-1", true).await.unwrap();
    assert_eq!(page.clicks_on("#go"), 1);
    browser.set_files("ctx-1", "node-3", &["/tmp/a.txt"]).await.unwrap();
    assert_eq!(page.files("#upload"), ["/tmp/a.txt"]);
    assert!(browser.set_files("ctx-1", "node-1", &["/tmp/a.txt"]).await.is_err(), "a file goes only in a file input");
    assert_eq!(browser.screenshot("ctx-1").await.unwrap(), SCREENSHOT_BASE64);
}

#[tokio::test]
async fn it_tries_the_stream_again_while_the_browser_starts() {
    // Nothing listens twice (socat fails, then the stream ends), then it does.
    let driver = Fake::new(vec![Next::Tool, Next::Closed, Next::Serve(FakeBidi::new())]);
    Browser::new(driver.cell.clone(), quick()).send("session.status", json!({})).await.unwrap();
    assert_eq!(driver.stream_count(), 3);
}

#[tokio::test]
async fn it_gives_up_after_its_bound_and_says_what_it_last_saw() {
    let driver = Fake::new((0..10).map(|_| Next::Tool).collect());
    let err = Browser::new(driver.cell.clone(), quick()).send("session.status", json!({})).await.unwrap_err();
    assert!(matches!(err, Error::GaveUp { attempts: 5, .. }), "{err}");
    assert_eq!(driver.stream_count(), 5);
    assert!(err.to_string().contains("connection refused"));
}

#[tokio::test]
async fn an_error_that_trying_again_cannot_fix_is_not_retried() {
    let driver = Fake::new(vec![Next::Unsupported, Next::Serve(FakeBidi::new())]);
    let err = Browser::new(driver.cell.clone(), quick()).send("session.status", json!({})).await.unwrap_err();
    assert!(matches!(err, Error::Cell(_)), "{err}");
    assert_eq!(driver.stream_count(), 1);
}

#[tokio::test]
async fn a_browser_that_says_it_is_not_ready_is_an_error_after_the_bound() {
    let driver = Fake::new(vec![Next::Serve(FakeBidi::new().fail("session.status", "unknown error", "starting"))]);
    let err = Browser::new(driver.cell.clone(), ConnectOptions { attempts: 1, ..quick() }).send("session.status", json!({})).await.unwrap_err();
    assert!(err.to_string().contains("starting"), "{err}");
}

#[tokio::test]
async fn after_the_connection_drops_the_next_command_succeeds_on_a_new_one() {
    // Message 1 is the readiness check, 2 opens the session; message 3 drops the connection.
    let second = FakeBidi::new();
    let second_page = second.page();
    let driver = Fake::new(vec![Next::Serve(FakeBidi::new().drop_on_message(3)), Next::Serve(second)]);
    let browser = Browser::new(driver.cell.clone(), quick());
    assert!(browser.send("session.status", json!({})).await.is_err(), "the command that met the drop is not repeated");
    assert_eq!(driver.stream_count(), 1);
    assert_eq!(browser.send("session.status", json!({})).await.unwrap()["ready"], true);
    assert_eq!(driver.stream_count(), 2, "a second connection was made");
    assert_eq!(second_page.methods(), ["session.status", "session.new", "session.status"]);
}

#[tokio::test]
async fn a_remote_error_does_not_drop_the_connection() {
    let driver = Fake::new(vec![Next::Serve(FakeBidi::new())]);
    let browser = Browser::new(driver.cell.clone(), quick());
    assert!(browser.send("nonsense.method", json!({})).await.is_err());
    browser.send("session.status", json!({})).await.unwrap();
    assert_eq!(driver.stream_count(), 1);
}
