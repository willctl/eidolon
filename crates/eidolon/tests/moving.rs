//! Moves over an in-process duplex, cut or killed at each step. Whatever
//! breaks, exactly one machine holds the seat, and the same send or a
//! settle finishes it.
#![allow(clippy::unwrap_used, reason = "in a test, a failure should say where")]

mod common;

use common::{home, list, read_tree, write_tree, World};
use eidolon::{Code, Dest, Home, Limits, SeatRecord, Settled, SignedSeat, Source};
use std::io::{pipe, Read, Write};

fn sealed() -> World {
    let w = World::new();
    write_tree(&w.path("desk-notes"), &[("a.md", "a"), ("deep/b.md", "b")]);
    w.desk.seal("notes", Source::Folder(None), Vec::new()).unwrap();
    w
}

/// Sends over two pipes with the receiver on its own thread, as `-- ssh tower eidolon receive` would.
fn send(from: &Home, to_name: &str, to: &Home, clear: bool) -> (eidolon::Result<eidolon::Moved>, eidolon::Result<eidolon::Received>) {
    let (offer_r, offer_w) = pipe().unwrap();
    let (ack_r, ack_w) = pipe().unwrap();
    std::thread::scope(|s| {
        let receiver = s.spawn(move || to.receive(offer_r, ack_w, Limits::default()));
        let sent = from.send("notes", to_name, clear, offer_w, ack_r);
        (sent, receiver.join().unwrap())
    })
}

fn offer(from: &Home) -> Vec<u8> {
    let mut bytes = Vec::new();
    from.write_offer("notes", &mut bytes).unwrap();
    bytes
}

fn code<T>(r: eidolon::Result<T>) -> Option<Code> {
    r.err().and_then(|e| e.code())
}

fn holds(m: &Home) -> bool {
    m.status("notes").unwrap().held_here
}

fn opens(m: &Home) -> Option<Code> {
    code(m.open_target("notes", Dest::Folder(None), Limits::default()))
}

#[test]
fn a_move_hands_the_seat_over_and_back() {
    let w = sealed();
    let (sent, received) = send(&w.desk, "tower", &w.tower, false);
    let (sent, received) = (sent.unwrap(), received.unwrap());
    assert_eq!((sent.generation, received.generation, received.from.as_str(), received.again), (2, 2, "desk", false));
    assert!(holds(&w.tower) && !holds(&w.desk));
    assert_eq!(opens(&w.tower), None);
    assert_eq!(read_tree(&w.path("tower-notes")), vec![("a.md".into(), "a".into()), ("deep/b.md".into(), "b".into())]);
    assert_eq!(opens(&w.desk), Some(Code::NotTheHolder));
    assert_eq!(code(w.desk.seal("notes", Source::Folder(None), Vec::new())), Some(Code::NotTheHolder));

    write_tree(&w.path("tower-notes"), &[("c.md", "made on tower")]);
    w.tower.seal("notes", Source::Folder(None), Vec::new()).unwrap();
    let (sent, received) = send(&w.tower, "desk", &w.desk, true);
    assert_eq!(sent.unwrap().generation, 4);
    assert!(received.unwrap().generation == 4 && holds(&w.desk));
    assert!(!w.path("tower-notes").exists(), "--clear removed tower's live copy");
    assert_eq!(opens(&w.desk), None);
    assert!(read_tree(&w.path("desk-notes")).contains(&("c.md".into(), "made on tower".into())));
    assert_eq!(w.tower.verify("notes", Limits::default()).unwrap().seats_checked, 4);
    assert_eq!(w.desk.verify("notes", Limits::default()).unwrap().opened.unwrap().files, 3);
}

#[test]
fn killed_after_pending_is_written_the_sender_stays_closed_and_the_same_send_finishes() {
    let w = sealed();
    w.desk.start_move("notes", "tower").unwrap();
    assert_eq!(opens(&w.desk), Some(Code::MovePending));
    assert_eq!(code(w.desk.seal("notes", Source::Folder(None), Vec::new())), Some(Code::MovePending));
    assert_eq!(code(w.desk.start_move("notes", "lab")), Some(Code::UnknownHolder));
    let (sent, _) = send(&w.desk, "tower", &w.tower, false);
    assert_eq!(sent.unwrap().generation, 2);
    assert!(holds(&w.tower));
}

#[test]
fn an_offer_cut_anywhere_leaves_the_receiver_without_the_seat() {
    let w = sealed();
    w.desk.start_move("notes", "tower").unwrap();
    let whole = offer(&w.desk);
    for cut in [0, 100, 512, 1500, 4096, whole.len() / 2, whole.len() - 1500] {
        let mut ack = Vec::new();
        assert!(w.tower.receive(&whole[..cut], &mut ack, Limits::default()).is_err(), "cut at {cut}");
        assert!(!holds(&w.tower) && w.tower.status("notes").unwrap().generation.is_none(), "cut at {cut}");
        assert_eq!(code(w.desk.settle_with_ack("notes", &ack, false)), Some(Code::BadAck), "an unsigned error keeps the move pending");
    }
    assert!(std::fs::read_dir(w.tower.root().join("incoming")).unwrap().next().is_none(), "no partial bundle is kept");
    let (sent, _) = send(&w.desk, "tower", &w.tower, false);
    assert!(sent.is_ok() && holds(&w.tower));
}

#[test]
fn an_ack_lost_on_the_way_back_is_settled_by_sending_again_or_by_the_saved_ack() {
    let w = sealed();
    w.desk.start_move("notes", "tower").unwrap();
    let mut lost = Vec::new();
    w.tower.receive(&offer(&w.desk)[..], &mut lost, Limits::default()).unwrap();
    assert!(holds(&w.tower));
    assert_eq!(opens(&w.desk), Some(Code::MovePending), "the sender cannot tell, so it stays closed");
    let (sent, received) = send(&w.desk, "tower", &w.tower, false);
    assert!(received.unwrap().again, "the receiver acks the seat it already took");
    assert_eq!(sent.unwrap().generation, 2);

    let w = sealed();
    w.desk.start_move("notes", "tower").unwrap();
    let mut saved = Vec::new();
    w.tower.receive(&offer(&w.desk)[..], &mut saved, Limits::default()).unwrap();
    assert_eq!(code(w.desk.settle_with_ack("notes", &saved[..saved.len() / 2], false)), Some(Code::BadAck));
    assert!(matches!(w.desk.settle_with_ack("notes", &saved, false).unwrap(), Settled::Moved(_)));
    assert_eq!(code(w.desk.settle_with_ack("notes", &saved, false)), Some(Code::NoPending));
}

#[test]
fn an_ack_not_signed_by_the_receiver_or_for_another_move_settles_nothing() {
    let w = sealed();
    w.desk.start_move("notes", "tower").unwrap();
    let mut ack = Vec::new();
    w.tower.receive(&offer(&w.desk)[..], &mut ack, Limits::default()).unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(&ack).unwrap();
    let statement = value["statement"].as_str().unwrap().to_owned();
    value["signature"] = w.desk.key().sign(eidolon::ACK_NAMESPACE, statement.as_bytes()).unwrap().into();
    assert_eq!(code(w.desk.settle_with_ack("notes", &serde_json::to_vec(&value).unwrap(), false)), Some(Code::BadAck), "signed by the sender itself");
    let other = statement.replace("\"generation\":2", "\"generation\":3");
    value["statement"] = other.clone().into();
    value["signature"] = w.tower.key().sign(eidolon::ACK_NAMESPACE, other.as_bytes()).unwrap().into();
    assert_eq!(code(w.desk.settle_with_ack("notes", &serde_json::to_vec(&value).unwrap(), false)), Some(Code::BadAck), "for another generation");
    assert_eq!(opens(&w.desk), Some(Code::MovePending));
}

#[test]
fn a_signed_refusal_clears_the_pending_move_and_the_seat_stays() {
    let w = sealed();
    let lab = home(w.dir.path(), "lab");
    w.tower.add_holder("lab", lab.key().public().clone()).unwrap();
    let (sent, received) = send(&w.desk, "tower", &w.tower, false);
    assert_eq!(code(sent), Some(Code::HoldersDiffer));
    assert_eq!(code(received), Some(Code::HoldersDiffer));
    assert!(holds(&w.desk) && w.desk.status("notes").unwrap().pending_to.is_none());
    assert_eq!(opens(&w.desk), None);
}

#[test]
fn abandon_drops_the_pending_move() {
    let w = sealed();
    w.desk.start_move("notes", "tower").unwrap();
    assert!(matches!(w.desk.abandon("notes").unwrap(), Settled::Abandoned));
    assert_eq!(opens(&w.desk), None);
    assert_eq!(code(w.desk.abandon("notes")), Some(Code::NoPending));
}

#[test]
fn a_stale_replayed_or_forged_offer_is_refused() {
    let w = sealed();
    let lab = home(w.dir.path(), "lab");
    for m in [&w.desk, &w.tower] {
        m.add_holder("lab", lab.key().public().clone()).unwrap();
    }
    list(&[("desk", &w.desk), ("tower", &w.tower), ("lab", &lab)], &lab);
    lab.add_target("notes", Some(eidolon::Target::parse(common::NOTES).unwrap()), Some(w.path("lab-notes"))).unwrap();

    w.desk.start_move("notes", "tower").unwrap();
    let first = offer(&w.desk);
    send(&w.desk, "tower", &w.tower, false).0.unwrap();
    assert_eq!(opens(&w.tower), None);
    w.tower.seal("notes", Source::Folder(None), Vec::new()).unwrap();
    send(&w.tower, "desk", &w.desk, false).0.unwrap();
    let mut ack = Vec::new();
    assert_eq!(code(w.tower.receive(&first[..], &mut ack, Limits::default())), Some(Code::StaleGeneration), "a replay of an old offer");

    write_tree(&w.path("lab-notes"), &[("x", "lab's own")]);
    lab.seal("notes", Source::Folder(None), Vec::new()).unwrap();
    lab.start_move("notes", "tower").unwrap();
    let mut ack = Vec::new();
    assert_eq!(code(w.tower.receive(&offer(&lab)[..], &mut ack, Limits::default())), Some(Code::StaleGeneration), "another, shorter history from generation 1");

    let mut forged = Vec::new();
    w.desk.start_move("notes", "tower").unwrap();
    let honest = offer(&w.desk);
    let mut archive = tar::Archive::new(&honest[..]);
    let mut out = tar::Builder::new(&mut forged);
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        let path = entry.path().unwrap().display().to_string();
        let mut body = Vec::new();
        entry.read_to_end(&mut body).unwrap();
        if path == "seats/5.json.sig" {
            let record: SeatRecord = serde_json::from_slice(&std::fs::read(w.desk.root().join("targets/notes/pending/next.json")).unwrap()).unwrap();
            body = SignedSeat::sign(record, lab.key()).unwrap().signature().as_bytes().to_vec();
        }
        let mut h = tar::Header::new_gnu();
        h.set_size(body.len() as u64);
        h.set_mode(0o600);
        out.append_data(&mut h, path, &body[..]).unwrap();
    }
    out.into_inner().unwrap().flush().unwrap();
    assert_eq!(code(w.tower.receive(&forged[..], &mut ack, Limits::default())), Some(Code::SeatNotSignedByHolder), "a seat signed by a listed machine that does not hold it");
    assert!(!holds(&w.tower) && w.desk.status("notes").unwrap().pending_to.as_deref() == Some("tower"));
}

#[test]
fn a_bundle_with_an_unsafe_entry_is_refused_before_the_receiver_keeps_anything() {
    let w = sealed();
    let mut tar = Vec::new();
    let mut h = tar::Header::new_gnu();
    h.as_old_mut().name[..9].copy_from_slice(b"../escape");
    h.set_size(1);
    h.set_mode(0o644);
    h.set_cksum();
    tar.extend_from_slice(h.as_bytes());
    tar.extend_from_slice(&[b'x'; 512]);
    tar.extend_from_slice(&[0; 1024]);
    let mut bad = Vec::new();
    let recipients = [w.tower.key().public().recipient().unwrap()];
    let encryptor = age::Encryptor::with_recipients(recipients.iter().map(|r| r as &dyn age::Recipient)).unwrap();
    let mut writer = encryptor.wrap_output(&mut bad).unwrap();
    writer.write_all(&zstd::encode_all(&tar[..], 3).unwrap()).unwrap();
    writer.finish().unwrap();
    let sha = eidolon::hex(&<sha2::Sha256 as sha2::Digest>::digest(&bad));

    let next = SeatRecord { target: "notes".into(), generation: 2, holder: w.tower.key().public().clone(), previous: Some(w.desk.key().public().clone()), bundle: sha, at: 0 };
    let seats = [
        std::fs::read(w.desk.root().join("targets/notes/seats/00000000000000000001.json")).unwrap(),
        std::fs::read(w.desk.root().join("targets/notes/seats/00000000000000000001.json.sig")).unwrap(),
    ];
    let next = SignedSeat::sign(next, w.desk.key()).unwrap();
    let mut offer = tar::Builder::new(Vec::new());
    let mut put = |path: &str, body: &[u8]| {
        let mut h = tar::Header::new_gnu();
        h.set_size(body.len() as u64);
        h.set_mode(0o600);
        offer.append_data(&mut h, path, body).unwrap();
    };
    put("target.toml", w.desk.target("notes").unwrap().to_toml().as_bytes());
    put("holders", w.desk.holders().unwrap().render().as_bytes());
    put("seats/1.json", &seats[0]);
    put("seats/1.json.sig", &seats[1]);
    put("seats/2.json", next.bytes());
    put("seats/2.json.sig", next.signature().as_bytes());
    put("bundle.age", &bad);
    let offer = offer.into_inner().unwrap();
    let mut ack = Vec::new();
    assert_eq!(code(w.tower.receive(&offer[..], &mut ack, Limits::default())), Some(Code::UnsafeEntry));
    assert!(!holds(&w.tower) && w.tower.status("notes").unwrap().generation.is_none());
}
