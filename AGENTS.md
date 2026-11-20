# Eidolon: notes for agents

- Two crates: `crates/eidolon` (the library) and `crates/eidolon-cli` (the `eidolon` command). [The design](docs/design.md) states the invariants; [CONTRIBUTING.md](CONTRIBUTING.md) lists the checks.
- Eidolon depends on no other willctl project. It composes standard formats: age, zstd, tar, OpenSSH keys, SSHSIG and allowed_signers. Prefer a standard crate or format over new code.
- The seat rules are in `crates/eidolon/src/rules.rs` with their proptests; restore safety is in `archive.rs`; the folder swap is in `swap.rs`.
- Live tests (`EIDOLON_LIVE=1`) use a throwaway Firefox profile in a temporary folder and one Lima VM named `adytum-test-*` in a `LIMA_HOME` under `/tmp`. They touch no other profile or VM.
- A refusal gets its test first. Docs state the current state only.
