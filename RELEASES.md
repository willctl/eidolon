# Eidolon release notes

## 0.1.0 (unreleased)

- Keep a folder of a cell, or a Firefox profile with its sign-ins, as one sealed eidolon with `eidolon new`, `seal` and `open`.
- Move an eidolon to another of your machines with `eidolon move <eid> -- ssh tower eidolon receive`. Only the chunks that changed cross, and only one machine can open it at a time.
- Sign in to a site yourself with `eidolon view <eid>`. Automation is off while the window is open.
- Give an agent the browser with `eidolon mcp <eid>`.
- Use the same operations from Rust through the `eidolon` library.

It needs Adytum for the cells, and builds with `cargo install --git`. [The README](README.md) lists the limits.
