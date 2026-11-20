# Eidolon design

An eidolon is one identity's state: a set of facets, each a folder or a Firefox profile, sealed in a machine's vault. It opens in one Adytum cell behind its wall, and it moves whole between the machines on its holder list. This page states what Eidolon guarantees, and why each guarantee is built the way it is. The [README](../README.md) covers using it.

## One seat

Exactly one machine may open an eidolon at a time. That machine holds the seat, a signed record of the eidolon, a generation number, the holder, and the holder before it. A move writes the next generation, signed by the sender and naming the receiver. A receiver accepts a seat only if it is the next generation, signed by the holder of the current one, and names a machine on the holder list (`rules::accept_seat`).

A signature alone would let two machines claim generation 2 from the same generation 1. The generation and the previous holder make each seat follow exactly one other, so a copied or replayed seat is refused as stale.

An eidolon is `sealed`, `opening` in a cell, `open` in a cell, or `retired`. `opening` is written before the first file reaches the cell, and `open` only once all of them are there. A command killed in between leaves `opening`, and the next open finishes it, or clears that cell first if the person opens elsewhere. A seal writes every chunk, then the head, then the state, so a kill between any two leaves a holding that still opens. The kill tests (`eidolon-cli/tests/kill.rs`) stop the binary at each of these steps.

## The holder list

The holder list is fixed when the eidolon is made: each holder is a name, an Ed25519 key that signs seats and heads, and an X25519 key the eidolon's key is wrapped to. A machine off the list can neither receive nor open the eidolon. A machine that already holds the eidolon refuses a move whose holder list differs from its own, so a sender cannot add a machine on the way.

An eidolon opens only from a head that some holder signed. The seat names who may open it now, but the head was signed by whoever sealed it last, which after a few moves may be neither the holder nor the sender.

## The vault

Files are cut into content-defined chunks (16 KiB minimum, 64 KiB average, 256 KiB maximum), so an edit changes only the chunks around it. One 32-byte key per eidolon gives, by HKDF-SHA256, a name key and a content key. A chunk's name is an HMAC-SHA256 of its plaintext under the name key, so two eidolons that share a file share no chunk name. Its body is deflated and sealed with ChaCha20-Poly1305 under the content key, with a nonce taken from the name. Equal plaintexts therefore seal to equal bytes and are stored once, and no keystream ever covers two different messages.

A chunk that does not open, or opens to a plaintext with another name, is refused. The manifest, which lists every file with its mode, size and chunk names, is itself sealed into chunks. Only the head is in the clear: the eidolon, the generation and the names of the manifest's chunks, signed by a holder.

The eidolon key lives in the machine's key store under `eidolon/<id>`, never in a file beside the vault.

## The move protocol

A move runs over any byte stream: a pipe, `ssh`, or a stream another program carries. Frames are a u32 length, a type byte and a body.

1. The sender sends a hello with a fresh challenge. The receiver answers with its holder key and a signature over the challenge, so the sender knows which listed machine it talks to.
2. The sender offers the spec, its seat, the head and the names of every chunk the head needs. The receiver checks the spec, the seat and the list, and answers with the names its vault lacks.
3. The sender sends those chunks as they sit in its vault, still sealed. The receiver keeps them as they arrive, so a cut transfer resumes from what arrived.
4. The sender writes `pending.json`, then sends the next seat and the eidolon key wrapped to the receiver's X25519 key. The receiver checks the seat, opens every chunk, stores the key and acknowledges with a signature.
5. Only after a valid acknowledgement does the sender retire its copy and delete its key.

A move killed before the acknowledgement leaves the seat with the sender, and after it with the receiver. A sender that cannot tell, because the stream died after step 4, keeps its copy closed until the person settles it with `eidolon move-settle --abandon` or `--confirm <seat.json>`. A refusal the receiver signs after step 4 undoes the pending move, because the receiver proves it kept nothing. Whatever carries the stream sees sealed chunks and a wrapped key, nothing it can open.

## Never hiding automation

Eidolon never hides from a site that a browser is driven. There are no stealth preferences, no user-agent edits and no fingerprint changes, and a test (`nothing_hides_automation`) keeps it that way. Hiding automation is an arms race against the sites an identity uses, and losing it costs the identity. A site that asks for a CAPTCHA gets a person in `eidolon view`.

The agent tools act through snapshots and uids. No tool runs arbitrary script, and the page scripts Eidolon runs read the page without writing to it.

## The automation port is never forwarded

Firefox listens for WebDriver BiDi on the guest's own 127.0.0.1, and the cell's port-forward rules ignore it. The host reaches it as a byte stream: Adytum runs `socat` in the cell and its stdin and stdout carry the WebSocket. Anything that can reach a forwarded automation port can drive a signed-in browser, so no such port exists on the host.

## The sign-in window is loopback only

`eidolon view` listens on 127.0.0.1 alone, by construction, and each connection reaches the guest's screen share through its own cell stream. VNC's own authentication is weak, so both ends stay on loopback and the secret works for one session. While the window is open, the browser runs as a unit with no automation socket, which is what a site sees, and a marker in the holding makes the tools refuse.

## Where Adytum is used

Every call into Adytum is in `crates/eidolon/src/cell.rs`. `adytum::Cells` keeps a cell's wall, its holds and its starts in Adytum's ledger, and its driver runs commands, streams and copies. Adytum writes a copy into a cell as the user it is given, so the files belong to that user and land only where it could write anyway. A folder facet is written by the cell's user. The browser's profile and uploads are written by the browser's own user, so no folder in its home is open to anyone else: the profile lands beside the old one, root swaps the two, and the profile is closed to group and others.

## Next steps

These were weighed and not adopted:

- fastcdc or iroh-blobs for chunking and transfer: they change the vault's format or add heavy dependencies, and no bug calls for it.
- age for wrapping keys: it is new cryptography beside ring and x25519-dalek.
- Mozilla's or Playwright's MCP server in the cell: it needs Node in the image.
- Lima 2.3's `hostLoopback: false` and `--sync`, which would replace the in-guest guard.
- Collecting chunks no head names, so a vault stops growing.
