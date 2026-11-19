# Eidolon design

Eidolon keeps an entity's state as one sealed thing that opens in a cell and moves between machines. It opens no network listener beyond 127.0.0.1. It links Adytum's library crates for the cells and `xenonet-id`, `xenonet-os` and `xenonet-text` of XenoNet's for signing, files and text. Across machines, who may receive what is decided outside Eidolon; [XenoNet](xenonet.md) is the planned place.

```
Eidolon  an entity's state, sealed, opened in a cell, moved
   |     links Adytum's library crates
Adytum   cells on one machine (VMs and containers)
```

## Vocabulary

- An **eidolon** is a set of **facets** (a folder, a browser profile). It is **sealed** (encrypted chunks in a vault) or **open** (laid out in one cell). It has one **seat**: the machine that may open it now.
- A **holder** is a machine on the eidolon's holder list, named by its Ed25519 key.
- A **cell** is an Adytum VM or container, and a **wall** is the opaque label every cell carries. An eidolon opens only in a cell behind its own wall.

## Crates

| Crate | Job |
|---|---|
| `eidolon-model` | Typed ids (`eid_`, `fct_`), walls, facet specs, holders, the seat record and its `State` (`Sealed`, `Opening`, `Open`, `Retired`) |
| `eidolon-policy` | One seat, the holder list, the wall, what a facet may leave out |
| `eidolon-vault` | Chunks named by a keyed hash of their content, encrypted at rest |
| `eidolon-pack` | A facet's files into chunks, the manifest that lists them, and the signed head that names the manifest's chunks; and back |
| `eidolon-facet` | The facet trait and the `folder` kind |
| `eidolon-move` | Have-and-want over any byte stream, resumable, key wrapped to the receiver, the seat passed |
| `eidolon-bidi` | A WebDriver BiDi client |
| `eidolon-browser` | The browser facet: profile rules, the systemd units, a browser kept alive in a cell |
| `eidolon-view` | The person's window for sign-ins: a screen share on 127.0.0.1 |
| `eidolon-mcp` | The browser facet's tools for agents, over stdio or over HTTP on localhost |
| `eidolon-cli` | The `eidolon` binary, and the machine's file key store |
| `eidolon-testkit` | A fake BiDi server |

## Sealed

- Files are cut into content-defined chunks, so an edit in the middle of a file changes the chunks around it and no others.
- A chunk's name is an HMAC-SHA256 of its plaintext under the eidolon's name key, so the vault doesn't show which eidolons share a file. Its body is compressed, then sealed with ChaCha20-Poly1305 under the eidolon's content key, the nonce taken from the name. A chunk whose body doesn't open, or opens to a plaintext with another name, is refused.
- Both keys come from one 32-byte eidolon key, kept under the eidolon's name in the machine's key store (`xenonet_id::store::KeyStore`), never in a file beside the vault. On this platform the store is a file store, one owner-only file per secret under `<home>/keys`, because no operating system store is used yet; `eidolon machine init` says so.
- The manifest (in `eidolon-pack`) lists each facet's files (path, mode, size, chunk names) and is itself sealed into chunks. The head names those chunks and is signed by a machine key. An eidolon opens only if some machine on the holder list signed its head, because after several moves the signer can be neither the current holder nor the sender of the last move.

## One seat

The seat is a signed record: the eidolon, a generation number, and the holder that may open it. A move writes the next generation, naming the receiver, signed by the sender. The sender then deletes its eidolon key, so its vault can no longer open the eidolon even though its chunks remain. A receiver refuses a seat record that isn't the next generation, isn't signed by the current seat, or names a machine off the holder list.

An eidolon is `Sealed`, `Opening`, `Open` in one cell, or `Retired`. `Opening` is written before the first file is placed in a cell, so a kill during an open is found by the next command, which clears the cell or finishes the open. A sender that cannot tell whether the receiver took the eidolon keeps it closed until the person settles it (`move-settle`).

## Moving

Over any byte stream (a pipe, Adytum's duplex exec mode, SSH):

1. The sender offers the manifest's chunk names.
2. The receiver answers with the names it lacks.
3. The sender sends those chunks as they sit in its vault, already sealed. The receiver checks each and keeps it, so a broken transfer resumes from what arrived.
4. The sender wraps the eidolon key to the receiver's X25519 key and sends it with the new seat record. The receiver checks the record, stores the key and acknowledges. Only then does the sender delete its key.

A move killed before the acknowledgement leaves the seat with the sender; after it, with the receiver. There is never a moment with two seats or none.

## The browser facet

Firefox runs in its own cell, kept alive by systemd units, with no program of Eidolon's in the guest. The cell is set up on its first open: Firefox from Mozilla's package repository, a virtual display and a screen share. Its automation is WebDriver BiDi on a socket in the guest, which the host reaches as a byte stream over Adytum's exec channel; the port is never forwarded. Ready means the protocol answered. Before sealing, the browser is stopped so its profile is quiet, and caches are left out. Eidolon does not hide automation from sites.

The person signs in through a window (`eidolon view`) on 127.0.0.1 with a secret per session, shown once. The window is three units: the browser without its automation socket, a virtual display and a screen share. Opening it stops the automation browser, and closing it starts that browser again on the same profile. While it is open, automation is off. The host reaches the screen share as a byte stream over Adytum's exec channel, as it does the automation socket; nothing is forwarded.

Agents get the browser through `eidolon mcp <eid>`, an MCP server on standard input and output, or `eidolon mcp <eid> --http 127.0.0.1:<port>`, the same tools over HTTP on the loopback address only. The HTTP form makes a token, writes it once to standard error and checks it on every request; it answers only on `/mcp/` and refuses a request that carries an `Origin` header. A program that publishes that address to other machines, as XenoNet can, lets a session there call it with the token as its `Authorization` header. Files for uploads and downloads pass through one folder on the host, `$EIDOLON_HOME/files/<eid>/`. An upload must resolve to a regular file inside it, and a fetched download lands in its `downloads` folder. [The skill](../skills/eidolon-browser/SKILL.md) tells an agent how to use the tools.

## The alignment check

- **Which goal?** Secure by construction: a wall is a machine boundary, so a session can't act as an identity that isn't in its cell. An identity's state moves to the machine where the work is. Each tool is one command with a doctor.
- **Intent to outcome.** A session given an identity's task works in a cell holding that identity's eidolon, on whichever host holds it; the browser facet lets it post, upload and download as that identity.
- **One-of-anything?** None. Any number of eidolons per machine; an eidolon's holder list names any number of machines. The one seat is deliberate: one live copy of a signed-in session.
- **Who, under what authority?** Eidolon acts only for the person on their own machine. An eidolon's seat record is signed by the machine key that held it, and the holder list bounds who may receive it. Across machines a request to move is decided by the receiving machine, outside Eidolon.
- **Push or poll?** The browser's readiness and events are BiDi messages, never sleeps or polls. Moves send only missing chunks.
- **Vendor knowledge in the core?** None: Eidolon knows browsers through one facet kind and cells through Adytum's driver trait.
- **Existing protocols.** WebDriver BiDi for the browser, content-defined chunking and have-and-want as rsync and git do it, ring and x25519-dalek for the cryptography. Eidolon's own format covers only the manifest and the seat record.
- **How we know.** Refusal tests for every rule, kill tests for the move, and a live gate (`ADYTUM_LIVE=1 cargo test -p eidolon-cli --test live`) that proves its own teardown.

## Related

- [The guide](guide.md), for using Eidolon.
- [XenoNet](xenonet.md), the planned move as a network act.
- Adytum, which makes the cells, has its own design page.
- [The README](../README.md) and [release notes](../RELEASES.md).
