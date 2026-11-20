//! Facets sealed from one folder cell and opened in another: a plain folder
//! and a Firefox profile, and what each asks of its cell.
#![cfg(unix)]
#![expect(clippy::unwrap_used, reason = "a failing test should say where; helpers here are not #[test] fns")]

use adytum::testing::{failed, ok, DriverCall, FakeDriver};
use eidolon::cell::Cell;
use eidolon::facet::firefox::{self, PROFILE_LEAVE_OUT};
use eidolon::facet::{leave_out, open_facets, seal_facets};
use eidolon::model::{EidolonId, FacetId, FacetKind, FacetSpec, RefusalCode};
use eidolon::pack::glob::matches;
use eidolon::testing::{folder_cells, running};
use eidolon::vault::{EidolonKey, Keys, Vault};
use eidolon::Error;
use std::collections::VecDeque;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use xenonet_id::codec::{b64, hex, random, sha256};
use xenonet_id::sign::{KeyHome, SeedSigner};

const PROFILE: &str = "/home/eidolon/profile";

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Scratch {
        let dir = std::env::temp_dir().join(format!("eidolon-facet-test-{}", hex(&random::<8>().unwrap())));
        fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn folder(path: &str, leave_out: &[&str]) -> FacetSpec {
    FacetSpec { id: FacetId::from_bytes([4; 16]), kind: FacetKind::Folder, name: "notes".into(), path: path.into(), leave_out: leave_out.iter().map(|s| (*s).to_owned()).collect() }
}

fn browser(leave_out: &[&str]) -> FacetSpec {
    FacetSpec {
        id: FacetId::from_bytes([6; 16]),
        kind: FacetKind::Browser,
        name: "browser".into(),
        path: PROFILE.into(),
        leave_out: leave_out.iter().map(|s| (*s).to_owned()).collect(),
    }
}

fn keys() -> Keys {
    Keys::new(&EidolonKey::from_bytes([3; 32])).unwrap()
}

fn signer() -> SeedSigner {
    SeedSigner::from_seed(&[8; 32], KeyHome::File).unwrap()
}

/// Folder cells under `root`, whose `pgrep` answers come from `pgrep` (1,
/// nothing left, once they run out), whose `systemctl` succeeds, and where
/// every command with `failing` among its words fails.
fn cells(root: &Path, pgrep: &[i32], failing: Option<&'static str>) -> Arc<FakeDriver> {
    let pgrep = Mutex::new(pgrep.iter().copied().collect::<VecDeque<_>>());
    Arc::new(folder_cells(root, move |exec| {
        if failing.is_some_and(|f| exec.argv.iter().any(|a| a == f)) {
            return Some(failed(1, "no"));
        }
        match exec.argv.first().map(String::as_str) {
            Some("pgrep") => Some(failed(pgrep.lock().unwrap().pop_front().unwrap_or(1), "")),
            Some("systemctl") => Some(ok("")),
            _ => None,
        }
    }))
}

fn cell(fake: &Arc<FakeDriver>, name: &str) -> Cell {
    running(fake, &name.parse().unwrap())
}

/// Where `path` in the folder cell `name` is on the host.
fn guest(fake: &FakeDriver, name: &str, path: &str) -> PathBuf {
    fake.folder(&name.parse().unwrap(), path).unwrap()
}

/// The commands run, each as one line, with `root` in front of those run as root.
fn commands(fake: &FakeDriver) -> Vec<String> {
    fake.calls()
        .into_iter()
        .filter_map(|c| match c {
            DriverCall::Exec(e) => Some(if e.root { format!("root {}", e.argv.join(" ")) } else { e.argv.join(" ") }),
            _ => None,
        })
        .collect()
}

fn snapshot(root: &Path) -> Vec<String> {
    fn go(dir: &Path, base: &Path, out: &mut Vec<String>) {
        let mut kids: Vec<_> = fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
        kids.sort();
        for p in kids {
            let rel = p.strip_prefix(base).unwrap().display().to_string();
            let meta = fs::symlink_metadata(&p).unwrap();
            let mode = meta.permissions().mode() & 0o777;
            if meta.file_type().is_symlink() {
                out.push(format!("{rel} -> {}", fs::read_link(&p).unwrap().display()));
            } else if meta.is_dir() {
                out.push(format!("{rel}/ {mode:o}"));
                go(&p, base, out);
            } else {
                out.push(format!("{rel} {mode:o} {}", hex(&sha256(&fs::read(&p).unwrap()))));
            }
        }
    }
    let mut out = Vec::new();
    go(root, root, &mut out);
    out
}

fn fill(root: &Path) {
    fs::create_dir_all(root.join("sub/deeper")).unwrap();
    fs::create_dir_all(root.join("cache")).unwrap();
    fs::write(root.join("a.txt"), b"alpha").unwrap();
    fs::write(root.join("sub/deeper/b.bin"), vec![7u8; 300_000]).unwrap();
    fs::write(root.join("cache/junk"), b"junk").unwrap();
    fs::write(root.join("x.lock"), b"lock").unwrap();
    fs::write(root.join("tool.sh"), b"#!/bin/sh").unwrap();
    fs::set_permissions(root.join("tool.sh"), fs::Permissions::from_mode(0o750)).unwrap();
    symlink("a.txt", root.join("link")).unwrap();
}

#[tokio::test]
async fn a_folder_sealed_in_one_cell_opens_identical_in_another() {
    let s = Scratch::new();
    let fake = cells(&s.path("cells"), &[], None);
    fill(&guest(&fake, "adytum-a", "/home/will/notes"));
    let vault = Vault::open(&s.path("vault")).unwrap();
    let k = keys();
    let facets = [folder("/home/will/notes", &["cache/**", "*.lock"])];
    let head = seal_facets(&facets, &cell(&fake, "adytum-a"), &k, &vault, &signer(), EidolonId::from_bytes([1; 16]), 4, &s.path("work")).await.unwrap();
    assert_eq!(head.head.generation, 4);
    assert_eq!(fs::read_dir(s.path("work")).unwrap().count(), 0, "staging is cleaned up");
    open_facets(&head, &facets, &cell(&fake, "adytum-b"), &k, &vault, &s.path("work")).await.unwrap();
    assert_eq!(fs::read_dir(s.path("work")).unwrap().count(), 0);
    let (a, b) = (snapshot(&guest(&fake, "adytum-a", "/home/will/notes")), snapshot(&guest(&fake, "adytum-b", "/home/will/notes")));
    let carried: Vec<_> = a.into_iter().filter(|l| !l.starts_with("cache") && !l.starts_with("x.lock")).collect();
    assert_eq!(carried, b);
    assert!(b.iter().any(|l| l.starts_with("tool.sh 750")));
}

#[tokio::test]
async fn a_seal_and_an_open_ask_the_cell_for_paths_only_never_a_key() {
    let s = Scratch::new();
    let fake = cells(&s.path("cells"), &[], None);
    fill(&guest(&fake, "adytum-a", "/home/notes"));
    fill(&guest(&fake, "adytum-a", PROFILE));
    let key = EidolonKey::generate().unwrap();
    let k = Keys::new(&key).unwrap();
    let vault = Vault::open(&s.path("vault")).unwrap();
    let facets = [folder("/home/notes", &[]), browser(&[])];
    let head = seal_facets(&facets, &cell(&fake, "adytum-a"), &k, &vault, &signer(), EidolonId::new_random().unwrap(), 1, &s.path("work")).await.unwrap();
    open_facets(&head, &facets, &cell(&fake, "adytum-b"), &k, &vault, &s.path("work")).await.unwrap();
    let calls = format!("{:?}", fake.calls());
    let secret = key.as_bytes();
    for needle in [hex(secret), b64(secret), String::from_utf8_lossy(secret).into_owned()] {
        assert!(!calls.contains(&needle), "the key reached the cell in {calls}");
    }
}

#[test]
fn a_leave_out_that_escapes_its_facet_is_refused() {
    for spec in [folder("/home/notes", &["../secrets/**"]), folder("/p", &["/etc/**"]), browser(&["../elsewhere/**"])] {
        assert!(matches!(spec.validate(), Err(Error::Refused(r)) if r.code == RefusalCode::LeaveOutEscapes));
    }
}

#[tokio::test]
async fn opening_a_manifest_without_the_facet_places_nothing() {
    let s = Scratch::new();
    let fake = cells(&s.path("cells"), &[], None);
    fill(&guest(&fake, "adytum-a", "/home/will/notes"));
    let vault = Vault::open(&s.path("vault")).unwrap();
    let k = keys();
    let head = seal_facets(&[folder("/home/will/notes", &[])], &cell(&fake, "adytum-a"), &k, &vault, &signer(), EidolonId::from_bytes([1; 16]), 1, &s.path("work")).await.unwrap();
    let other = FacetSpec { id: FacetId::from_bytes([5; 16]), name: "other".into(), ..folder("/home/will/other", &[]) };
    let err = open_facets(&head, &[folder("/home/will/notes", &[]), other], &cell(&fake, "adytum-b"), &k, &vault, &s.path("work")).await.unwrap_err();
    assert!(matches!(err, Error::Damaged { .. }), "{err}");
    assert!(!guest(&fake, "adytum-b", "/").exists(), "the cell saw nothing");
    assert_eq!(fs::read_dir(s.path("work")).unwrap().count(), 0);
}

#[test]
fn the_profiles_leave_out_list_drops_its_caches_and_keeps_the_identity() {
    let patterns = leave_out(&browser(&[]));
    let left_out = |path: &str| patterns.iter().any(|p| matches(p, path));
    for gone in [
        "cache2",
        "cache2/entries/ABCDEF",
        "startupCache/startupCache.8.little",
        "thumbnails/1.png",
        "crashes/events/e1",
        "minidumps/a.dmp",
        "datareporting/state.json",
        "saved-telemetry-pings/ping",
        "lock",
        ".parentlock",
        "extensions/.parentlock",
        "storage.sqlite.lock",
    ] {
        assert!(left_out(gone), "{gone} is rebuilt by the browser and must be left out");
    }
    for kept in [
        "cookies.sqlite",
        "cookies.sqlite-wal",
        "key4.db",
        "logins.json",
        "prefs.js",
        "user.js",
        "places.sqlite",
        "sessionstore-backups/recovery.baklz4",
        "storage/default/https+++x/ls/data.sqlite",
        "extensions/a.xpi",
    ] {
        assert!(!left_out(kept), "{kept} is the identity and must be kept");
    }
    assert_eq!(patterns.len(), PROFILE_LEAVE_OUT.len());
    let extra = leave_out(&browser(&["cache2/**", "tmp/**"]));
    assert_eq!(extra.iter().filter(|p| *p == "cache2/**").count(), 1);
    assert!(extra.contains(&"tmp/**".to_owned()));
}

#[tokio::test(start_paused = true)]
async fn quiet_stops_the_units_and_waits_until_no_process_is_left() {
    let s = Scratch::new();
    let fake = cells(&s.path("cells"), &[0, 0, 1], None);
    firefox::quiet(&cell(&fake, "adytum-a")).await.unwrap();
    let commands = commands(&fake);
    assert!(commands[0].starts_with("root systemctl stop eidolon-browser.service eidolon-view.service"), "{commands:?}");
    assert_eq!(commands.iter().filter(|c| *c == "pgrep -u eidolon").count(), 3);
}

#[tokio::test(start_paused = true)]
async fn quiet_gives_up_after_a_bounded_number_of_asks() {
    let s = Scratch::new();
    let fake = cells(&s.path("cells"), &[0; 200], None);
    assert!(firefox::quiet(&cell(&fake, "adytum-a")).await.is_err());
    let asks = commands(&fake).iter().filter(|c| c.starts_with("pgrep")).count();
    assert!((1..200).contains(&asks), "asked {asks} times");
}

#[tokio::test]
async fn quiet_fails_when_the_units_cannot_be_stopped() {
    let s = Scratch::new();
    let fake = cells(&s.path("cells"), &[], Some("systemctl"));
    assert!(firefox::quiet(&cell(&fake, "adytum-a")).await.is_err());
    assert!(!commands(&fake).iter().any(|c| c.starts_with("pgrep")), "it did not go on to ask");
}

fn fill_profile(root: &Path) {
    for dir in ["cache2/entries", "startupCache", "sessionstore-backups", "crashes", "storage/default/site"] {
        fs::create_dir_all(root.join(dir)).unwrap();
    }
    fs::write(root.join("cookies.sqlite"), b"cookie jar").unwrap();
    fs::write(root.join("key4.db"), b"keys").unwrap();
    fs::write(root.join("user.js"), b"user_pref(\"a\", 1);").unwrap();
    fs::write(root.join("cache2/entries/AB"), b"cache").unwrap();
    fs::write(root.join("startupCache/x"), b"cache").unwrap();
    fs::write(root.join("crashes/c"), b"crash").unwrap();
    fs::write(root.join("sessionstore-backups/recovery.baklz4"), b"tabs").unwrap();
    fs::write(root.join("storage/default/site/data"), b"local storage").unwrap();
    fs::write(root.join(".parentlock"), b"").unwrap();
    symlink("10.0.0.1:+123", root.join("lock")).unwrap();
}

/// What placing a non-empty profile runs in the cell, in order.
const PLACE: [&str; 4] = [
    "root rm -rf -- /home/eidolon/profile.incoming",
    "root rm -rf -- /home/eidolon/profile",
    "root mv -- /home/eidolon/profile.incoming /home/eidolon/profile",
    "root chmod -R u+rwX,go-rwx /home/eidolon/profile",
];

#[tokio::test]
async fn a_profile_is_sealed_without_its_caches_and_opened_with_its_cookies() {
    let s = Scratch::new();
    let fake = cells(&s.path("cells"), &[1], None);
    fill_profile(&guest(&fake, "adytum-a", PROFILE));
    let vault = Vault::open(&s.path("vault")).unwrap();
    let head = seal_facets(&[browser(&[])], &cell(&fake, "adytum-a"), &keys(), &vault, &signer(), EidolonId::from_bytes([1; 16]), 2, &s.path("work")).await.unwrap();
    let ran = commands(&fake);
    assert_eq!(ran.len(), 4, "{ran:?}");
    for (at, step) in ["systemctl stop", "pgrep", "a+rX", "u+rwX,go-rwx"].iter().enumerate() {
        assert!(ran[at].contains(step), "{ran:?}");
    }
    assert!(!ran.iter().any(|c| c.contains("systemctl start")), "a sealed browser stays stopped");

    let before = fake.calls().len();
    open_facets(&head, &[browser(&[])], &cell(&fake, "adytum-b"), &keys(), &vault, &s.path("work")).await.unwrap();
    let there = guest(&fake, "adytum-b", PROFILE);
    assert_eq!(fs::read(there.join("cookies.sqlite")).unwrap(), b"cookie jar");
    assert_eq!(fs::read(there.join("key4.db")).unwrap(), b"keys");
    assert_eq!(fs::read(there.join("storage/default/site/data")).unwrap(), b"local storage");
    assert_eq!(fs::read(there.join("sessionstore-backups/recovery.baklz4")).unwrap(), b"tabs");
    for gone in ["cache2", "startupCache", "crashes", ".parentlock", "lock"] {
        assert!(fs::symlink_metadata(there.join(gone)).is_err(), "{gone} should not have been carried");
    }
    let placed: Vec<String> = commands(&fake).into_iter().skip(ran.len()).collect();
    assert_eq!(placed, PLACE);
    let copied = fake.calls()[before..].iter().find_map(|c| if let DriverCall::CopyIn { guest, owner, .. } = c { Some((guest.clone(), owner.clone())) } else { None });
    assert_eq!(copied, Some(("/home/eidolon/profile.incoming".to_owned(), Some("eidolon".parse().unwrap()))), "the browser's user writes its profile");
}

#[tokio::test]
async fn placing_a_profile_replaces_the_one_in_the_cell_whole() {
    let s = Scratch::new();
    let fake = cells(&s.path("cells"), &[], None);
    fill_profile(&s.path("staged"));
    let old = guest(&fake, "adytum-a", PROFILE);
    fs::create_dir_all(&old).unwrap();
    fs::write(old.join("cookies.sqlite-wal"), b"journal").unwrap();
    fs::create_dir_all(guest(&fake, "adytum-a", "/home/eidolon/profile.incoming")).unwrap();
    fs::write(guest(&fake, "adytum-a", "/home/eidolon/profile.incoming/half-copied"), b"x").unwrap();
    firefox::place(&cell(&fake, "adytum-a"), PROFILE, &s.path("staged")).await.unwrap();
    assert_eq!(commands(&fake), PLACE);
    assert!(!old.join("cookies.sqlite-wal").exists(), "the browser's journal is gone");
    assert!(!old.join("half-copied").exists(), "a half-placed profile is removed first");
    assert_eq!(fs::read(old.join("cookies.sqlite")).unwrap(), b"cookie jar");
    assert!(!guest(&fake, "adytum-a", "/home/eidolon/profile.incoming").exists());
}

#[tokio::test]
async fn an_empty_staged_profile_places_nothing() {
    let s = Scratch::new();
    let fake = cells(&s.path("cells"), &[], None);
    fs::create_dir_all(s.path("staged")).unwrap();
    firefox::place(&cell(&fake, "adytum-a"), PROFILE, &s.path("staged")).await.unwrap();
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
}

#[tokio::test]
async fn a_failed_swap_stops_before_the_old_profile_is_touched() {
    let s = Scratch::new();
    let fake = cells(&s.path("cells"), &[], Some("/home/eidolon/profile.incoming"));
    fill_profile(&s.path("staged"));
    assert!(firefox::place(&cell(&fake, "adytum-a"), PROFILE, &s.path("staged")).await.is_err());
    assert!(!commands(&fake).iter().any(|c| c == "root rm -rf -- /home/eidolon/profile"));
}

#[tokio::test]
async fn a_failed_copy_still_closes_the_profile_again() {
    let s = Scratch::new();
    let fake = cells(&s.path("cells"), &[], None);
    assert!(firefox::take(&cell(&fake, "adytum-a"), PROFILE, &s.path("staging")).await.is_err());
    assert!(commands(&fake).last().unwrap().contains("go-rwx"));
}
