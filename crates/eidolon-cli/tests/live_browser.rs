//! Live proof 3: an eidolon with a folder facet and a browser facet, sealed
//! in one real cell, moved to a second, and opened there. The folder is
//! byte for byte, the browser is still signed in, the first machine is
//! refused, and a second move after a short session sends only what changed.
//!
//! Skipped unless `ADYTUM_LIVE=1`. Run it as
//! `ADYTUM_LIVE=1 cargo test -p eidolon-cli --test live_browser -- --nocapture`.
//!
//! To be quick it makes one base cell, sets it up for the browser once,
//! stops it, and clones it into cells A and B (a clone takes a moment).
//! All three are `adytum-test-*` in a Lima home of their own under a short
//! `ADYTUM_HOME`, and a drop guard deletes them, also when the test fails.
//! The person's sign-in is stood in for by automation on a test page served
//! inside each cell; the owner signs in through `eidolon view` later.
#![cfg(unix)]
#![expect(clippy::unwrap_used, reason = "in a test, a failure should say where")]

#[expect(dead_code, reason = "the helpers are shared; each test uses some")]
mod common;

use adytum::lima::{lima_home_in, Lima};
use adytum::{CellName, CellSpec, Cells, Driver, ExecOpts, Resources};
use common::{write_tree, Machine, Sandbox};
use eidolon::browser::{Browser, ConnectOptions};
use eidolon::cell::Cell;
use eidolon::facet::firefox::AUTOMATION_PORT;
use eidolon::facet::plan;
use eidolon::home::FileKeyStore;
use eidolon::mcp::snapshot::PAGE_TEXT_JS;
use eidolon::model::{EidolonId, FacetKind};
use eidolon::moving::Holding;
use eidolon::pack::{load, Node, SignedHead};
use eidolon::vault::{ChunkName, KeyRing, Keys, Vault};
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

const WALL: &str = "wall_live";
const FOLDER: &str = "/tmp/eidolon-live/notes";
const PAGE_PORT: u16 = 8000;

/// The test page's server, written into the guest. Python 3 is on the image.
/// `/` says who is signed in (by the `session` cookie) or shows a form;
/// `POST /login` sets the cookie for 30 days; `/download` is an attachment;
/// `POST /upload` counts the bytes it was given.
const SERVER: &str = r#"
import html
import http.server
import urllib.parse


class Page(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def cookie(self):
        for part in self.headers.get("Cookie", "").split(";"):
            key, _, value = part.strip().partition("=")
            if key == "session" and value:
                return value
        return None

    def reply(self, code, body, kind="text/html", extra=()):
        data = body if isinstance(body, bytes) else body.encode()
        self.send_response(code)
        self.send_header("Content-Type", kind)
        self.send_header("Content-Length", str(len(data)))
        for key, value in extra:
            self.send_header(key, value)
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        path = urllib.parse.urlsplit(self.path).path
        if path == "/":
            who = self.cookie()
            if who:
                self.reply(200, "<!doctype html><title>home</title><p id='who'>signed in as %s</p>" % html.escape(who))
            else:
                self.reply(
                    200,
                    "<!doctype html><title>sign in</title><form method='post' action='/login'>"
                    "<input id='user' name='user'><button id='go' type='submit'>Sign in</button></form>",
                )
        elif path == "/download":
            self.reply(200, b"report line\n" * 2000, "application/octet-stream", [("Content-Disposition", "attachment; filename=\"report.txt\"")])
        elif path.startswith("/page/"):
            self.reply(200, "<!doctype html><title>page</title><p>page %s</p>" % html.escape(path[6:]))
        else:
            self.reply(404, "not found")

    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("Content-Length", "0")))
        path = urllib.parse.urlsplit(self.path).path
        if path == "/login":
            user = urllib.parse.parse_qs(body.decode()).get("user", ["tester"])[0]
            user = "".join(c for c in user if c.isalnum()) or "tester"
            self.reply(303, "", extra=[("Location", "/"), ("Set-Cookie", "session=%s; Max-Age=2592000; HttpOnly; Path=/" % user)])
        elif path == "/upload":
            self.reply(200, "received %d bytes" % len(body), "text/plain")
        else:
            self.reply(404, "not found")


http.server.ThreadingHTTPServer(("127.0.0.1", 8000), Page).serve_forever()
"#;

/// Deletes the cells and Adytum's folder, whatever happens.
struct Cleanup {
    home: PathBuf,
    names: Vec<String>,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        for name in &self.names {
            // Only our own names, only in our own Lima home.
            assert!(name.starts_with("adytum-test-"));
            let _ = Command::new("limactl").env("LIMA_HOME", lima_home_in(&self.home)).args(["delete", "--force", "--tty=false", name]).output();
        }
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn spec(name: &str) -> CellSpec {
    CellSpec { resources: Resources { cpus: 2, memory_mib: 3072, disk_gib: 10 }, automation_port: Some(AUTOMATION_PORT), ..common::spec(name, WALL) }
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| (*s).to_owned()).collect()
}

/// Runs a script in the cell as root, so a test reads everything.
async fn sh(lima: &Lima, cell: &CellName, script: &str) -> String {
    let out = lima.exec(cell, &argv(&["sh", "-c", script]), ExecOpts { root: true, ..ExecOpts::default() }).await.unwrap();
    assert!(out.success(), "{script}: {}", out.stderr_lossy());
    out.stdout_lossy()
}

/// Mode, kind, link target and checksum of everything under the folder,
/// without what the facet leaves out.
async fn listing(lima: &Lima, cell: &CellName) -> String {
    let all = sh(lima, cell, &format!("cd {FOLDER} && find . -printf '%y %m %p %l\\n' | LC_ALL=C sort && find . -type f -exec sha256sum {{}} + | LC_ALL=C sort -k2")).await;
    all.lines().filter(|l| !l.contains("./cache")).collect::<Vec<_>>().join("\n")
}

/// Makes `to` as a copy of the stopped cell `from`.
async fn clone_cell(cells: &Cells, from: &CellName, to: &str) {
    cells.create(&CellSpec { clone_from: Some(from.clone()), ..spec(to) }).await.unwrap();
}

/// Starts the test page in a cell, as a transient unit, and waits until it
/// answers. A clone can carry a unit of the same name from the base, so any
/// old one is stopped and cleared first. On failure the panic carries the
/// unit's status and journal, so the cause is in the test output.
async fn serve_page(lima: &Lima, cell: &CellName) {
    let run = |parts: &[&str]| {
        let parts = argv(parts);
        async move { lima.exec(cell, &parts, ExecOpts { root: true, ..ExecOpts::default() }).await.unwrap() }
    };
    // Best effort: the unit usually does not exist, and that is fine.
    run(&["systemctl", "stop", "eidolon-testpage.service"]).await;
    run(&["systemctl", "reset-failed", "eidolon-testpage.service"]).await;
    let started = run(&["systemd-run", "--unit=eidolon-testpage", "--collect", "--property=Restart=on-failure", "python3", "/opt/testpage/server.py"]).await;
    assert!(started.success(), "systemd-run did not start the test page in {cell}: {}", started.stderr_lossy());
    // The server is a moment behind its unit: poll until the unit is active
    // and the page answers. Each poll is bounded.
    let target = format!("http://127.0.0.1:{PAGE_PORT}/");
    let mut up = false;
    for _ in 0..50 {
        let active = run(&["systemctl", "is-active", "--quiet", "eidolon-testpage.service"]).await;
        if active.success() {
            let answer = run(&["curl", "-fsS", "--max-time", "2", "-o", "/dev/null", &target]).await;
            up = answer.success();
            if up {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    if up {
        return;
    }
    let status = run(&["systemctl", "status", "eidolon-testpage.service", "--no-pager"]).await;
    let journal = run(&["journalctl", "-u", "eidolon-testpage.service", "--no-pager", "-n", "50"]).await;
    let script = run(&["ls", "-l", "/opt/testpage/server.py"]).await;
    let curl = run(&["curl", "-sv", "--max-time", "3", "-o", "/dev/null", &target]).await;
    assert!(
        up,
        "the test page did not answer in {cell}\n-- status:\n{}{}\n-- journal:\n{}\n-- server.py:\n{}{}\n-- curl:\n{}",
        status.stdout_lossy(),
        status.stderr_lossy(),
        journal.stdout_lossy(),
        script.stdout_lossy(),
        script.stderr_lossy(),
        curl.stderr_lossy()
    );
}

/// A fresh page in the cell's browser, over the stream.
async fn browse(lima: &Arc<Lima>, cell: &CellName) -> (Browser, String) {
    let browser = Browser::new(Cell::new(lima.clone(), cell.clone()), ConnectOptions::default());
    let page = browser.new_tab().await.unwrap();
    (browser, page)
}

async fn text_of(browser: &Browser, page: &str) -> String {
    let value = browser.call_function(page, PAGE_TEXT_JS, vec![]).await.unwrap();
    value.pointer("/value").and_then(|v| v.as_str()).unwrap_or_default().to_owned()
}

/// The element `css` finds on the page, by the browser's id for it.
async fn locate(browser: &Browser, page: &str, css: &str) -> String {
    let found = browser.send("browsingContext.locateNodes", serde_json::json!({ "context": page, "locator": { "type": "css", "value": css } })).await.unwrap();
    found.pointer("/nodes/0/sharedId").and_then(|v| v.as_str()).unwrap().to_owned()
}

/// Waits for the page to show `want`. A click that submits a form returns
/// before the next page is there; this is the test waiting for it, not the
/// product.
async fn expect_text(session: &Browser, page: &str, want: &str) {
    let mut last = String::new();
    for _ in 0..100 {
        last = text_of(session, page).await;
        if last.contains(want) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(last.contains(want), "the page never showed {want:?}; it shows {last:?}");
}

fn url(path: &str) -> String {
    format!("http://127.0.0.1:{PAGE_PORT}{path}")
}

struct Steps(Vec<(String, f64)>);

impl Steps {
    fn time<T>(&mut self, name: &str, started: Instant, value: T) -> T {
        self.0.push((name.to_owned(), started.elapsed().as_secs_f64()));
        value
    }
}

/// What a sealed eidolon holds, per facet: the chunk names and each one's
/// sealed size on disk.
struct Held {
    folder: Vec<(ChunkName, u64)>,
    browser: Vec<(ChunkName, u64)>,
}

/// Reads the head a machine holds. The machine must still have the key, so
/// this runs before a move hands the eidolon away.
fn held(home: &std::path::Path, eid: &str) -> Held {
    let store = FileKeyStore::open(&home.join("keys")).unwrap();
    let id: EidolonId = eid.parse().unwrap();
    let key = KeyRing::new(&store).get(eid).unwrap().unwrap();
    let keys = Keys::new(&key).unwrap();
    let vault = Vault::open(&home.join("vault")).unwrap();
    let holding = Holding::load(&home.join("holdings"), &id).unwrap();
    let head: SignedHead = serde_json::from_slice(&holding.head).unwrap();
    let manifest = load(&head, &keys, &vault).unwrap();
    let mut out = Held { folder: Vec::new(), browser: Vec::new() };
    for facet in &manifest.facets {
        let mut names = BTreeSet::new();
        for entry in &facet.entries {
            if let Node::File { chunks, .. } = &entry.node {
                names.extend(chunks.iter().copied());
            }
        }
        let sized: Vec<(ChunkName, u64)> = names.into_iter().map(|n| (n, vault.get(&n).unwrap().len() as u64)).collect();
        match facet.kind {
            FacetKind::Folder => out.folder = sized,
            FacetKind::Browser => out.browser = sized,
        }
    }
    out
}

/// Chunks and bytes of `after` that `before` did not have: what a move sends.
fn added(before: &[(ChunkName, u64)], after: &[(ChunkName, u64)]) -> (usize, u64) {
    let had: BTreeSet<&ChunkName> = before.iter().map(|(n, _)| n).collect();
    let new: Vec<&(ChunkName, u64)> = after.iter().filter(|(n, _)| !had.contains(n)).collect();
    (new.len(), new.iter().map(|(_, size)| size).sum())
}

fn total(chunks: &[(ChunkName, u64)]) -> (usize, u64) {
    (chunks.len(), chunks.iter().map(|(_, size)| size).sum())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_signed_in_browser_and_a_folder_move_between_two_real_cells_together() {
    if std::env::var("ADYTUM_LIVE").as_deref() != Ok("1") {
        eprintln!("skipped: set ADYTUM_LIVE=1 to run against Lima");
        return;
    }
    let tag = format!("{:05x}", std::process::id() & 0xf_ffff);
    let (base_name, a_name, b_name) = (format!("adytum-test-base-{tag}"), format!("adytum-test-a-{tag}"), format!("adytum-test-b-{tag}"));
    // Short, because Lima keeps unix sockets under it.
    let home = PathBuf::from(format!("/tmp/eidl-{tag}"));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let _cleanup = Cleanup { home: home.clone(), names: vec![base_name.clone(), a_name.clone(), b_name.clone()] };
    let mut steps = Steps(Vec::new());

    // ---- one base cell, set up once, then two clones ---------------------------
    let lima = Arc::new(Lima::system(lima_home_in(&home)).unwrap());
    let cells = Cells::open(&home, lima.clone()).unwrap();
    let (base, a, b): (CellName, CellName, CellName) = (base_name.parse().unwrap(), a_name.parse().unwrap(), b_name.parse().unwrap());
    let t = Instant::now();
    cells.create(&spec(&base_name)).await.unwrap();
    cells.start(&base).await.unwrap();
    steps.time("create and start the base cell", t, ());
    let t = Instant::now();
    plan::provision().run(&Cell::new(lima.clone(), base.clone())).await.unwrap();
    steps.time("set the base cell up for the browser (once)", t, ());
    // The test page's server lives in the base, so both clones have it.
    let page = ExecOpts { stdin: Some(SERVER.as_bytes().to_vec()), root: true, ..ExecOpts::default() };
    sh(&lima, &base, "install -d -m 0755 /opt/testpage").await;
    let wrote = lima.exec(&base, &argv(&["tee", "/opt/testpage/server.py"]), page).await.unwrap();
    assert!(wrote.success());
    // Flushed before the stop, so both clones see the whole file.
    sh(&lima, &base, "sync").await;
    let t = Instant::now();
    cells.stop(&base).await.unwrap();
    steps.time("stop the base cell", t, ());
    let t = Instant::now();
    clone_cell(&cells, &base, &a_name).await;
    clone_cell(&cells, &base, &b_name).await;
    steps.time("clone the base into A and B", t, ());
    let t = Instant::now();
    let (started_a, started_b) = tokio::join!(cells.start(&a), cells.start(&b));
    started_a.unwrap();
    started_b.unwrap();
    steps.time("start A and B", t, ());
    serve_page(&lima, &a).await;
    serve_page(&lima, &b).await;

    // ---- two machines and an eidolon -----------------------------------------------
    let sb = Sandbox::new();
    let env = vec![
        ("PATH".to_owned(), std::env::var("PATH").unwrap_or_default()),
        ("HOME".to_owned(), std::env::var("HOME").unwrap_or_default()),
        ("ADYTUM_HOME".to_owned(), home.display().to_string()),
    ];
    let machine = |name: &str| {
        let m = Machine { home: sb.home(name), cells: None, name: name.to_owned(), env: env.clone() };
        assert_eq!(m.run(&["machine", "init", "--name", name]).code, 0);
        m
    };
    let (desk, tower) = (machine("desk"), machine("tower"));
    let holders = [desk.holder_file(&sb.root), tower.holder_file(&sb.root)];
    let made =
        desk.run(&["new", "--wall", WALL, "--holder", &holders[0], "--holder", &holders[1], "--folder", &format!("notes={FOLDER}"), "--leave-out", "cache", "--browser", "web"]);
    assert_eq!(made.code, 0, "{}{}", made.stderr, made.stdout);
    let eid = made.json()["eidolon"].as_str().unwrap().to_owned();

    // ---- in A: sign in, write the folder -----------------------------------------------
    let t = Instant::now();
    plan::start_browser().run(&Cell::new(lima.clone(), a.clone())).await.unwrap();
    let (session, tab) = browse(&lima, &a).await;
    session.navigate(&tab, &url("/")).await.unwrap();
    assert!(!text_of(&session, &tab).await.contains("signed in as"), "the page shows a form before the sign-in");
    let field = locate(&session, &tab, "#user").await;
    session.point_at(&tab, &field, true).await.unwrap();
    session.type_text(&tab, "tester").await.unwrap();
    let go = locate(&session, &tab, "#go").await;
    session.point_at(&tab, &go, true).await.unwrap();
    expect_text(&session, &tab, "signed in as tester").await;
    drop(session);
    steps.time("sign in on A through automation", t, ());
    // The page works for what the test checks later.
    let served = sh(&lima, &a, &format!("curl -fsS -D- -o /dev/null http://127.0.0.1:{PAGE_PORT}/download; curl -fsS -d hello http://127.0.0.1:{PAGE_PORT}/upload")).await;
    assert!(served.contains("attachment") && served.contains("received 5 bytes"), "{served}");

    let seed = sb.root.join("seed/notes");
    write_tree(&seed);
    let t = Instant::now();
    sh(&lima, &a, "rm -rf /tmp/eidolon-live").await;
    lima.copy_in(&a, &seed, FOLDER, None).await.unwrap();
    steps.time("write the folder in A", t, ());
    let written = listing(&lima, &a).await;

    // ---- seal on desk, move to tower, open in B ------------------------------------------
    let t = Instant::now();
    let sealed = desk.run(&["seal", &eid, "--cell", &a_name]);
    assert_eq!(sealed.code, 0, "{}{}", sealed.stderr, sealed.stdout);
    steps.time("desk: seal from A (folder and browser)", t, ());
    let first_held = held(&desk.home, &eid);

    let t = Instant::now();
    let first = desk.move_to(&eid, &tower);
    assert_eq!(first.code, 0, "{}{}", first.stderr, first.stdout);
    let first = first.json();
    steps.time("move desk to tower", t, ());

    let refused = desk.run(&["open", &eid, "--cell", &a_name]);
    assert_eq!((refused.code, refused.code_of_error().as_str()), (1, "retired"), "desk's open is refused");

    let t = Instant::now();
    let opened = tower.run(&["open", &eid, "--cell", &b_name]);
    assert_eq!(opened.code, 0, "{}{}", opened.stderr, opened.stdout);
    steps.time("tower: open in B (place, start the browser)", t, ());

    let in_b = listing(&lima, &b).await;
    assert_eq!(in_b, written, "the folder in B matches A in every byte, mode and link");
    let owners =
        sh(&lima, &b, "find /home/eidolon/profile /home/eidolon/files ! -user eidolon -printf '%u %p\\n'; find /home/eidolon -perm -o+w ! -type l -printf 'o+w %p\\n'").await;
    assert_eq!(owners, "", "the placed profile and the files folders are the browser's user's alone");

    let t = Instant::now();
    let (session, tab) = browse(&lima, &b).await;
    session.navigate(&tab, &url("/")).await.unwrap();
    let shown = text_of(&session, &tab).await;
    assert!(shown.contains("signed in as tester"), "B's browser asks for no sign-in; the page shows {shown:?}");
    steps.time("B: connect and see the page signed in", t, ());

    // ---- a short session in B, seal, move back ------------------------------------------------
    let t = Instant::now();
    for page in ["/page/one", "/page/two"] {
        session.navigate(&tab, &url(page)).await.unwrap();
        expect_text(&session, &tab, &format!("page {}", &page[6..])).await;
    }
    drop(session);
    steps.time("B: a short session (two pages)", t, ());

    let t = Instant::now();
    let resealed = tower.run(&["seal", &eid]);
    assert_eq!(resealed.code, 0, "{}{}", resealed.stderr, resealed.stdout);
    steps.time("tower: seal from B", t, ());
    let second_held = held(&tower.home, &eid);

    let t = Instant::now();
    let second = tower.move_to(&eid, &desk);
    assert_eq!(second.code, 0, "{}{}", second.stderr, second.stdout);
    let second = second.json();
    steps.time("move tower to desk", t, ());

    let t = Instant::now();
    let back = desk.run(&["open", &eid, "--cell", &a_name]);
    assert_eq!(back.code, 0, "{}{}", back.stderr, back.stdout);
    steps.time("desk: open in A again", t, ());
    let (session, tab) = browse(&lima, &a).await;
    session.navigate(&tab, &url("/")).await.unwrap();
    assert!(text_of(&session, &tab).await.contains("signed in as tester"), "still signed in after the way back");
    drop(session);
    assert_eq!(tower.run(&["open", &eid, "--cell", &b_name]).code_of_error(), "retired", "tower let it go");

    // ---- the second move sent only what changed ----------------------------------------------------
    let (sent, offered) = (second["chunks_sent"].as_u64().unwrap(), second["chunks_offered"].as_u64().unwrap());
    let (first_sent, first_offered) = (first["chunks_sent"].as_u64().unwrap(), first["chunks_offered"].as_u64().unwrap());
    assert_eq!(first_sent, first_offered, "tower had nothing yet");
    assert!(sent * 4 < offered, "only the changed chunks went: {sent} of {offered}");
    assert!(second["bytes_sent"].as_u64().unwrap() * 3 < first["bytes_sent"].as_u64().unwrap(), "{second} against {first}");

    // ---- the summary -----------------------------------------------------------------------------------------
    let mut table = String::from("\nstep                                                       seconds\n");
    for (name, secs) in &steps.0 {
        let _ = writeln!(table, "{name:<58} {secs:>8.2}");
    }
    for (label, m) in [("first move (desk to tower)", &first), ("second move (tower to desk)", &second)] {
        let _ = writeln!(table, "{label}: {} of {} chunks sent, {} bytes sent, {} seconds", m["chunks_sent"], m["chunks_offered"], m["bytes_sent"], m["seconds"]);
    }
    let (f_chunks, f_bytes) = total(&first_held.browser);
    let (fo_chunks, fo_bytes) = total(&first_held.folder);
    let (b_chunks, b_bytes) = added(&first_held.browser, &second_held.browser);
    let (n_chunks, n_bytes) = added(&first_held.folder, &second_held.folder);
    let _ = writeln!(table, "first move, sealed share:  browser {f_chunks} chunks / {f_bytes} bytes; folder {fo_chunks} chunks / {fo_bytes} bytes");
    let _ = writeln!(table, "second move, changed share: browser {b_chunks} chunks / {b_bytes} bytes; folder {n_chunks} chunks / {n_bytes} bytes");
    println!("{table}");
}
