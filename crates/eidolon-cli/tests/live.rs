//! Live tests, run only with `EIDOLON_LIVE=1`:
//!
//! - `firefox_profile_moves_between_two_machines`: a real headless Firefox on
//!   a throwaway profile in a temporary folder. It never touches another
//!   profile, and `-no-remote` keeps it from talking to a Firefox already
//!   running.
//! - `stock_tools_open_a_bundle_and_check_a_seat`: a Lima VM with its own
//!   `LIMA_HOME` under /tmp, named `adytum-test-*`, deleted at the end.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::panic, reason = "in a test, a failure should say where")]

mod common;

use common::{write_tree, Machine};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const FIREFOX: &str = "/Applications/Firefox.app/Contents/MacOS/firefox";

fn live() -> bool {
    std::env::var("EIDOLON_LIVE").is_ok_and(|v| v == "1")
}

fn step(start: &mut Instant, what: &str) {
    eprintln!("{what}: {:.1} s", start.elapsed().as_secs_f64());
    *start = Instant::now();
}

/// A page on 127.0.0.1 that sets a cookie and records the cookie each request brought.
fn cookie_server(token: &str) -> (String, Arc<Mutex<Vec<Option<String>>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (seen2, token) = (seen.clone(), token.to_owned());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut cookie = None;
            let mut first = String::new();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let _ = reader.read_line(&mut first);
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                    break;
                }
                if let Some(v) = line.strip_prefix("Cookie:").or_else(|| line.strip_prefix("cookie:")) {
                    cookie = Some(v.trim().to_owned());
                }
            }
            if first.starts_with("GET / ") {
                seen2.lock().unwrap().push(cookie);
            }
            let body = "<html><body>eidolon live test</body></html>";
            let _ = write!(stream, "HTTP/1.1 200 OK\r\nSet-Cookie: eid={token}; Max-Age=86400; Path=/\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        }
    });
    (url, seen)
}

fn firefox(profile: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(FIREFOX);
    c.args(["-headless", "-no-remote", "-new-instance", "-profile"]).arg(profile).args(args).stdout(Stdio::null()).stderr(Stdio::null());
    c
}

fn visit(profile: &Path, url: &str, shot: &Path) {
    let status = firefox(profile, &["--screenshot", shot.to_str().unwrap(), url]).status().unwrap();
    assert!(status.success(), "firefox --screenshot");
}

fn quit(mut child: Child) {
    Command::new("kill").arg(child.id().to_string()).status().unwrap();
    child.wait().unwrap();
}

#[test]
fn firefox_profile_moves_between_two_machines() {
    if !live() || !Path::new(FIREFOX).exists() {
        eprintln!("skipped: set EIDOLON_LIVE=1 on a Mac with Firefox");
        return;
    }
    let all = Instant::now();
    let mut t = Instant::now();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let (desk, tower) = (Machine::new(root, "desk"), Machine::new(root, "tower"));
    let keys = [desk.key_line(), tower.key_line()];
    for m in [&desk, &tower] {
        m.run(&["holders", "add", "desk", &keys[0]]).ok();
        m.run(&["holders", "add", "tower", &keys[1]]).ok();
    }
    let (desk_profile, tower_profile) = (root.join("desk-profile"), root.join("tower-profile"));
    std::fs::create_dir(&desk_profile).unwrap();
    desk.run(&["target", "add", "firefox", "--path", desk_profile.to_str().unwrap()]).ok();
    tower.run(&["target", "add", "firefox", "--path", tower_profile.to_str().unwrap()]).ok();

    let token = format!("t{}", std::process::id());
    let (url, seen) = cookie_server(&token);
    visit(&desk_profile, &url, &root.join("first.png"));
    assert_eq!(seen.lock().unwrap().clone(), vec![None], "the first run had no cookie");
    step(&mut t, "first Firefox run");

    let running = firefox(&desk_profile, &["about:blank"]).spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let refused = loop {
        let code = desk.run(&["seal", "firefox"]);
        if code.code != 0 {
            let c = code.refused();
            if c == "app_running" || Instant::now() > deadline {
                break c;
            }
        } else if Instant::now() > deadline {
            break "sealed while Firefox ran".to_owned();
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    quit(running);
    assert_eq!(refused, "app_running");
    step(&mut t, "seal refused while Firefox runs");

    let sealed = desk.run(&["seal", "firefox"]).ok().json();
    eprintln!("sealed {} files, {} bytes into a {} byte bundle", sealed["counts"]["files"], sealed["counts"]["bytes"], sealed["size"]);
    step(&mut t, "seal");
    let receiver = tower.argv(&["receive"]);
    let mut args = vec!["send", "firefox", "--to", "tower", "--clear", "--"];
    args.extend(receiver.iter().map(String::as_str));
    desk.run(&args).ok();
    assert!(!desk_profile.exists(), "--clear removed desk's profile after the ack");
    step(&mut t, "move to tower");
    tower.run(&["open", "firefox"]).ok();
    step(&mut t, "open on tower");

    visit(&tower_profile, &url, &root.join("second.png"));
    assert_eq!(seen.lock().unwrap().last().cloned().flatten(), Some(format!("eid={token}")), "the restored profile still has the cookie the first run set");
    assert!(std::fs::read_to_string(tower_profile.join("prefs.js")).unwrap().contains("user_pref("), "Firefox kept using the profile");
    step(&mut t, "second Firefox run on the restored profile");
    assert_eq!(desk.run(&["open", "firefox"]).refused(), "not_the_holder");
    eprintln!("firefox live test: {:.1} s in all", all.elapsed().as_secs_f64());
}

struct Vm {
    home: PathBuf,
    name: String,
}

impl Vm {
    fn limactl(&self) -> Command {
        let mut c = Command::new("limactl");
        c.env("LIMA_HOME", &self.home);
        c
    }

    fn shell(&self, script: &str) -> std::process::Output {
        self.limactl().args(["shell", "--workdir", "/tmp", &self.name, "--", "bash", "-c", script]).output().unwrap()
    }
}

impl Drop for Vm {
    fn drop(&mut self) {
        let _ = self.limactl().args(["delete", "--force", &self.name]).status();
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

#[test]
fn stock_tools_open_a_bundle_and_check_a_seat() {
    if !live() || Command::new("limactl").arg("--version").output().is_err() {
        eprintln!("skipped: set EIDOLON_LIVE=1 with limactl on PATH");
        return;
    }
    let all = Instant::now();
    let mut t = Instant::now();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let desk = Machine::new(root, "desk");
    desk.run(&["holders", "add", "desk", &desk.key_line()]).ok();
    let notes = root.join("notes");
    write_tree(&notes, &[("a.txt", "opened by stock tools"), ("deep/b.txt", "b")]);
    std::fs::write(root.join("notes.toml"), "include = [\"**\"]\n").unwrap();
    desk.run(&["target", "add", "notes", "--file", root.join("notes.toml").to_str().unwrap(), "--path", notes.to_str().unwrap()]).ok();
    let sealed = desk.run(&["seal", "notes"]).ok().json();
    let bundle = desk.home.join(format!("targets/notes/bundles/{}.age", sealed["bundle"].as_str().unwrap()));
    let seat = desk.home.join("targets/notes/seats/00000000000000000001.json");

    let id = std::process::id();
    let vm = Vm { home: PathBuf::from(format!("/tmp/eidolon-lima-{id}")), name: format!("adytum-test-eidolon-{id}") };
    let started = vm.limactl().args(["start", "--yes", "--plain", "--name", &vm.name, "template:ubuntu"]).stdout(Stdio::null()).stderr(Stdio::null()).status().unwrap();
    assert!(started.success(), "limactl start");
    step(&mut t, "VM start");
    let install = vm.shell("sudo DEBIAN_FRONTEND=noninteractive apt-get update -qq && sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq age zstd openssh-client tar >/dev/null && age --version");
    assert!(install.status.success(), "apt: {}", String::from_utf8_lossy(&install.stderr));
    eprintln!("age in the VM: {}", String::from_utf8_lossy(&install.stdout).trim());
    step(&mut t, "apt install age zstd openssh-client");
    for (from, to) in [(&bundle, "bundle.age"), (&desk.key, "key"), (&seat, "seat.json"), (&seat.with_extension("json.sig"), "seat.json.sig"), (&desk.home.join("holders"), "holders")] {
        let copied = vm.limactl().args(["copy", from.to_str().unwrap(), &format!("{}:/tmp/{to}", vm.name)]).status().unwrap();
        assert!(copied.success(), "copy {to}");
    }
    let opened = vm.shell("chmod 600 key && mkdir -p out && age -d -i key bundle.age | zstd -d | tar -x -C out && cat out/a.txt && cat out/deep/b.txt");
    assert!(opened.status.success(), "stock open: {}", String::from_utf8_lossy(&opened.stderr));
    assert_eq!(String::from_utf8_lossy(&opened.stdout), "opened by stock toolsb");
    let verified = vm.shell(&format!("ssh-keygen -Y verify -f holders -I desk -n {} -s seat.json.sig < seat.json", eidolon::SEAT_NAMESPACE));
    assert!(verified.status.success(), "ssh-keygen: {}", String::from_utf8_lossy(&verified.stderr));
    eprintln!("{}", String::from_utf8_lossy(&verified.stdout).trim());
    let forged = vm.shell(&format!("sed 's/\"generation\":1/\"generation\":2/' seat.json > forged.json && ssh-keygen -Y verify -f holders -I desk -n {} -s seat.json.sig < forged.json", eidolon::SEAT_NAMESPACE));
    assert!(!forged.status.success(), "a changed seat does not verify");
    step(&mut t, "stock open and verify");
    drop(vm);
    step(&mut t, "VM delete");
    eprintln!("stock tools live test: {:.1} s in all", all.elapsed().as_secs_f64());
}
