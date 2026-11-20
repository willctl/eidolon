#![allow(dead_code, clippy::unwrap_used, reason = "test helpers, each test file uses part of them; a failure should say where")]

use eidolon::{Home, KeySource, Target};
use std::fs;
use std::path::{Path, PathBuf};

pub const NOTES: &str = "include = [\"**\"]\nexclude = [\"cache\"]\n\n[closed]\napp = \"Notes\"\nunix_lock = \".lock\"\nwindows_lock = \"lock\"\n";

pub struct World {
    pub dir: tempfile::TempDir,
    pub desk: Home,
    pub tower: Home,
}

pub fn home(root: &Path, name: &str) -> Home {
    Home::open(root.join(name), &KeySource::File(root.join(format!("{name}.key")))).unwrap()
}

pub fn list(machines: &[(&str, &Home)], on: &Home) {
    for (name, m) in machines {
        on.add_holder(name, m.key().public().clone()).unwrap();
    }
}

impl World {
    /// desk and tower, each listing both, each with the `notes` target at its own path.
    pub fn new() -> World {
        let dir = tempfile::tempdir().unwrap();
        let (desk, tower) = (home(dir.path(), "desk"), home(dir.path(), "tower"));
        for m in [&desk, &tower] {
            list(&[("desk", &desk), ("tower", &tower)], m);
        }
        let w = World { dir, desk, tower };
        w.desk.add_target("notes", Some(Target::parse(NOTES).unwrap()), Some(w.path("desk-notes"))).unwrap();
        w.tower.add_target("notes", Some(Target::parse(NOTES).unwrap()), Some(w.path("tower-notes"))).unwrap();
        w
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }
}

pub fn write_tree(root: &Path, files: &[(&str, &str)]) {
    for (rel, body) in files {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }
}

/// Every file under `root` as (relative path, contents), sorted.
pub fn read_tree(root: &Path) -> Vec<(String, String)> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let rel = path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            let meta = fs::symlink_metadata(&path).unwrap();
            if meta.file_type().is_symlink() {
                out.push((rel, format!("-> {}", fs::read_link(&path).unwrap().display())));
            } else if meta.is_dir() {
                walk(root, &path, out);
            } else {
                out.push((rel, fs::read_to_string(&path).unwrap_or_else(|_| "<binary>".into())));
            }
        }
    }
    let mut out = Vec::new();
    if root.exists() {
        walk(root, root, &mut out);
    }
    out.sort();
    out
}

/// Nothing but `name` itself beside it: no staging or old copy left over.
pub fn no_leftovers(dest: &Path) -> bool {
    let name = dest.file_name().unwrap().to_string_lossy().into_owned();
    fs::read_dir(dest.parent().unwrap()).unwrap().all(|e| {
        let n = e.unwrap().file_name().to_string_lossy().into_owned();
        !(n.contains(&name) && n.contains(".eidolon-"))
    })
}
