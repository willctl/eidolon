#![allow(dead_code, clippy::unwrap_used, clippy::panic, reason = "test helpers, each test file uses part of them; a failure should say where")]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

pub const BIN: &str = env!("CARGO_BIN_EXE_eidolon");

pub struct Run {
    pub code: i32,
    pub stdout: Vec<u8>,
    pub stderr: String,
}

impl Run {
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.stdout).unwrap_or_else(|e| panic!("not JSON ({e}): {}\nstderr: {}", String::from_utf8_lossy(&self.stdout), self.stderr))
    }

    pub fn ok(self) -> Run {
        assert_eq!(self.code, 0, "stderr: {}\nstdout: {}", self.stderr, String::from_utf8_lossy(&self.stdout));
        self
    }

    pub fn refused(&self) -> String {
        assert_eq!(self.code, 1, "stderr: {}", self.stderr);
        self.json()["error"]["code"].as_str().unwrap().to_owned()
    }
}

impl From<Output> for Run {
    fn from(o: Output) -> Run {
        Run { code: o.status.code().unwrap_or(-1), stdout: o.stdout, stderr: String::from_utf8_lossy(&o.stderr).into_owned() }
    }
}

#[derive(Clone)]
pub struct Machine {
    pub home: PathBuf,
    pub key: PathBuf,
}

impl Machine {
    pub fn new(root: &Path, name: &str) -> Machine {
        Machine { home: root.join(name), key: root.join(format!("{name}.key")) }
    }

    /// The command line that runs eidolon as this machine.
    pub fn argv(&self, args: &[&str]) -> Vec<String> {
        let mut argv = vec![BIN.to_owned(), "--json".into(), "--home".into(), self.home.display().to_string(), "--key-file".into(), self.key.display().to_string()];
        argv.extend(args.iter().map(|a| (*a).to_owned()));
        argv
    }

    pub fn cmd(&self, args: &[&str]) -> Command {
        let argv = self.argv(args);
        let mut c = Command::new(&argv[0]);
        c.args(&argv[1..]).env_remove("EIDOLON_HOME").env_remove("EIDOLON_KEY_FILE");
        c
    }

    pub fn run(&self, args: &[&str]) -> Run {
        self.cmd(args).stdin(Stdio::null()).output().unwrap().into()
    }

    pub fn run_with(&self, args: &[&str], stdin: &[u8]) -> Run {
        use std::io::Write;
        let mut child = self.cmd(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        let mut input = child.stdin.take().unwrap();
        let bytes = stdin.to_vec();
        let writer = std::thread::spawn(move || {
            let _ = input.write_all(&bytes);
        });
        let out = child.wait_with_output().unwrap();
        writer.join().unwrap();
        out.into()
    }

    pub fn key_line(&self) -> String {
        self.run(&["key", "show"]).ok().json()["key"].as_str().unwrap().to_owned()
    }
}

/// desk and tower, listing each other, with a `notes` target at `<root>/<name>-notes`.
pub fn pair(root: &Path) -> (Machine, Machine) {
    let (desk, tower) = (Machine::new(root, "desk"), Machine::new(root, "tower"));
    let rules = root.join("notes.toml");
    std::fs::write(&rules, "include = [\"**\"]\nexclude = [\"cache\"]\n").unwrap();
    let keys = [desk.key_line(), tower.key_line()];
    for m in [&desk, &tower] {
        for (name, key) in [("desk", &keys[0]), ("tower", &keys[1])] {
            m.run(&["holders", "add", name, key]).ok();
        }
    }
    for (m, name) in [(&desk, "desk"), (&tower, "tower")] {
        let path = root.join(format!("{name}-notes"));
        m.run(&["target", "add", "notes", "--file", rules.to_str().unwrap(), "--path", path.to_str().unwrap()]).ok();
    }
    (desk, tower)
}

pub fn write_tree(root: &Path, files: &[(&str, &str)]) {
    for (rel, body) in files {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
}
