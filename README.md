# Eidolon

Eidolon seals a folder, such as a Firefox profile, into one encrypted bundle, moves it between your machines so that exactly one of them holds it at a time, and opens it again without letting the bundle write outside its folder.

It is a thin layer over standard formats. A bundle is `age(zstd(tar))`, encrypted to each machine's Ed25519 OpenSSH key, so `age`, `zstd` and `tar` open it without Eidolon. The record of who holds a folder is a chain of small JSON files signed as SSHSIG, which `ssh-keygen -Y verify` checks. Eidolon is a Rust library, `eidolon`, and a command, `eidolon`, built from `eidolon-cli`. It depends on no other willctl project.

## Install

```text
cargo install --locked --git https://github.com/willctl/eidolon eidolon-cli
```

## Move a Firefox profile from desk to tower

Each machine makes its key once and lists every machine, itself too, as a holder. Both lists must be the same.

```text
desk$  eidolon key init
ssh-ed25519 AAAAC3...desk
tower$ eidolon key init
ssh-ed25519 AAAAC3...tower

desk$  eidolon holders add desk ssh-ed25519 AAAAC3...desk
desk$  eidolon holders add tower ssh-ed25519 AAAAC3...tower
tower$ eidolon holders add desk ssh-ed25519 AAAAC3...desk
tower$ eidolon holders add tower ssh-ed25519 AAAAC3...tower
```

Each machine names where the profile lives on it, since profile paths differ by system:

```text
desk$  eidolon target add firefox --path "$HOME/Library/Application Support/Firefox/Profiles/abcd1234.work"
tower$ eidolon target add firefox --path "%APPDATA%\Mozilla\Firefox\Profiles\efgh5678.work"
```

Quit Firefox, then seal, move and open:

```text
desk$  eidolon seal firefox
desk$  eidolon send firefox --to tower --clear -- ssh tower eidolon receive
tower$ eidolon open firefox
```

`seal` refuses with `app_running` while Firefox has the profile open, and holds Firefox's own lock while it reads, so Firefox cannot start halfway through. It checks every entry as `open` would, and keeps the last good bundle until the new one has opened once. `send` runs the command after `--` as an argument list, never through a shell, writes the offer to its standard input and reads the ack from its standard output. `--clear` removes desk's copy once tower has acked. After the move, `eidolon open firefox` on desk is refused with `not_the_holder`.

## Recipes

Move by hand, with no connection between the machines:

```text
desk$  eidolon send firefox --to tower --out offer
tower$ eidolon receive < offer > ack
desk$  eidolon settle firefox --confirm ack
```

Seal a folder inside a VM or an Adytum cell from its tar, and put it back the same way. `--from-tar -` reads a tar on standard input and keeps what the target's rules keep; `--tar` writes the checked entries as a tar on standard output:

```text
adytum exec adytum-web -- tar -C /home/cell/profile -c . | eidolon seal web --from-tar -
eidolon open web --tar | adytum exec adytum-web -- tar -x -C /home/cell/profile
```

Open a bundle with stock tools, on any machine that has a holder's key:

```text
age -d -i ~/.ssh/eidolon_key ~/.eidolon/targets/firefox/bundles/<sha256>.age | zstd -d | tar -x -C restored
```

Check a seat with OpenSSH. The holder list is an `allowed_signers` file. Each seat is signed by the holder before it, or by its own holder at generation 1:

```text
ssh-keygen -Y verify -f ~/.eidolon/holders -I desk -n eidolon-seat@willctl.com \
  -s ~/.eidolon/targets/firefox/seats/00000000000000000002.json.sig \
  < ~/.eidolon/targets/firefox/seats/00000000000000000002.json
```

Keep a recovery key that can open every bundle but never holds the seat. Any age recipient works: a paper `age1...` key, or a YubiKey or Secure Enclave key through its age plugin. Put one per line in `~/.eidolon/recipients`, or pass `-r` to one seal:

```text
eidolon seal firefox -r age1yubikey1q...
```

## Targets

A target is a folder with rules. `firefox` is built in; `eidolon target show firefox` prints it. Its rules leave out what Firefox rebuilds or downloads again (caches, safe-browsing lists, crash reports, telemetry, codec plugins) and keep sign-ins, cookies, history, settings and add-ons. Any other folder is a TOML file of the same shape:

```toml
about = "Notes and their attachments"
include = ["**"]
exclude = ["cache", "*.tmp", "**/.DS_Store"]

[closed]
app = "Notes"
unix_lock = ".lock"
windows_lock = "lock"
```

```text
eidolon target add notes --file notes.toml --path ~/notes
```

Globs are relative to the folder; `*` stays within one folder and `**` crosses them. A path is kept when an `include` glob matches it or a folder above it, and no `exclude` glob does. `[closed]` names the lock file the app holds while it runs: an fcntl write lock on Unix, a file kept open without sharing on Windows. Eidolon takes the same lock before it reads or replaces the folder, and never captures the lock file. `eidolon target add work --preset firefox --path ...` adds a second profile under another name.

## Commands

| Command | What it does |
| --- | --- |
| `key init`, `key show` | Makes this machine's key; prints its public key, or says there is none yet. |
| `holders add <name> <key>`, `holders ls` | Lists the machines that may hold a target. |
| `target add <name> [--preset P \| --file F] [--path DIR]`, `target ls`, `target show <name>` | Adds a target (a path alone takes the whole folder), or sets its path on this machine. |
| `seal <target> [--from DIR \| --from-tar -] [-r RECIPIENT] [--max-size 16G]` | Captures the folder into a new bundle and signs the next seat. |
| `open <target> [--to DIR \| --tar] [--max-size 16G]` | Restores the newest bundle into a fresh folder and swaps it into place. |
| `send <target> --to <holder> [--clear] (-- COMMAND \| --out FILE)` | Moves the seat to another holder. |
| `receive [--max-size 16G]` | Reads an offer on standard input and writes the ack on standard output. |
| `settle <target> (--confirm ACK \| --abandon) [--clear]` | Finishes a move that was cut off. |
| `ls`, `verify <target>` | Shows every target and its holder; checks the seats and the bundle. |

Every command takes `--json` and then prints one JSON object; a failure is `{"error": {"code", "message", "fix"}}`. The codes are stable, such as `app_running`, `not_the_holder`, `move_pending`, `unsafe_entry`, `no_key` or `busy`. Reading commands (`ls`, `key show`, `holders ls`, `target show`, `verify`) write nothing; on a machine with no key, `ls --json` prints `"key": null`. Each command that changes a target holds a lock on it, and a second one at the same time is refused with `busy`. Exit 0 is done, 1 is a refusal or a failure, and 2 is a mistake in how the command was typed. With `open --tar` and `receive`, standard output carries data, so the summary goes to standard error.

The home is `~/.eidolon`, or `--home` / `EIDOLON_HOME`. The key lives in the system's key store (the macOS keychain, Windows Credential Manager, or the Secret Service on Linux) under `eidolon` / `machine-key`. On a machine without one, such as a server reached over ssh, pass `--key-file` or set `EIDOLON_KEY_FILE` to an OpenSSH key that only its owner can read. `eidolon key init` makes it if it is missing, and every command refuses it if others can read it.

## Limits

- Moves send the whole bundle every time. zstd keeps a Firefox profile small, but nothing is deduplicated between moves.
- `--abandon` trusts you: if the receiver did take the seat, both machines then believe they hold it. Run the same `send` again instead whenever the receiver can be reached; it acks a seat it already took.
- A listed machine is trusted to follow the rules. One that keeps a copy of a bundle can still decrypt it, since every bundle is encrypted to every holder.
- A site that binds a session to one device, a fingerprint or a network may ask you to sign in again after a move. Firefox does not open a profile that a newer Firefox wrote.
- On Windows, symlinks in a bundle are refused, and Firefox's lock is released just before the folders are swapped, because Windows cannot rename a folder with an open file in it.
- Restore keeps permission bits and modification times, not owners, extended attributes or ACLs.

[The design](docs/design.md) says what Eidolon guarantees and why.

## Tests

```text
cargo test --workspace --all-features
EIDOLON_LIVE=1 cargo test -p eidolon-cli --test live -- --test-threads=1 --nocapture
```

The live tests run a headless Firefox on a throwaway profile in a temporary folder, and make one Lima VM named `adytum-test-*` in a `LIMA_HOME` of their own under `/tmp`, which they delete at the end.
