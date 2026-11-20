//! A tar that tries to write outside the folder it fills, or to fill too
//! much, is refused before anything at the destination changes.
#![allow(clippy::unwrap_used, reason = "in a test, a failure should say where")]

mod common;

use common::{no_leftovers, read_tree, write_tree};
use eidolon::{unpack, Code, Limits};
use std::path::Path;
use tar::{EntryType, Header};

struct Raw(Vec<u8>);

impl Raw {
    fn new() -> Raw {
        Raw(Vec::new())
    }

    /// An entry written header by header, so nothing checks or fixes its path.
    fn add(mut self, name: &str, kind: EntryType, mode: u32, data: &[u8], link: Option<&str>) -> Raw {
        let mut h = Header::new_gnu();
        h.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
        if let Some(link) = link {
            h.as_old_mut().linkname[..link.len()].copy_from_slice(link.as_bytes());
        }
        h.set_entry_type(kind);
        h.set_mode(mode);
        h.set_size(data.len() as u64);
        h.set_mtime(1_700_000_000);
        h.set_cksum();
        self.0.extend_from_slice(h.as_bytes());
        self.0.extend_from_slice(data);
        self.0.resize(self.0.len().div_ceil(512) * 512, 0);
        self
    }

    fn file(self, name: &str, data: &str) -> Raw {
        self.add(name, EntryType::Regular, 0o644, data.as_bytes(), None)
    }

    fn link(self, name: &str, to: &str) -> Raw {
        self.add(name, EntryType::Symlink, 0o777, b"", Some(to))
    }

    fn done(mut self) -> Vec<u8> {
        self.0.extend_from_slice(&[0; 1024]);
        self.0
    }
}

fn refused(tar: &[u8], limits: Limits) -> Option<Code> {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("profile");
    write_tree(&dest, &[("keep.txt", "old")]);
    let result = unpack(tar, &dest, limits);
    assert_eq!(read_tree(&dest), vec![("keep.txt".into(), "old".into())], "the folder is untouched");
    assert!(no_leftovers(&dest));
    assert!(!dir.path().join("escape").exists() && !Path::new("/tmp/eidolon-escape").exists());
    result.err().and_then(|e| e.code())
}

fn small() -> Limits {
    Limits { max_bytes: 1 << 20, max_entries: 100 }
}

#[test]
fn paths_that_climb_out_or_are_absolute_are_refused() {
    for name in ["../escape", "a/../../escape", "/tmp/eidolon-escape", "a\\..\\escape"] {
        assert_eq!(refused(&Raw::new().file(name, "x").done(), Limits::default()), Some(Code::UnsafeEntry), "{name}");
    }
}

#[test]
fn links_that_point_out_or_are_written_through_are_refused() {
    let cases = [
        Raw::new().link("l", "../escape").done(),
        Raw::new().link("l", "/etc").done(),
        Raw::new().link("a/l", "../../escape").done(),
        Raw::new().link("l", "sub").file("l/x", "through a link").done(),
        Raw::new().link("L", "sub").file("l/x", "through a link, by case").done(),
        Raw::new().file("f", "a file").file("f/x", "under a file").done(),
        Raw::new().add("h", EntryType::Link, 0o644, b"", Some("keep.txt")).done(),
    ];
    for (i, tar) in cases.iter().enumerate() {
        assert_eq!(refused(tar, Limits::default()), Some(Code::UnsafeEntry), "case {i}");
    }
}

#[test]
fn devices_fifos_setuid_and_twins_are_refused() {
    let cases = [
        Raw::new().add("dev", EntryType::Char, 0o644, b"", None).done(),
        Raw::new().add("disk", EntryType::Block, 0o644, b"", None).done(),
        Raw::new().add("pipe", EntryType::Fifo, 0o644, b"", None).done(),
        Raw::new().add("suid", EntryType::Regular, 0o4755, b"x", None).done(),
        Raw::new().add("sgid", EntryType::Directory, 0o2755, b"", None).done(),
        Raw::new().file("twin", "a").file("twin", "b").done(),
        Raw::new().file("Twin", "a").file("twin", "b").done(),
    ];
    for (i, tar) in cases.iter().enumerate() {
        assert_eq!(refused(tar, Limits::default()), Some(Code::UnsafeEntry), "case {i}");
    }
}

#[test]
fn a_bomb_stops_at_the_limit() {
    let big = vec![0u8; 2 << 20];
    assert_eq!(refused(&Raw::new().add("big", EntryType::Regular, 0o644, &big, None).done(), small()), Some(Code::TooLarge), "one large file");
    let mut claim = Raw::new().done();
    let mut h = Header::new_gnu();
    h.set_path("claims-a-terabyte").unwrap();
    h.set_size(1 << 40);
    h.set_mode(0o644);
    h.set_cksum();
    claim.splice(0..0, h.as_bytes().iter().copied());
    assert_eq!(refused(&claim, small()), Some(Code::TooLarge), "a header that claims more than the limit");
    let mut many = Raw::new();
    for i in 0..101 {
        many = many.file(&format!("f{i}"), "");
    }
    assert_eq!(refused(&many.done(), small()), Some(Code::TooLarge), "too many entries");
    let pax = vec![b'x'; 65 << 10];
    let extension = |kind| refused(&Raw::new().add("ext", kind, 0o644, &pax, None).file("a", "x").done(), Limits::default());
    for kind in [EntryType::XHeader, EntryType::XGlobalHeader, EntryType::GNULongName, EntryType::GNULongLink] {
        assert_eq!(extension(kind), Some(Code::UnsafeEntry), "an extension header over 64 KiB is refused before it is read: {kind:?}");
    }
    let zeros = zstd::encode_all(&Raw::new().add("zeros", EntryType::Regular, 0o644, &vec![0u8; 8 << 20], None).done()[..], 19).unwrap();
    assert!(zeros.len() < 4096, "the bomb is small: {}", zeros.len());
    let dir = tempfile::tempdir().unwrap();
    let code = unpack(zstd::Decoder::new(&zeros[..]).unwrap(), &dir.path().join("p"), small()).err().and_then(|e| e.code());
    assert_eq!(code, Some(Code::TooLarge), "a compression bomb");
}

#[test]
fn a_clean_tar_replaces_the_folder_with_its_modes_times_and_inside_links() {
    let tar = Raw::new()
        .add("sub", EntryType::Directory, 0o750, b"", None)
        .file("sub/a.txt", "a")
        .add("secret", EntryType::Regular, 0o600, b"s", None)
        .link("to-a", "sub/a.txt")
        .link("sub/up", "../secret")
        .file("./deep/er/b.txt", "b")
        .done();
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("profile");
    write_tree(&dest, &[("old.txt", "old")]);
    let counts = unpack(&tar[..], &dest, Limits::default()).unwrap();
    assert_eq!((counts.files, counts.dirs, counts.links), (3, 1, 2));
    assert_eq!(
        read_tree(&dest),
        vec![
            ("deep/er/b.txt".into(), "b".into()),
            ("secret".into(), "s".into()),
            ("sub/a.txt".into(), "a".into()),
            ("sub/up".into(), "-> ../secret".into()),
            ("to-a".into(), "-> sub/a.txt".into()),
        ]
    );
    assert!(no_leftovers(&dest));
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let meta = std::fs::metadata(dest.join("secret")).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        assert_eq!(meta.mtime(), 1_700_000_000);
        assert_eq!(std::fs::metadata(dest.join("sub")).unwrap().permissions().mode() & 0o777, 0o750);
    }
}

#[test]
fn links_that_chain_through_other_links_are_refused() {
    let cases =
        [Raw::new().link("a/b/l", "../..").link("x", "a/b/l/..").done(), Raw::new().link("x", "a/..").link("a", ".").done(), Raw::new().file("d/f", "x").link("d", ".").done()];
    for (i, tar) in cases.iter().enumerate() {
        assert_eq!(refused(tar, Limits::default()), Some(Code::UnsafeEntry), "case {i}");
    }
}

#[test]
fn a_huge_modification_time_restores_without_a_panic() {
    let mut tar = Raw::new().file("f", "hi").done();
    let mut h = Header::from_byte_slice(&tar[..512]).clone();
    let mut mtime = [0xffu8; 12];
    mtime[0] = 0x80;
    h.as_old_mut().mtime = mtime;
    h.set_cksum();
    tar[..512].copy_from_slice(h.as_bytes());
    let dir = tempfile::tempdir().unwrap();
    let counts = unpack(&tar[..], &dir.path().join("p"), Limits::default()).unwrap();
    assert_eq!(counts.files, 1);
}
