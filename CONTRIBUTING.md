# Contributing to Eidolon

Eidolon seals a folder target into an `age(zstd(tar))` bundle, moves it between your machines with one holder at a time, and opens it safely. [The design](docs/design.md) says what it guarantees.

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

The live tests run only with `EIDOLON_LIVE=1`: `cargo test -p eidolon-cli --test live -- --test-threads=1 --nocapture`. They need Firefox in `/Applications` and `limactl` on PATH.

## Rules

- Conventional Commits (`feat(cli): ...`, `fix: ...`, `docs: ...`).
- `unsafe` is denied. No `unwrap`, `panic!` or `todo!` outside tests. An exception is `#[expect(lint, reason = "...")]`.
- Each dependency says why it is there, beside it in `Cargo.toml`.
- Commands are argument lists, never shell strings. No secret in an argument, a temporary file or a log.
- A rule that refuses something starts with the test that proves the refusal.
- No real names, hostnames or accounts in the repository. Machines in docs are `desk` and `tower`.

## Writing copy

Plain words, sentence case, no em dashes, one idea per sentence, a mechanism or a number in each claim. Write "SI", never "AI", where the text needs the word.

By sending a pull request you agree that your change is under the [MIT license](LICENSE).
