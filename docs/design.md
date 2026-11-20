# Eidolon design

Eidolon seals a target, moves it between the machines on a holder list, and opens it again. This page states what it guarantees and how each guarantee is built. The [README](../README.md) covers using it.

## Built from standard parts

Every format is one that other tools read, and Eidolon composes them:

- The bundle is a tar, compressed with zstd, encrypted with age (`age 0.12`, `zstd`, `tar` crates). `age -d -i key | zstd -d | tar -x` opens it.
- Each machine has one Ed25519 OpenSSH key (`ssh-key`). It is an age `ssh-ed25519` recipient for bundles and signs seats and acks as SSHSIG, which `ssh-keygen -Y verify` checks.
- The holder list is an OpenSSH `allowed_signers` file, with each line limited to Eidolon's two namespaces.
- Targets are TOML, globs are `globset`, the key store is `keyring`, and the app lock is `fcntl` through `rustix` on Unix and an unshared open from the standard library on Windows.

The library is synchronous Rust with `unsafe` denied. It knows nothing of VMs or cells: a tar stream goes in (`seal --from-tar -`) and comes out (`open --tar`), and a move is bytes on any duplex.

## A home

```text
holders                          allowed_signers of every holder
recipients                       extra age recipients, one per line (optional)
targets/<name>/target.toml       the rules, the same on every machine
targets/<name>/path              where the target is on this machine
targets/<name>/seats/<gen>.json  each seat, with <gen>.json.sig beside it
targets/<name>/bundles/<sha>.age the bundle the newest seat names
targets/<name>/pending/          a move that waits for its ack
targets/<name>/lock              held by the one command changing the target
receive.lock                     held by the one receive running
```

Files are written whole under a temporary name and renamed into place, owner-only. A seat's signature is written before its record, so a record on disk always has its signature. Reading writes nothing: `ls`, `key show`, `holders ls`, `target show` and `verify` make no folder and no key, and the home appears with the first command that writes. Every command that changes a target takes an exclusive lock on `targets/<name>/lock` (`flock` on Unix, `LockFileEx` on Windows, through the standard library), and `receive` takes `receive.lock`. A second command is refused with `busy`; a command that dies releases its lock.

## One holder

The seat says which machine holds a target. Each seat is a record of the target, a generation, the holder, the previous holder, the SHA-256 of the bundle, a time and 16 random bytes, stored as the exact JSON bytes that were signed. The random bytes make every seat's bytes unique, so an ack can name one seat by their hash. A machine may seal or open a target only when the newest seat names it and no move is pending. A seat that arrived from another machine must be opened at the target's path before the folder there is sealed, or the seal is refused with `not_opened`: the folder would not yet hold what arrived. No generation follows the largest one a u64 holds; that seat is refused rather than wrapped.

`rules::accept_seat` decides whether one seat may follow another:

- The first is generation 1, has no previous holder, and is signed by its own holder.
- Every later one is the next generation, signed by the holder of the seat before it, names a machine on the list, and names that holder as previous.

A signature alone would let two machines claim the same generation; the generation and the previous holder make each seat follow exactly one other. Seals and moves both add a generation: a seal names the same holder with a new bundle, a move names the receiver with the same bundle.

A machine keeps every seat, so the history is a chain checked link by link from generation 1 (`rules::accept_chain`). A receiver checks an offered chain from the newest seat it already knows, which must appear in the offer byte for byte. A replayed offer falls short of that seat and is refused as `stale_generation`. Another history that reaches it is refused as `second_seat`. A seat signed by a listed machine that did not hold the one before is refused as `seat_not_signed_by_holder`. The proptests in `rules.rs` cover every generation but the next, every key off the list, and every signer but the holder.

## Capture

A target's rules keep a path when an include glob matches it or a folder above it, and no exclude glob does. The app's lock file is never kept, because closing any descriptor to an fcntl-locked file releases the lock. Capture writes regular files, folders and symlinks in name order, with permission bits and modification times and no owners. It leaves out sockets, devices and FIFOs. A file is opened without following links (`O_NOFOLLOW`; a reparse point is opened as itself on Windows) and read from that descriptor, and the seal is refused with `changed` if it is not a regular file once open, or if its size changes while it is read. A tar given on standard input passes the same rules and the same limits as a restore; its setuid, setgid and sticky bits are dropped.

Every captured entry passes the restore checks below as it is written, so a seal refuses with `unsafe_entry` what an open would refuse, such as a link out of the folder or two paths that differ only in case. The new bundle is then decrypted and read through the same checks once more before its seat is signed. Only after that is the previous bundle removed, so a failed seal leaves the last good bundle and seat as they were.

When the target names an app lock, Eidolon takes it as the app does before reading: a non-blocking fcntl write lock on `.parentlock` for Firefox on Unix, an open of `parent.lock` without sharing on Windows. If the app holds it, the seal is refused with `app_running`. While Eidolon holds it, the app cannot start on the folder. With Firefox closed, its SQLite files are consistent; a `-wal` left by a crash is captured with its database.

## Restore

A bundle is opened only after its SHA-256 matches the seat. Each entry is checked before anything is written:

- Its path must be relative, without `..`, a backslash or a name that is not UTF-8. On Windows, device names such as `CON` or `LPT1` and names ending in a dot or a space are refused too.
- It must not repeat a path, ignoring case, or lie under a symlink or a file.
- A symlink must resolve inside the folder, read without touching the disk, and must not pass through another link on the way. No link may be placed where an earlier entry or an earlier link's path goes through. Two links that each resolve inside on their own therefore cannot chain out.
- Hard links, devices, FIFOs, sparse files and setuid, setgid or sticky bits are refused.
- The total size and the entry count are capped (16 GiB and a million entries by default), and so is every byte read from the decompressed stream, so a compression bomb stops at the cap.
- The stream is read a block ahead of the tar crate: a GNU long name, long link or PAX header over 64 KiB is refused before the tar crate reads it into memory.
- A modification time past the year 9999 is set to that year.

Entries go into a fresh folder beside the destination, made 0700. Each folder above a file is checked to be a real folder, and files are created with `O_EXCL`. Only when every entry has passed is the destination renamed aside, the new folder renamed in, and the old one removed. A run killed between the renames leaves the old folder aside, and the next run puts it back or finishes. The app's lock is held through the swap; on Windows it is released just before, because a folder with an open file cannot be renamed there. `open --tar` runs the same checks and writes the entries out as a clean tar instead.

## A move

1. The sender signs the next seat, naming the receiver, and writes it as pending. From then on it neither seals nor opens.
2. It writes the offer: a tar of the target's rules, the holder list, every seat including the pending one, and the bundle.
3. The receiver checks that the holder lists are the same, so a sender cannot add a machine on the way, that the rules match any it already has, the seat chain, that the newest seat names it, and the bundle's hash. Then it decrypts the whole bundle through the restore checks without keeping anything.
4. It keeps the bundle, then the seats, and answers with an ack: a statement of the target, generation, bundle, receiver and the SHA-256 of the offered seat's bytes, with `taken` true or a refusal, signed in the `eidolon-ack@willctl.com` namespace.
5. The sender checks that the ack is signed by the receiver the pending seat names and names that exact seat by its hash, so an ack from an earlier try never settles a later one. A taken ack adds the seat to its chain, which retires its hold, and drops the bundle. With `--clear` it then removes its live copy, under the app's lock. A signed refusal proves the receiver kept nothing, so the pending move is dropped and the seat stays.

The receiver signs a refusal only for a check that failed before it wrote anything, and only when its own history does not already reach the offered generation. A receiver whose history already holds the offered seat, even with later seats after it, answers with a taken ack for that seat, so a sender whose ack was lost settles even after the receiver opened or sealed again. A receiver with a later history that lacks the offered seat answers `ahead`, unsigned. An unsigned answer, a broken stream or an ack for another seat settles nothing, and the sender keeps the move pending.

A move cut before step 4 leaves the receiver without the seat. One cut after it leaves the receiver holding and the sender pending. Running the same `send` again re-sends the same offer, and the receiver acks it again. `settle --confirm` takes an ack carried by hand. `settle --abandon` drops the pending move and is right only when the receiver surely took nothing. A receive killed partway leaves only a partial file in `incoming/`, which the next receive removes.

## Keys

`eidolon key init` makes the key; no other command does, and one that needs it is refused with `no_key`. The key is kept in the system's key store by default, under the service `eidolon` and the user `machine-key`, as an unencrypted OpenSSH private key. `--key-file` points at an OpenSSH key file instead: `key init` writes it 0600 and syncs it and its folder, and every command refuses it if group or others can read it. The key's text is wiped from memory when Eidolon drops it (`zeroize`). Extra age recipients (`recipients`, `-r`) can open bundles for recovery but are never holders: only an Ed25519 key on the list can sign a seat.

## Tests

- `rules.rs`, `seat.rs`, `target.rs` and `key.rs` test the rules, the seat bytes, the Firefox preset and the key file.
- `tests/restore.rs` refuses a malicious tar: `..`, absolute paths, links out, chained links, paths through a link, devices, FIFOs, hard links, setuid and setgid, twins, oversized extension headers and bombs.
- `tests/lock.rs` holds the lock in a second process, as an app does.
- `tests/moving.rs` moves over an in-process duplex and cuts it at each step, replays old offers and acks, re-sends after the receiver moved on, fails the receiver after it started writing, and holds a target's lock against a second command.
- `eidolon-cli/tests/kill.rs` kills the binary mid-seal, mid-open, mid-send and mid-receive.
- `eidolon-cli/tests/live.rs` moves a real Firefox profile between two homes, and opens a bundle with stock `age`, `zstd` and `tar` and checks a seat with `ssh-keygen` in a Lima VM.
