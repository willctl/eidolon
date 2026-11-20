# Eidolon: notes for agents

- Two crates: `crates/eidolon` (the library; features `browser` and `testing`) and `crates/eidolon-cli` (the `eidolon` command). [The design](docs/design.md) states the invariants; [CONTRIBUTING.md](CONTRIBUTING.md) lists the checks.
- Every call into Adytum is in `crates/eidolon/src/cell.rs`. Adytum is the one `adytum` crate, a path to a checkout beside this one in `Cargo.toml`. XenoNet's `xenonet-id`, `-os` and `-text` are git dependencies pinned to a commit there.
- Eidolon never hides automation from a site, never forwards the automation port to the host, and listens on nothing beyond 127.0.0.1. Each of these has a test; keep them.
- Tests run against Adytum's `FakeDriver` (`adytum::testing`), with `testing::folder_cells` for cells whose files are folders. Live tests (`ADYTUM_LIVE=1`) name every cell `adytum-test-*` and touch no other VM.
- A refusal gets its test first. Docs state the current state only.
