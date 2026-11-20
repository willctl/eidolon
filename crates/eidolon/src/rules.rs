//! What may hold a seat, and which seat may follow which.

use crate::error::{Code, Refusal};
use crate::holders::Holders;
use crate::key::HolderKey;
use crate::seat::SignedSeat;

pub fn may_hold(holders: &Holders, machine: &HolderKey) -> Result<(), Refusal> {
    if holders.by_key(machine).is_some() {
        Ok(())
    } else {
        Err(Refusal::new(Code::NotAHolder, "That machine is not on the holder list.", "Add it with `eidolon holders add` on every machine first."))
    }
}

/// Whether `next` may follow `current` as the seat of `target`. The first
/// seat (`current` is none) is generation 1, has no previous holder and is
/// signed by its own holder. Every later one is the next generation, signed
/// by the holder of the seat it follows, naming a machine on the list, with
/// that holder as its `previous`.
pub fn accept_seat(holders: &Holders, target: &str, current: Option<&SignedSeat>, next: &SignedSeat) -> Result<(), Refusal> {
    let n = &next.record;
    if n.target != target || current.is_some_and(|c| c.record.target != n.target) {
        return Err(Refusal::new(Code::StaleGeneration, format!("That seat is for {:?}, not {target:?}.", n.target), "Ask the sender for this target's seat."));
    }
    let want = current.map_or(1, |c| c.record.generation.saturating_add(1));
    if n.generation != want || (current.is_some() && want == 1) {
        return Err(Refusal::new(
            Code::StaleGeneration,
            format!("That seat is generation {} and this one needs {want}.", n.generation),
            "Ask the sender to start the move again from the current seat.",
        ));
    }
    let signer = current.map_or(&n.holder, |c| &c.record.holder);
    if !next.signed_by(signer) {
        return Err(Refusal::new(Code::SeatNotSignedByHolder, "That seat was not signed by the machine that holds the one before it.", "Move the target from the machine that holds it."));
    }
    may_hold(holders, &n.holder)?;
    if n.previous.as_ref() != current.map(|c| &c.record.holder) {
        return Err(Refusal::new(Code::StaleGeneration, "That seat does not name the current holder as the previous one.", "Ask the sender to start the move again from the current seat."));
    }
    Ok(())
}

/// Checks a history of seats link by link. With `known`, the history must
/// contain that exact seat and is checked from there; without, it starts at
/// generation 1.
pub fn accept_chain(holders: &Holders, target: &str, known: Option<&SignedSeat>, chain: &[SignedSeat]) -> Result<(), Refusal> {
    let start = match known {
        None => {
            let first = chain.first().ok_or_else(|| Refusal::new(Code::StaleGeneration, "There is no seat.", "Seal the target first."))?;
            accept_seat(holders, target, None, first)?;
            0
        }
        Some(known) => match chain.iter().position(|s| s.record.generation == known.record.generation) {
            Some(i) if chain[i] == *known => i,
            Some(_) => {
                return Err(Refusal::new(
                    Code::SecondSeat,
                    format!("The offered history differs from this machine's at generation {}.", known.record.generation),
                    "Two machines claim the seat; decide which one holds the target and settle the other.",
                ))
            }
            None => {
                return Err(Refusal::new(
                    Code::StaleGeneration,
                    format!("The offered history does not reach generation {}, which this machine has.", known.record.generation),
                    "Move from the machine that holds the newest seat.",
                ))
            }
        },
    };
    for pair in chain[start..].windows(2) {
        accept_seat(holders, target, Some(&pair[0]), &pair[1])?;
    }
    Ok(())
}

/// A rule's glob is written relative to the target, inside it.
pub fn pattern_is_inside(pattern: &str) -> Result<(), Refusal> {
    let escapes = pattern.is_empty() || pattern.starts_with(['/', '\\']) || pattern.split(['/', '\\']).any(|part| part == "..") || pattern.contains('\0') || pattern.contains(':');
    if escapes {
        Err(Refusal::new(Code::BadTarget, format!("The pattern {pattern:?} reaches outside the target."), "Write it relative to the target's folder, without `..`."))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::holders::Holder;
    use crate::key::MachineKey;
    use crate::seat::SeatRecord;
    use proptest::prelude::*;

    fn machine(n: u8) -> MachineKey {
        MachineKey::from_seed([n; 32])
    }

    fn seat(target: &str, generation: u64, holder: &MachineKey, previous: Option<&MachineKey>, signed_by: &MachineKey) -> SignedSeat {
        let record = SeatRecord { target: target.into(), generation, holder: holder.public().clone(), previous: previous.map(|p| p.public().clone()), bundle: "ab".repeat(32), at: 1 };
        SignedSeat::sign(record, signed_by).unwrap()
    }

    fn list(machines: &[&MachineKey]) -> Holders {
        let mut holders = Holders::default();
        for (i, m) in machines.iter().enumerate() {
            holders.add(Holder { name: format!("m{i}"), key: m.public().clone() }).unwrap();
        }
        holders
    }

    fn code(r: Result<(), Refusal>) -> Option<Code> {
        r.err().map(|e| e.code)
    }

    const T: &str = "firefox";

    #[test]
    fn a_machine_off_the_list_may_not_hold() {
        let (desk, tower, stranger) = (machine(1), machine(2), machine(3));
        let holders = list(&[&desk, &tower]);
        assert_eq!(code(may_hold(&holders, stranger.public())), Some(Code::NotAHolder));
        assert!(may_hold(&holders, tower.public()).is_ok());
    }

    #[test]
    fn a_seat_that_skips_or_repeats_a_generation_is_refused() {
        let (desk, tower) = (machine(1), machine(2));
        let holders = list(&[&desk, &tower]);
        let cur = seat(T, 3, &desk, None, &desk);
        for g in [3, 5, 1] {
            let next = seat(T, g, &tower, Some(&desk), &desk);
            assert_eq!(code(accept_seat(&holders, T, Some(&cur), &next)), Some(Code::StaleGeneration), "generation {g}");
        }
        let second_first = seat(T, 2, &desk, None, &desk);
        assert_eq!(code(accept_seat(&holders, T, None, &second_first)), Some(Code::StaleGeneration), "a first seat is generation 1");
    }

    #[test]
    fn a_seat_for_another_target_is_refused() {
        let desk = machine(1);
        let holders = list(&[&desk]);
        let other = seat("notes", 1, &desk, None, &desk);
        assert_eq!(code(accept_seat(&holders, T, None, &other)), Some(Code::StaleGeneration));
    }

    #[test]
    fn a_seat_not_signed_by_the_current_holder_is_refused() {
        let (desk, tower, third) = (machine(1), machine(2), machine(3));
        let holders = list(&[&desk, &tower, &third]);
        let cur = seat(T, 1, &desk, None, &desk);
        for signer in [&tower, &third] {
            let next = seat(T, 2, &tower, Some(&desk), signer);
            assert_eq!(code(accept_seat(&holders, T, Some(&cur), &next)), Some(Code::SeatNotSignedByHolder));
        }
        let first = seat(T, 1, &desk, None, &tower);
        assert_eq!(code(accept_seat(&holders, T, None, &first)), Some(Code::SeatNotSignedByHolder));
    }

    #[test]
    fn a_seat_naming_a_machine_off_the_list_is_refused() {
        let (desk, stranger) = (machine(1), machine(3));
        let holders = list(&[&desk]);
        let cur = seat(T, 1, &desk, None, &desk);
        let next = seat(T, 2, &stranger, Some(&desk), &desk);
        assert_eq!(code(accept_seat(&holders, T, Some(&cur), &next)), Some(Code::NotAHolder));
    }

    #[test]
    fn a_seat_with_the_wrong_previous_holder_is_refused() {
        let (desk, tower, third) = (machine(1), machine(2), machine(3));
        let holders = list(&[&desk, &tower, &third]);
        let cur = seat(T, 1, &desk, None, &desk);
        for previous in [Some(&third), None] {
            let next = seat(T, 2, &tower, previous, &desk);
            assert_eq!(code(accept_seat(&holders, T, Some(&cur), &next)), Some(Code::StaleGeneration));
        }
    }

    #[test]
    fn the_first_seat_and_the_next_one_are_accepted() {
        let (desk, tower) = (machine(1), machine(2));
        let holders = list(&[&desk, &tower]);
        let first = seat(T, 1, &desk, None, &desk);
        assert!(accept_seat(&holders, T, None, &first).is_ok());
        let next = seat(T, 2, &tower, Some(&desk), &desk);
        assert!(accept_seat(&holders, T, Some(&first), &next).is_ok());
    }

    #[test]
    fn a_chain_is_checked_from_the_seat_this_machine_knows() {
        let (desk, tower, third) = (machine(1), machine(2), machine(3));
        let holders = list(&[&desk, &tower, &third]);
        let chain = [seat(T, 1, &desk, None, &desk), seat(T, 2, &tower, Some(&desk), &desk), seat(T, 3, &desk, Some(&tower), &tower)];
        assert!(accept_chain(&holders, T, None, &chain).is_ok());
        assert!(accept_chain(&holders, T, Some(&chain[1]), &chain).is_ok());
        assert_eq!(code(accept_chain(&holders, T, None, &chain[1..])), Some(Code::StaleGeneration), "a history must start at 1");
        let fork = seat(T, 2, &third, Some(&desk), &desk);
        assert_eq!(code(accept_chain(&holders, T, Some(&fork), &chain)), Some(Code::SecondSeat));
        let newer = seat(T, 4, &tower, Some(&desk), &desk);
        assert_eq!(code(accept_chain(&holders, T, Some(&newer), &chain)), Some(Code::StaleGeneration));
        let forged = [chain[0].clone(), chain[1].clone(), seat(T, 3, &third, Some(&tower), &third)];
        assert_eq!(code(accept_chain(&holders, T, None, &forged)), Some(Code::SeatNotSignedByHolder));
    }

    #[test]
    fn a_pattern_that_escapes_is_refused() {
        for p in ["/etc/passwd", "../x", "a/../../b", "a/..", "..", "\\windows", "a\\..\\b", "a\0b", "C:x", ""] {
            assert_eq!(code(pattern_is_inside(p)), Some(Code::BadTarget), "{p:?}");
        }
        for p in ["cache2", "cache2/**", "*.lock", "a/b..c", "..hidden/x", "Crash Reports/**"] {
            assert!(pattern_is_inside(p).is_ok(), "{p:?}");
        }
    }

    proptest! {
        #[test]
        fn any_generation_but_the_next_is_refused(cur_gen in 1u64..u64::MAX - 1, next_gen in any::<u64>()) {
            prop_assume!(next_gen != cur_gen + 1);
            let (desk, tower) = (machine(1), machine(2));
            let holders = list(&[&desk, &tower]);
            let cur = seat(T, cur_gen, &desk, None, &desk);
            let next = seat(T, next_gen, &tower, Some(&desk), &desk);
            prop_assert_eq!(code(accept_seat(&holders, T, Some(&cur), &next)), Some(Code::StaleGeneration));
        }

        #[test]
        fn any_machine_off_the_list_is_refused(seed in any::<[u8; 32]>()) {
            let (desk, tower) = (machine(1), machine(2));
            let holders = list(&[&desk, &tower]);
            let stranger = MachineKey::from_seed(seed);
            prop_assume!(stranger.public() != desk.public() && stranger.public() != tower.public());
            prop_assert_eq!(code(may_hold(&holders, stranger.public())), Some(Code::NotAHolder));
        }

        #[test]
        fn any_signer_but_the_current_holder_is_refused(seed in any::<[u8; 32]>()) {
            let (desk, tower) = (machine(1), machine(2));
            let signer = MachineKey::from_seed(seed);
            prop_assume!(signer.public() != desk.public());
            let holders = list(&[&desk, &tower]);
            let cur = seat(T, 1, &desk, None, &desk);
            let next = seat(T, 2, &tower, Some(&desk), &signer);
            prop_assert_eq!(code(accept_seat(&holders, T, Some(&cur), &next)), Some(Code::SeatNotSignedByHolder));
        }
    }
}
