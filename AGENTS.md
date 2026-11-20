# Eidolon: notes for agents

Eidolon keeps everything that makes one entity present as one sealed thing with facets (a folder first, a browser profile second), opens it in an Adytum cell and moves it between machines, sending only what changed. The browser is one facet; the core must seal, open and move a plain folder with no browser code.

- Crates: `eidolon-model` (ids, manifest, seat), `-policy` (one seat, holder list, wall; pure), `-vault` (encrypted content-named chunks), `-pack`, `-facet`, `-move` (have-and-want over any byte stream, resumable, key wrapped to the receiver), `-bidi` (WebDriver BiDi client, fuzz-tested), `-browser`, `-view` (the person's window, localhost), `-mcp` (agent tools), `-testkit`, `-cli` (the `eidolon` command, `--json`).
- It depends on Adytum and on `xenonet-id`, `-os`, `-text` from XenoNet, as git dependencies pinned to a commit in `Cargo.toml`.
- Design: `docs/design.md`. Checks: see CONTRIBUTING.md. Live loop: `ADYTUM_LIVE=1 cargo test -p eidolon-cli --test live`.
- The automation port is never forwarded to the host, and Eidolon never hides automation from a site. Neither tool opens a network listener beyond localhost.
- A refusal gets its test first. Docs state the current state only.
