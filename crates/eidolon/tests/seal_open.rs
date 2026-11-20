//! Sealing a target and opening it again, on the holder only.

mod common;

use common::{no_leftovers, read_tree, write_tree, World};
use eidolon::{Code, Dest, Limits, Recipient, Source};
use std::fs;
use std::io::Read;

#[test]
fn a_sealed_folder_opens_whole_without_what_its_rules_leave_out() {
    let w = World::new();
    write_tree(&w.path("desk-notes"), &[("a.md", "a"), ("deep/b.md", "b"), ("cache/x", "cached")]);
    let sealed = w.desk.seal("notes", Source::Folder(None), Vec::new()).unwrap();
    assert_eq!((sealed.generation, sealed.counts.files), (1, 2));

    let out = w.path("restored");
    let opened = w.desk.open_target("notes", Dest::Folder(Some(out.clone())), Limits::default()).unwrap();
    assert_eq!(opened.counts.files, 2);
    assert_eq!(read_tree(&out), vec![("a.md".into(), "a".into()), ("deep/b.md".into(), "b".into())]);

    fs::write(w.path("desk-notes/a.md"), "changed").unwrap();
    w.desk.open_target("notes", Dest::Folder(None), Limits::default()).unwrap();
    assert_eq!(fs::read_to_string(w.path("desk-notes/a.md")).unwrap(), "a", "open puts the sealed state back");
    assert!(!w.path("desk-notes/cache").exists(), "a left-out folder does not come back");
    assert!(no_leftovers(&w.path("desk-notes")));

    let again = w.desk.seal("notes", Source::Folder(None), Vec::new()).unwrap();
    assert_eq!(again.generation, 2, "each seal is the next generation");
    assert_eq!(fs::read_dir(w.desk.root().join("targets/notes/bundles")).unwrap().count(), 1, "only the newest bundle is kept");
    assert_eq!(w.desk.verify("notes", Limits::default()).unwrap().seats_checked, 2);
}

#[test]
fn a_bundle_is_age_zstd_tar_and_opens_with_any_holders_key_or_an_extra_recipient() {
    let w = World::new();
    write_tree(&w.path("desk-notes"), &[("a.md", "a")]);
    let paper = age::x25519::Identity::generate();
    let sealed = w.desk.seal("notes", Source::Folder(None), vec![Recipient::parse(&paper.to_public().to_string()).unwrap()]).unwrap();
    let bundle = w.desk.root().join(format!("targets/notes/bundles/{}.age", sealed.bundle));
    assert_eq!(eidolon::sha256_of(&bundle).unwrap(), sealed.bundle);
    let tower_key = age::ssh::Identity::from_buffer(w.tower.key().to_openssh().as_bytes(), None).unwrap();
    for identity in [&tower_key as &dyn age::Identity, &paper] {
        let decryptor = age::Decryptor::new(fs::File::open(&bundle).unwrap()).unwrap();
        let plain = decryptor.decrypt(std::iter::once(identity)).unwrap();
        let mut tar = tar::Archive::new(zstd::Decoder::new(plain).unwrap());
        let mut names = Vec::new();
        for entry in tar.entries().unwrap() {
            let mut entry = entry.unwrap();
            let mut body = String::new();
            entry.read_to_string(&mut body).unwrap();
            names.push((entry.path().unwrap().display().to_string(), body, entry.header().uid().unwrap()));
        }
        assert_eq!(names, vec![("a.md".to_owned(), "a".to_owned(), 0)]);
    }
}

#[test]
fn only_the_holder_seals_or_opens() {
    let w = World::new();
    write_tree(&w.path("desk-notes"), &[("a.md", "a")]);
    w.desk.seal("notes", Source::Folder(None), Vec::new()).unwrap();
    let stranger = common::home(w.dir.path(), "stranger");
    stranger.add_target("notes", Some(eidolon::Target::parse(common::NOTES).unwrap()), Some(w.path("desk-notes"))).unwrap();
    assert_eq!(stranger.seal("notes", Source::Folder(None), Vec::new()).err().and_then(|e| e.code()), Some(Code::NotAHolder));
    assert_eq!(w.desk.open_target("unknown", Dest::Folder(None), Limits::default()).err().and_then(|e| e.code()), Some(Code::UnknownTarget));
    assert_eq!(w.tower.open_target("notes", Dest::Folder(None), Limits::default()).err().and_then(|e| e.code()), Some(Code::NotSealed));
}

#[test]
fn a_changed_bundle_does_not_open() {
    let w = World::new();
    write_tree(&w.path("desk-notes"), &[("a.md", "a")]);
    let sealed = w.desk.seal("notes", Source::Folder(None), Vec::new()).unwrap();
    let bundle = w.desk.root().join(format!("targets/notes/bundles/{}.age", sealed.bundle));
    let mut bytes = fs::read(&bundle).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    fs::write(&bundle, bytes).unwrap();
    assert_eq!(w.desk.open_target("notes", Dest::Folder(None), Limits::default()).err().and_then(|e| e.code()), Some(Code::BundleChanged));
    assert_eq!(read_tree(&w.path("desk-notes")).iter().filter(|(p, _)| p == "a.md").count(), 1);
}

#[test]
fn a_tar_from_elsewhere_is_filtered_and_cleaned_and_open_can_write_one() {
    let w = World::new();
    let mut input = tar::Builder::new(Vec::new());
    for (path, mode) in [("./a.md", 0o4755), ("cache/x", 0o644), ("b.md", 0o640)] {
        let mut h = tar::Header::new_gnu();
        h.set_size(1);
        h.set_mode(mode);
        h.set_uid(501);
        h.set_cksum();
        input.append_data(&mut h, path, &b"x"[..]).unwrap();
    }
    let input = input.into_inner().unwrap();
    let sealed = w.desk.seal("notes", Source::Tar(&mut &input[..]), Vec::new()).unwrap();
    assert_eq!(sealed.counts.files, 2);
    let mut out = Vec::new();
    w.desk.open_target("notes", Dest::Tar(&mut out), Limits::default()).unwrap();
    let mut tar = tar::Archive::new(&out[..]);
    let seen: Vec<_> = tar.entries().unwrap().map(|e| {
        let e = e.unwrap();
        (e.path().unwrap().display().to_string(), e.header().mode().unwrap(), e.header().uid().unwrap())
    }).collect();
    assert_eq!(seen, vec![("a.md".to_owned(), 0o755, 0), ("b.md".to_owned(), 0o640, 0)], "no setuid bit, no owner, nothing left out");
}

#[test]
fn a_target_add_with_other_rules_is_refused_and_a_path_alone_is_set() {
    let w = World::new();
    let other = eidolon::Target::parse("include = [\"x\"]").unwrap();
    assert_eq!(w.desk.add_target("notes", Some(other), None).err().and_then(|e| e.code()), Some(Code::TargetDiffers));
    let status = w.desk.add_target("notes", None, Some(w.path("elsewhere"))).unwrap();
    assert_eq!(status.path, Some(w.path("elsewhere")));
    assert!(w.desk.add_target("firefox", None, None).unwrap().about.contains("Firefox"));
    assert_eq!(w.desk.add_target("custom", None, None).err().and_then(|e| e.code()), Some(Code::UnknownTarget));
}
