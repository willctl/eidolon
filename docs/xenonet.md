# A move as a XenoNet act

Designed, not built. Nothing in Eidolon or in XenoNet implements this yet.

XenoNet is a separate program that signs acts between a person's machines and decides, on the receiving machine, who may do what. Eidolon depends on three of its library crates (`xenonet-id`, `xenonet-os`, `xenonet-text`) and on nothing of its network. This note says where a move could meet XenoNet. It changes none of XenoNet's authorization rules.

## Today

`eidolon move` runs over a byte stream the person names: a pipe, or a command such as `ssh` that Eidolon starts and speaks to. Between two machines of one network the same move could be a XenoNet act instead, so the person needs no shell access between them and the receiving machine decides.

## The act

The sender's XenoNet signs an act for the move and sends it to the receiver as it does any act from another machine. The act names the eidolon, its wall, the generation the move would write, and the sender's holder entry (public keys only). The receiver's `decide` takes it at the network door from a member, as it does a start from another machine, and then needs the person: a machine receiving an identity widens what that machine holds, so the nearest existing rows are `admit` and `widen`, with a presence block bound to the act's hash. The ask shows in the receiver's window ("tower wants to send you an eidolon for wall W: Allow?"), and Touch ID answers it. A decision to hold, because governance is stale, or a refusal, ends the move before a chunk is sent. Whether this is a new act kind or an existing one with a new resource is a decision for XenoNet's authorization design when it is built.

## The stream

Once the receiver allows, the move needs a long, two-way byte stream, which XenoNet's request and answer service is not (its requests and answers are 8 MiB at most). The stream that fits is XenoNet's `forward(alpn)`: Eidolon attaches as the actor for one registered ALPN, say `eidolon/move/1`, with the grant that names it, and XenoNet carries the stream between the two machines over the member connection. Registering the ALPN is a line of data in XenoNet's composition root. The forward is Unix only, so a move to or from Windows waits for its named pipe.

## What changes in Eidolon

Only the stream's source. The `move` and `receive` commands would read and write the forward's socket, where now they read and write a pipe. The protocol doesn't change: have and want, chunks sent as sealed in the vault, the key wrapped to the receiver's X25519 key, the seat record for the next generation, and the sender deleting its key only after the receiver's acknowledgement.

The seat doesn't change either. Eidolon still checks the record's generation, the signature of the current seat and the holder list on its own, so there are two separate decisions, XenoNet's on whether this machine takes the eidolon at all and Eidolon's on whether the record is valid, and neither stands in for the other. XenoNet terminates QUIC and so reads the forwarded bytes, which are sealed chunks and a wrapped key, nothing it can open. A move killed at any point leaves one seat, as it does over a pipe.

See [the design](design.md) for the move as it works today.
