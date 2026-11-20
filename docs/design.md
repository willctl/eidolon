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
```

Files are written whole under a temporary name and renamed into place, owner-only. A seat's signature is written before its record, so a record on disk always has its signature.

## One holder

The seat says which machine holds a target. Each seat is a record of the target, a generation, the holder, the previous holder, the SHA-256 of the bundle and a time, stored as the exact JSON bytes that were signed. A machine may seal or open a target only when the newest seat names it and no move is pending. A seat that arrived from another machine must be opened at the target's path before the folder there is sealed, or the seal is refused with `not_opened`: the folder would not yet hold what arrived.

`rules::accept_seat` decides whether one seat may follow another:

- The first is generation 1, has no previous holder, and is signed by its own holder.
- Every later one is the next generation, signed by the holder of the seat before it, names a machine on the list, and names that holder as previous.

A signature alone would let two machines claim the same generation; the generation and the previous holder make each seat follow exactly one other. Seals and moves both add a generation: a seal names the same holder with a new bundle, a move names the receiver with the same bundle.

A machine keeps every seat, so the history is a chain checked link by link from generation 1 (`rules::accept_chain`). A receiver checks an offered chain from the newest seat it already knows, which must appear in the offer byte for byte. A replayed offer falls short of that seat and is refused as `stale_generation`. Another history that reaches it is refused as `second_seat`. A seat signed by a listed machine that did not hold the one before is refused as `seat_not_signed_by_holder`. The proptests in `rules.rs` cover every generation but the next, every key off the list, and every signer but the holder.

## Capture

A target's rules keep a path when an include glob matches it or a folder above it, and no exclude glob does. The app's lock file is never kept, because closing any descriptor to an fcntl-locked file releases the lock. Capture writes regular files, folders and symlinks in name order, with permission bits and modification times and no owners. It leaves out sockets, devices and FIFOs, and fails if a file changes size while it is read. A tar given on standard input passes the same rules; its paths must be relative, and its setuid, setgid and sticky bits are dropped.

When the target names an app lock, Eidolon takes it as the app does before reading: a non-blocking fcntl write lock on `.parentlock` for Firefox on Unix, an open of `parent.lock` without sharing on Windows. If the app holds it, the seal is refused with `app_running`. While Eidolon holds it, the app cannot start on the folder. With Firefox closed, its SQLite files are consistent; a `-wal` left by a crash is captured with its database.

## Restore

A bundle is opened only after its SHA-256 matches the seat. Each entry is checked before anything is written:

- Its path must be relative, without `..`, a backslash or a name that is not UTF-8.
- It must not repeat a path, ignoring case, or lie under a symlink or a file.
- A symlink must resolve inside the folder, read without touching the disk.
- Hard links, devices, FIFOs and setuid, setgid or sticky bits are refused.
- The total size and the entry count are capped (16 GiB and a million entries by default), and so is every byte read from the decompressed stream, PAX headers included, so a compression bomb stops at the cap.

Entries go into a fresh folder beside the destination, made 0700. Each folder above a file is checked to be a real folder, and files are created with `O_EXCL`. Only when every entry has passed is the destination renamed aside, the new folder renamed in, and the old one removed. A run killed between the renames leaves the old folder aside, and the next run puts it back or finishes. The app's lock is held through the swap; on Windows it is released just before, because a folder with an open file cannot be renamed there. `open --tar` runs the same checks and writes the entries out as a clean tar instead.

## A move

1. The sender signs the next seat, naming the receiver, and writes it as pending. From then on it neither seals nor opens.
2. It writes the offer: a tar of the target's rules, the holder list, every seat including the pending one, and the bundle.
3. The receiver checks that the holder lists are the same, so a sender cannot add a machine on the way, that the rules match any it already has, the seat chain, that the newest seat names it, and the bundle's hash. Then it decrypts the whole bundle through the restore checks without keeping anything.
4. It keeps the bundle, then the seats, and answers with an ack: a statement of the target, generation, bundle and receiver, with `taken` true or a refusal, signed in the `eidolon-ack@willctl.com` namespace.
5. The sender checks that the ack is signed by the receiver the pending seat names and matches that seat. A taken ack adds the seat to its chain, which retires its hold, and drops the bundle. With `--clear` it then removes its live copy, under the app's lock. A signed refusal proves the receiver kept nothing, so the pending move is dropped and the seat stays.

A move cut before step 4 leaves the receiver without the seat. One cut after it leaves the receiver holding and the sender pending. Running the same `send` again re-sends the same offer, and a receiver that already holds exactly that seat acks again. `settle --confirm` takes an ack carried by hand. `settle --abandon` drops the pending move and is right only when the receiver surely took nothing. An unsigned answer, a broken stream or an ack for another move settles nothing. A receive killed partway leaves only a partial file in `incoming/`, which the next receive removes.

## Keys

The key is kept in the system's key store by default, under the service `eidolon` and the user `machine-key`, as an unencrypted OpenSSH private key. `--key-file` points at an OpenSSH key file instead, made 0600 if it is missing and refused if group or others can read it. Extra age recipients (`recipients`, `-r`) can open bundles for recovery but are never holders: only an Ed25519 key on the list can sign a seat.

## Tests

- `rules.rs`, `seat.rs`, `target.rs` and `key.rs` test the rules, the seat bytes, the Firefox preset and the key file.
- `tests/restore.rs` refuses a malicious tar: `..`, absolute paths, links out, paths through a link, devices, FIFOs, hard links, setuid and setgid, twins and bombs.
- `tests/lock.rs` holds the lock in a second process, as an app does.
- `tests/moving.rs` moves over an in-process duplex and cuts it at each step.
- `eidolon-cli/tests/kill.rs` kills the binary mid-seal, mid-open, mid-send and mid-receive.
- `eidolon-cli/tests/live.rs` moves a real Firefox profile between two homes, and opens a bundle with stock `age`, `zstd` and `tar` and checks a seat with `ssh-keygen` in a Lima VM.
