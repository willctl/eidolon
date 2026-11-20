# Contributing to Eidolon

Eidolon keeps everything that makes one entity present (a folder, a browser profile, keys) as one sealed thing that opens in an Adytum cell and moves between machines, sending only what changed.

## Checks

Rust is pinned in `rust-toolchain.toml`. Run these before a pull request; CI runs the same:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features --locked
cargo deny --locked check
cargo shear
```

Live tests that need real VMs run only with `ADYTUM_LIVE=1` set. They name every cell `adytum-test-*` and never touch other VMs. The fuzz target for the BiDi frame parser builds with a nightly Rust: `cargo fuzz build bidi_frame` in `fuzz/`.

## Rules

- Conventional Commits (`feat(mcp): ...`, `fix: ...`, `docs: ...`).
- `unsafe` is denied. No `unwrap`, `panic!` or `todo!` outside tests. An exception is `#[expect(lint, reason = "...")]`.
- Commands are argument lists, never shell strings. No secret in an argument, a temporary file or a log.
- A rule about what may cross a wall starts with the test that proves the refusal.
- Golden files change only when behaviour should.
- No real names, hostnames or accounts in the repository. Machines in docs are `desk` and `tower`.

## Writing copy

Plain words, sentence case, no em dashes, one idea per sentence, a mechanism or a number in each claim. Write "SI", never "AI", where the text needs the word.

By sending a pull request you agree that your change is under the [MIT license](LICENSE).
