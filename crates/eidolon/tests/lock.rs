//! While the app holds its lock, nothing is captured or replaced; while
//! Eidolon holds it, the app cannot take it. The lock is held by a second
//! process, as an app would hold it: this test binary run again.
#![allow(clippy::unwrap_used, reason = "in a test, a failure should say where")]
#![cfg(unix)]

mod common;

use common::{read_tree, write_tree, World};
use eidolon::{Closed, Code, Dest, Limits, Source};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};

const HOLD: &str = "EIDOLON_TEST_HOLD_LOCK";

fn closed() -> Closed {
    Closed { app: "Notes".into(), unix_lock: Some(".lock".into()), windows_lock: None }
}

/// Run again as the app: takes the lock in the folder named by `HOLD`,
/// says `held`, and keeps it until its stdin closes.
#[test]
fn hold_the_lock_as_another_process() {
    let Ok(dir) = std::env::var(HOLD) else { return };
    let lock = eidolon::hold_app_lock(Path::new(&dir), &closed());
    let mut out = std::io::stdout();
    writeln!(out, "\n{}", if matches!(lock, Ok(Some(_))) { "held" } else { "busy" }).unwrap();
    out.flush().unwrap();
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
}

fn app_holding(dir: &Path) -> (Child, bool) {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "hold_the_lock_as_another_process", "--nocapture", "--test-threads=1"])
        .env(HOLD, dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Some(said) = ["held", "busy"].into_iter().find(|w| line.ends_with(w)) {
                let _ = tx.send(said == "held");
            }
        }
    });
    let held = rx.recv().unwrap();
    (child, held)
}

fn quit(mut child: Child) {
    drop(child.stdin.take());
    child.wait().unwrap();
}

#[test]
fn a_running_app_stops_seal_and_open_and_eidolon_keeps_the_app_out() {
    let w = World::new();
    let notes = w.path("desk-notes");
    write_tree(&notes, &[("a.md", "a")]);
    w.desk.seal("notes", Source::Folder(None), Vec::new()).unwrap();
    write_tree(&notes, &[("a.md", "edited while the app runs")]);

    let (app, held) = app_holding(&notes);
    assert!(held);
    assert_eq!(w.desk.seal("notes", Source::Folder(None), Vec::new()).err().and_then(|e| e.code()), Some(Code::AppRunning));
    assert_eq!(w.desk.open_target("notes", Dest::Folder(None), Limits::default()).err().and_then(|e| e.code()), Some(Code::AppRunning));
    assert!(read_tree(&notes).contains(&("a.md".into(), "edited while the app runs".into())), "open changed nothing");
    quit(app);

    let ours = eidolon::hold_app_lock(&notes, &closed()).unwrap();
    let (app, held) = app_holding(&notes);
    assert!(!held, "the app cannot start while Eidolon holds the lock");
    quit(app);
    drop(ours);

    w.desk.open_target("notes", Dest::Folder(None), Limits::default()).unwrap();
    assert!(read_tree(&notes).contains(&("a.md".into(), "a".into())));
}
