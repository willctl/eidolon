//! The command as a person or a script runs it.
#![allow(clippy::unwrap_used, reason = "in a test, a failure should say where")]

mod common;

use common::{pair, write_tree, Machine, BIN};
use std::process::Command;

#[test]
fn a_target_moves_over_a_spawned_receiver_and_opens_there() {
    let dir = tempfile::tempdir().unwrap();
    let (desk, tower) = pair(dir.path());
    write_tree(&dir.path().join("desk-notes"), &[("a.md", "a"), ("cache/x", "skip")]);
    let sealed = desk.run(&["seal", "notes"]).ok().json();
    assert_eq!((sealed["generation"].as_u64(), sealed["counts"]["files"].as_u64()), (Some(1), Some(1)));

    let receiver = tower.argv(&["receive"]);
    let mut args = vec!["send", "notes", "--to", "tower", "--"];
    args.extend(receiver.iter().map(String::as_str));
    let moved = desk.run(&args).ok().json();
    assert_eq!((moved["to"].as_str(), moved["generation"].as_u64()), (Some("tower"), Some(2)));

    assert_eq!(desk.run(&["open", "notes"]).refused(), "not_the_holder");
    tower.run(&["open", "notes"]).ok();
    assert_eq!(std::fs::read_to_string(dir.path().join("tower-notes/a.md")).unwrap(), "a");
    assert!(!dir.path().join("tower-notes/cache").exists());
    let ls = tower.run(&["ls"]).ok().json();
    assert_eq!(ls["targets"][0]["held_here"], true);
    assert_eq!(tower.run(&["verify", "notes"]).ok().json()["seats_checked"], 2);
}

#[test]
fn a_move_by_hand_is_an_offer_file_and_an_ack_file() {
    let dir = tempfile::tempdir().unwrap();
    let (desk, tower) = pair(dir.path());
    write_tree(&dir.path().join("desk-notes"), &[("a.md", "a")]);
    desk.run(&["seal", "notes"]).ok();
    let offer = dir.path().join("offer");
    desk.run(&["send", "notes", "--to", "tower", "--out", offer.to_str().unwrap()]).ok();
    assert_eq!(desk.run(&["open", "notes"]).refused(), "move_pending");
    let ack = tower.run_with(&["receive"], &std::fs::read(&offer).unwrap()).ok();
    let ack_file = dir.path().join("ack");
    std::fs::write(&ack_file, &ack.stdout).unwrap();
    let settled = desk.run(&["settle", "notes", "--confirm", ack_file.to_str().unwrap(), "--clear"]).ok().json();
    assert_eq!(settled["moved"]["cleared"], true);
    assert!(!dir.path().join("desk-notes").exists());
    assert_eq!(desk.run(&["settle", "notes", "--abandon"]).refused(), "no_pending");
}

#[test]
fn a_tar_goes_in_on_stdin_and_comes_out_on_stdout() {
    let dir = tempfile::tempdir().unwrap();
    let (desk, _) = pair(dir.path());
    let mut input = tar::Builder::new(Vec::new());
    let mut h = tar::Header::new_gnu();
    h.set_size(2);
    h.set_mode(0o600);
    input.append_data(&mut h, "from-a-cell.txt", &b"hi"[..]).unwrap();
    let input = input.into_inner().unwrap();
    desk.run_with(&["seal", "notes", "--from-tar", "-"], &input).ok();
    let out = desk.run(&["open", "notes", "--tar"]).ok();
    let mut archive = tar::Archive::new(&out.stdout[..]);
    let names: Vec<String> = archive.entries().unwrap().map(|e| e.unwrap().path().unwrap().display().to_string()).collect();
    assert_eq!(names, vec!["from-a-cell.txt"]);
    assert!(out.stderr.contains("\"generation\":1"), "the summary goes to stderr: {}", out.stderr);
}

#[test]
fn the_holder_file_is_what_ssh_keygen_reads_and_seats_verify_with_it() {
    let dir = tempfile::tempdir().unwrap();
    let (desk, _) = pair(dir.path());
    write_tree(&dir.path().join("desk-notes"), &[("a.md", "a")]);
    desk.run(&["seal", "notes"]).ok();
    let seat = desk.home.join("targets/notes/seats/00000000000000000001.json");
    let status = Command::new("ssh-keygen")
        .args(["-Y", "verify", "-f"])
        .arg(desk.home.join("holders"))
        .args(["-I", "desk", "-n", eidolon::SEAT_NAMESPACE, "-s"])
        .arg(seat.with_extension("json.sig"))
        .stdin(std::fs::File::open(&seat).unwrap())
        .output();
    let Ok(status) = status else { return };
    assert!(status.status.success(), "{}", String::from_utf8_lossy(&status.stderr));
}

#[test]
fn refusals_have_stable_codes_and_usage_mistakes_exit_2() {
    let dir = tempfile::tempdir().unwrap();
    let m = Machine::new(dir.path(), "desk");
    assert_eq!(m.run(&["open", "nothing"]).refused(), "unknown_target");
    assert_eq!(m.run(&["target", "add", "Bad/Name"]).refused(), "bad_name");
    assert_eq!(m.run(&["holders", "add", "x", "ssh-rsa", "AAAA"]).refused(), "bad_holder");
    m.run(&["target", "add", "firefox", "--path", dir.path().join("p").to_str().unwrap()]).ok();
    assert_eq!(m.run(&["seal", "firefox"]).refused(), "not_a_holder");
    assert_eq!(m.run(&["send", "firefox", "--to", "tower"]).refused(), "no_path");
    let usage = Command::new(BIN).args(["open"]).output().unwrap();
    assert_eq!(usage.status.code(), Some(2));
    let show = m.run(&["target", "show", "firefox"]).ok().json();
    assert!(show["toml"].as_str().unwrap().contains("parentlock"));
}

#[cfg(unix)]
#[test]
fn a_key_file_others_can_read_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let m = Machine::new(dir.path(), "desk");
    m.key_line();
    std::fs::set_permissions(&m.key, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(m.run(&["key", "show"]).refused(), "key_not_private");
}
