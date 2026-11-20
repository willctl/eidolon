//! A cell's browser, reached as a byte stream: the cell runs `socat` to the
//! automation port on its own loopback, so the port is never forwarded.
//! Ready means the protocol answered `session.status`.

use crate::bidi::{Error as BidiError, Session};
use crate::cell::Cell;
use crate::error::{Error, Result};
use crate::facet::firefox::{stream_argv, AUTOMATION_PORT};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

#[derive(Debug, Clone, Copy)]
pub struct ConnectOptions {
    pub attempts: u32,
    pub gap: Duration,
    pub ready_within: Duration,
}

impl Default for ConnectOptions {
    fn default() -> Self {
        Self { attempts: 60, gap: Duration::from_millis(500), ready_within: Duration::from_secs(10) }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Context {
    pub context: String,
    pub url: String,
}

/// A browser in a cell that connects when first used and again after the
/// connection breaks. A command that met the break returns its error: it
/// may or may not have run, so it is not repeated.
pub struct Browser {
    cell: Cell,
    options: ConnectOptions,
    session: Mutex<Option<Arc<Session>>>,
}

fn not_up_yet(e: &Error) -> bool {
    match e {
        Error::Bidi(b) => matches!(b, BidiError::Io(_) | BidiError::Closed(_) | BidiError::Handshake(_) | BidiError::Timeout(_) | BidiError::NotReady(_)),
        e => crate::cell::ran_and_failed(e),
    }
}

fn connection_lost(e: &BidiError) -> bool {
    matches!(e, BidiError::Io(_) | BidiError::Closed(_) | BidiError::Handshake(_) | BidiError::Protocol(_) | BidiError::MessageTooLarge { .. })
}

fn protocol(what: &str) -> Error {
    Error::Bidi(BidiError::Protocol(format!("the answer to {what} has an unexpected shape")))
}

impl Browser {
    pub fn new(cell: Cell, options: ConnectOptions) -> Browser {
        Browser { cell, options, session: Mutex::new(None) }
    }

    async fn connect(&self) -> Result<Session> {
        let mut last = None;
        for attempt in 1..=self.options.attempts {
            let tried = async {
                let stream = self.cell.stream(&stream_argv(AUTOMATION_PORT)).await?;
                let session = Session::connect(stream, &format!("127.0.0.1:{AUTOMATION_PORT}")).await?;
                session.wait_ready(self.options.ready_within).await?;
                Ok::<_, Error>(session)
            };
            match tried.await {
                Ok(session) => return Ok(session),
                Err(e) if not_up_yet(&e) => {
                    last = Some(e.to_string());
                    if attempt < self.options.attempts && !self.options.gap.is_zero() {
                        tokio::time::sleep(self.options.gap).await;
                    }
                }
                Err(e) => return Err(e),
            }
        }
        Err(Error::GaveUp { attempts: self.options.attempts, last: last.unwrap_or_else(|| "no attempt was made".to_owned()) })
    }

    async fn session(&self) -> Result<Arc<Session>> {
        let mut current = self.session.lock().await;
        if let Some(session) = &*current {
            return Ok(Arc::clone(session));
        }
        let session = self.connect().await?;
        // A new connection takes no command but `session.status` until it has a session.
        session.new_session().await?;
        let session = Arc::new(session);
        *current = Some(Arc::clone(&session));
        Ok(session)
    }

    pub async fn send(&self, method: &str, params: Value) -> Result<Value> {
        let session = self.session().await?;
        let answer = session.send(method, params).await;
        if answer.as_ref().is_err_and(connection_lost) {
            let mut current = self.session.lock().await;
            if current.as_ref().is_some_and(|s| Arc::ptr_eq(s, &session)) {
                *current = None;
            }
        }
        Ok(answer?)
    }

    pub async fn contexts(&self) -> Result<Vec<Context>> {
        let answer = self.send("browsingContext.getTree", json!({})).await?;
        serde_json::from_value(answer.get("contexts").cloned().unwrap_or(Value::Null)).map_err(|_| protocol("browsingContext.getTree"))
    }

    pub async fn title(&self, context: &str) -> Result<String> {
        let answer = self.send("script.evaluate", json!({ "expression": "document.title", "target": { "context": context }, "awaitPromise": false })).await?;
        Ok(answer.pointer("/result/value").and_then(Value::as_str).unwrap_or_default().to_owned())
    }

    pub async fn new_tab(&self) -> Result<String> {
        let answer = self.send("browsingContext.create", json!({ "type": "tab" })).await?;
        answer.get("context").and_then(Value::as_str).map(str::to_owned).ok_or_else(|| protocol("browsingContext.create"))
    }

    pub async fn activate(&self, context: &str) -> Result<()> {
        self.send("browsingContext.activate", json!({ "context": context })).await.map(drop)
    }

    pub async fn close(&self, context: &str) -> Result<()> {
        self.send("browsingContext.close", json!({ "context": context })).await.map(drop)
    }

    pub async fn navigate(&self, context: &str, url: &str) -> Result<String> {
        let answer = self.send("browsingContext.navigate", json!({ "context": context, "url": url, "wait": "complete" })).await?;
        Ok(answer.get("url").and_then(Value::as_str).unwrap_or(url).to_owned())
    }

    pub async fn traverse_history(&self, context: &str, delta: i32) -> Result<()> {
        self.send("browsingContext.traverseHistory", json!({ "context": context, "delta": delta })).await.map(drop)
    }

    pub async fn screenshot(&self, context: &str) -> Result<String> {
        let answer = self.send("browsingContext.captureScreenshot", json!({ "context": context })).await?;
        answer.get("data").and_then(Value::as_str).map(str::to_owned).ok_or_else(|| protocol("browsingContext.captureScreenshot"))
    }

    pub async fn call_function(&self, context: &str, declaration: &str, arguments: Vec<Value>) -> Result<Value> {
        let params = json!({ "functionDeclaration": declaration, "arguments": arguments, "target": { "context": context }, "awaitPromise": false, "resultOwnership": "none" });
        let answer = self.send("script.callFunction", params).await?;
        match answer.get("type").and_then(Value::as_str) {
            Some("success") => Ok(answer.get("result").cloned().unwrap_or(Value::Null)),
            Some("exception") => {
                let text = answer.pointer("/exceptionDetails/text").and_then(Value::as_str).unwrap_or("an exception without text");
                Err(Error::Bidi(BidiError::Script(text.to_owned())))
            }
            _ => Err(protocol("script.callFunction")),
        }
    }

    pub async fn perform_actions(&self, context: &str, actions: Value) -> Result<()> {
        self.send("input.performActions", json!({ "context": context, "actions": actions })).await.map(drop)
    }

    pub async fn point_at(&self, context: &str, element: &str, click: bool) -> Result<()> {
        let mut steps = vec![json!({ "type": "pointerMove", "x": 0, "y": 0, "origin": { "type": "element", "element": { "sharedId": element } } })];
        if click {
            steps.push(json!({ "type": "pointerDown", "button": 0 }));
            steps.push(json!({ "type": "pointerUp", "button": 0 }));
        }
        self.perform_actions(context, json!([{ "type": "pointer", "id": "mouse", "parameters": { "pointerType": "mouse" }, "actions": steps }])).await
    }

    pub async fn type_text(&self, context: &str, text: &str) -> Result<()> {
        let keys: Vec<Value> = text.chars().flat_map(|c| [json!({ "type": "keyDown", "value": c.to_string() }), json!({ "type": "keyUp", "value": c.to_string() })]).collect();
        self.perform_actions(context, json!([{ "type": "key", "id": "keyboard", "actions": keys }])).await
    }

    pub async fn set_files(&self, context: &str, element: &str, paths: &[&str]) -> Result<()> {
        self.send("input.setFiles", json!({ "context": context, "element": { "sharedId": element }, "files": paths })).await.map(drop)
    }
}
