//! `kill -9` of the real binary in the middle of a seal, an open and a move.
//! After each, the next command sees the old state or the new one, never a
//! mix, and running it again finishes.
#![cfg(unix)]
#![allow(clippy::unwrap_used, reason = "in a test, a failure should say where")]

mod common;

use common::{pair, write_tree};
use std::io::Write;
use std::path::Path;
use std::process::{Child, Stdio};
use std::time::Duration;

fn kill(mut child: Child) {
    child.kill().unwrap();
    child.wait().unwrap();
}

fn tree(root: &Path) -> Vec<(String, u64)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_owned()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).unwrap() {
            let path = e.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push((path.strip_prefix(root).unwrap().display().to_string(), std::fs::metadata(&path).unwrap().len()));
            }
        }
    }
    out.sort();
    out
}

fn sidecars(dest: &Path) -> Vec<String> {
    let name = dest.file_name().unwrap().to_string_lossy().into_owned();
    std::fs::read_dir(dest.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(&name) && n.contains(".eidolon-"))
        .collect()
}

fn many_files(root: &Path) {
    for i in 0..400 {
        write_tree(root, &[(&format!("d{}/f{i}.txt", i % 20), &"x".repeat(i * 7))]);
    }
    let noise: Vec<u8> = (0..24u32 << 20).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect();
    std::fs::write(root.join("big.bin"), noise).unwrap();
}

#[test]
fn a_seal_killed_halfway_leaves_the_last_generation_whole() {
    let dir = tempfile::tempdir().unwrap();
    let (desk, _) = pair(dir.path());
    let notes = dir.path().join("desk-notes");
    write_tree(&notes, &[("a.md", "first")]);
    desk.run(&["seal", "notes"]).ok();

    let mut tar = tar::Builder::new(Vec::new());
    let mut h = tar::Header::new_gnu();
    h.set_size(1 << 20);
    h.set_mode(0o600);
    tar.append_data(&mut h, "half.bin", &vec![7u8; 1 << 20][..]).unwrap();
    let tar = tar.into_inner().unwrap();
    let mut child = desk.cmd(&["seal", "notes", "--from-tar", "-"]).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    child.stdin.as_mut().unwrap().write_all(&tar[..tar.len() / 2]).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    kill(child);

    let ls = desk.run(&["ls"]).ok().json();
    assert_eq!(ls["targets"][0]["generation"], 1);
    std::fs::write(notes.join("a.md"), "edited").unwrap();
    desk.run(&["open", "notes"]).ok();
    assert_eq!(std::fs::read_to_string(notes.join("a.md")).unwrap(), "first");
    desk.run(&["seal", "notes"]).ok();
    assert_eq!(std::fs::read_dir(desk.home.join("targets/notes/bundles")).unwrap().count(), 1, "the killed seal's partial bundle is gone");
}

#[test]
fn an_open_killed_at_any_moment_leaves_the_old_folder_or_the_new_one() {
    let dir = tempfile::tempdir().unwrap();
    let (desk, _) = pair(dir.path());
    let notes = dir.path().join("desk-notes");
    many_files(&notes);
    desk.run(&["seal", "notes"]).ok();
    let sealed = tree(&notes);
    let mut states = Vec::new();
    for delay in [0, 10, 30, 60, 100, 150, 250, 400, 700] {
        std::fs::write(notes.join("only-in-the-old-copy"), "old").unwrap();
        let old = tree(&notes);
        let child = desk.cmd(&["open", "notes"]).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
        std::thread::sleep(Duration::from_millis(delay));
        kill(child);
        let now = if notes.exists() { Some(tree(&notes)) } else { None };
        match &now {
            Some(t) if *t == old => states.push("old"),
            Some(t) if *t == sealed => states.push("new"),
            None => {
                states.push("aside");
                assert!(sidecars(&notes).iter().any(|s| s.ends_with(".eidolon-old")), "killed at {delay} ms between the renames, the old copy is aside");
            }
            Some(_) => panic!("killed at {delay} ms, the folder is a mix of old and new"),
        }
        desk.run(&["open", "notes"]).ok();
        assert_eq!(tree(&notes), sealed, "after {delay} ms, a second open finishes");
        assert_eq!(sidecars(&notes), Vec::<String>::new());
    }
    eprintln!("states the kills left: {states:?}");
}

#[test]
fn a_sender_killed_while_it_waits_for_the_ack_settles_by_sending_again_or_by_the_ack() {
    let dir = tempfile::tempdir().unwrap();
    let (desk, tower) = pair(dir.path());
    write_tree(&dir.path().join("desk-notes"), &[("a.md", "a")]);
    desk.run(&["seal", "notes"]).ok();

    let ack = dir.path().join("ack");
    let receiver = tower.argv(&["receive"]);
    let script = format!("\"$@\" > '{}'; sleep 30", ack.display());
    let mut args = vec!["send", "notes", "--to", "tower", "--", "/bin/sh", "-c", &script, "sh"];
    args.extend(receiver.iter().map(String::as_str));
    let child = desk.cmd(&args).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    for _ in 0..100 {
        if std::fs::metadata(&ack).is_ok_and(|m| m.len() > 0) {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(Duration::from_millis(100));
    kill(child);

    assert_eq!(tower.run(&["ls"]).ok().json()["targets"][0]["held_here"], true, "the receiver took it");
    assert_eq!(desk.run(&["open", "notes"]).refused(), "move_pending", "the sender cannot tell, so it stays closed");
    desk.run(&["settle", "notes", "--confirm", ack.to_str().unwrap()]).ok();
    assert_eq!(desk.run(&["ls"]).ok().json()["targets"][0]["holder"], "tower");
}

#[test]
fn a_receiver_killed_mid_offer_keeps_nothing_and_the_same_send_finishes() {
    let dir = tempfile::tempdir().unwrap();
    let (desk, tower) = pair(dir.path());
    many_files(&dir.path().join("desk-notes"));
    desk.run(&["seal", "notes"]).ok();
    let offer = dir.path().join("offer");
    desk.run(&["send", "notes", "--to", "tower", "--out", offer.to_str().unwrap()]).ok();
    let bytes = std::fs::read(&offer).unwrap();
    let mut child = tower.cmd(&["receive"]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    child.stdin.as_mut().unwrap().write_all(&bytes[..bytes.len() / 2]).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    kill(child);
    assert!(tower.run(&["ls"]).ok().json()["targets"].as_array().unwrap().iter().all(|t| t["generation"].is_null()));

    let receiver = tower.argv(&["receive"]);
    let mut args = vec!["send", "notes", "--to", "tower", "--"];
    args.extend(receiver.iter().map(String::as_str));
    desk.run(&args).ok();
    assert_eq!(tower.run(&["ls"]).ok().json()["targets"][0]["held_here"], true);
    assert_eq!(std::fs::read_dir(tower.home.join("incoming")).unwrap().count(), 0, "the killed receive's partial bundle is gone");
}
