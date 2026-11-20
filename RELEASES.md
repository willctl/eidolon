# Eidolon release notes

## 0.3.0 (unreleased)

- Seal a folder target into an `age(zstd(tar))` bundle with `eidolon seal`, encrypted to every holder and any extra age recipients. Stock `age`, `zstd` and `tar` open it.
- `firefox` is a built-in target; any other folder is a TOML file of include and exclude globs and an optional app lock.
- Move a target with `eidolon send <target> --to tower -- ssh tower eidolon receive`, or as an offer file and an ack file. One machine holds it at a time, and the seats are SSHSIG that `ssh-keygen -Y verify` checks.
- Open a target with `eidolon open` into a fresh folder that is checked entry by entry and swapped into place, or as a tar with `--tar`.
- The machine key lives in the system's key store, or in an OpenSSH key file with `--key-file`.

[The README](README.md) lists the limits.
