# Eidolon

Eidolon keeps the state of one identity, a folder of files or a signed-in Firefox profile, as one sealed thing called an eidolon. It opens the eidolon in an [Adytum](https://github.com/willctl/adytum) cell behind the identity's wall, and moves it to another of your machines, sending only the chunks that changed. Exactly one machine can open an eidolon at a time.

It is a Rust library, `eidolon`, and a command, `eidolon`, built from `eidolon-cli`.

## Install

Eidolon makes no cells of its own. Set up Adytum first and check it with `adytum doctor`. Then build the command:

```text
cargo install --locked --git https://github.com/willctl/eidolon eidolon-cli
```

## Move a signed-in browser from desk to tower

Each machine gets its keys once, and lists the other as a holder. The cells `adytum-a` on desk and `adytum-b` on tower are Adytum cells behind `wall_work`.

```text
desk$  eidolon machine init --name desk && eidolon machine show > desk.holder.json
tower$ eidolon machine init --name tower && eidolon machine show > tower.holder.json
desk$  eidolon new --wall wall_work --holder desk.holder.json --holder tower.holder.json --folder notes=/home/cell/notes --browser web
desk$  eidolon open <eid> --cell adytum-a
desk$  eidolon view <eid>
desk$  eidolon seal <eid>
desk$  eidolon move <eid> -- ssh tower eidolon receive
tower$ eidolon open <eid> --cell adytum-b
desk$  eidolon open <eid> --cell adytum-a
```

`eidolon new` prints the eidolon's id. The first open of a browser facet installs Firefox in the cell from Mozilla's repository. `eidolon view` prints a `vnc://127.0.0.1:<port>` address and a one-time secret: sign in through Screen Sharing, then press Enter. After the move, tower opens the eidolon still signed in, and the last command on desk is refused with `retired`.

Every command takes `--json`. Exit 0 is done, 1 is a refusal or a failure, and 2 is a mistake in how the command was typed. A refusal has a stable code, such as `second_seat` or `wrong_wall`. Run `eidolon help` for every command.

## Give an agent the browser

`eidolon mcp <eid>` serves the browser of an open eidolon as an MCP server on standard input and output:

```json
{ "mcpServers": { "web": { "command": "eidolon", "args": ["mcp", "<eid>"] } } }
```

The tools are `list_pages`, `new_page`, `select_page`, `close_page`, `navigate_page`, `navigate_history`, `take_snapshot`, `click_by_uid`, `hover_by_uid`, `fill_by_uid`, `type_text`, `press_key`, `get_page_text`, `screenshot_page`, `upload_file_by_uid`, `list_downloads` and `fetch_download`, named as in Mozilla's Firefox MCP server. They act on elements by the uids of the last snapshot. No tool runs arbitrary script. Files pass through one folder, `~/.eidolon/files/<eid>/`: an upload must resolve to a regular file inside it, and `fetch_download` copies into its `downloads` folder. While the sign-in window is open, the tools are refused. [The skill](skills/eidolon-browser/SKILL.md) tells an agent how to use them.

## Use the library

`eidolon::home::Home` is a machine's eidolon home. It makes, seals, opens, moves, settles and verifies eidolons, and returns typed results. `eidolon::cell::Cells::system()` opens Adytum's cells. The `browser` feature adds the BiDi client, the sign-in window and the agent tools. The `testing` feature adds `folder_cells`, Adytum's fake driver with each cell's files in a folder, and `FakeBidi`, a fake browser.

## Limits

- The keys are owner-only files under `~/.eidolon/keys`, because this build uses no operating-system key store. Anyone who can read your files as you can read the keys.
- A site that binds a session to one device, as Device Bound Session Credentials (DBSC) do, or to a fingerprint or a network, asks you to sign in again after a move.
- The receiving cell needs the same or a newer Firefox than the one that wrote the profile. Firefox does not open a profile from a newer version.
- Eidolon never hides automation. A site can see that the browser is driven and may ask for a CAPTCHA, which you answer in `eidolon view`.
- Cells are Adytum's Lima VMs. There is no Windows or WSL driver.
- Adytum writes a copy into a cell as the cell's user, so a folder facet opens only where that user can write, such as `/home/cell` or `/tmp`.
- The vault keeps every chunk it ever held. Nothing collects old ones yet.
- A move runs over a command you name, such as `ssh`. You are responsible for it reaching the right machine; the seat and the holder list hold either way.

[The design](docs/design.md) says what Eidolon guarantees and why.

## Tests

```text
cargo test --workspace --all-features
ADYTUM_LIVE=1 cargo test -p eidolon-cli --test live -- --test-threads=1
ADYTUM_LIVE=1 cargo test -p eidolon-cli --test live_browser -- --test-threads=1
```

The live tests make VMs named `adytum-test-*` in an Adytum home of their own and delete them at the end, also when a step fails.
