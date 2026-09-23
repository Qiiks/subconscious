# NATS across a user's machines: hub topology and end-to-end sealing

Status: r2. r1 (c9f403c8) was reviewed by CALLO (callosum), CKCRED (key custody)
and ALF (ck-bus and the foundation spec); their answers are folded in below and
the changes from r1 are listed at the end. An Athena feasibility and
threat-model pass is running on r1. Two decisions remain with the operator (see
"Decisions needed"). The ck-bus spec's credential slices stay held until this
settles.

## What the operator settled

1. **Topology.** Hub by default. A user can self-host the hub, with a setup
   ceremony that has good UX, and can opt in to a hub CortexKit hosts.
2. **Split tolerance.** A cross-machine message sent during a network split is
   delivered once both sides reconnect. Late delivery is acceptable; loss is not.
3. **The hub must not read message contents.**
4. **Callosum stays**, with the role defined below.
5. **Auth is shape D**: signed NATS operator, account and user keys. The hub must
   verify credentials without calling back into a machine, and a hosted hub must
   isolate users by account; signed identities are the standard NATS answer to
   both.

## What already exists

From the foundation spec (prefrontal `docs/specs/nats-message-plane-foundation.md`)
and the ck-bus spec (`docs/specs/ck-bus-module.md`):

- One `nats-server` per machine, supervised by subc with `protocol: "none"`;
  ck-bus owns credentials and the plane. Local traffic never leaves the machine.
- Cross-host is a NATS leaf connection on its own TCP, never Callosum's fed-wire.
  The leaf learns its hub from `callosum.hub_read` (designed, not yet built):
  the roster row marked as hub, its pinned identity and its candidate addresses.
- Accounts are per box (`{acct}`), and subjects are built from opaque registry
  ids only: `agent_id` (`agent_20dabecd…`), `room_id` (`rm_…`), `session_id`
  (`ses_…`), never names, paths, device keys or credentials.
- The broker carries deliveries, not state of record: a message is an id plus a
  SHA-256 digest resolved from the owning module's store.
- The five streams: `CK_{ACCT}_ROOM`, `_WAKE`, `_PEER`, the work queue `_EFFECT`,
  and `_EFFECT_DEAD`.

From Callosum:

- Pairing compares a 30-digit code: SHA-256 over `fed-verify-v1` and the two
  machines' **X25519** transport statics (`fed-core/src/identity.rs`
  `verification_code`). The roster stores that X25519 key per peer and nothing
  else of the peer's. The Ed25519 device key is bound to X25519 only at
  rendezvous enrollment, which is cloud trust.
- A Noise IK session with a paired peer authenticates that peer's X25519 static,
  so every byte received in that session came from the machine the user paired.
- Removal is either a signed tombstone (rendezvous) or a local `roster retire`
  with no cloud author.
- A dedicated HPKE recipient key already exists for sealed phone pushes
  (`push_seal_pubkey_hex`, RFC 9180 base mode, X25519/HKDF-SHA256/
  ChaCha20-Poly1305). It is cloud-published and is not reused here.

## The shape

```
 machine A                        hub (self-hosted or ours)                machine B
 agents ── ck-bus ── nats(leaf) ══ TLS ══ nats(hub): one account per user ══ TLS ══ nats(leaf) ── ck-bus ── agents
           seals + signs                  stores and forwards ciphertext                  verifies + opens (vault)
```

- **Local messages** stay on the machine's own server.
- **Cross-machine messages** travel A's leaf → hub → B's leaf and are held in
  JetStream through a split, then caught up (mirroring and sourcing across
  JetStream domains is store and forward). Order is kept per sender.
- **Hub choices**: one of the user's always-on machines, or our hosted hub. A
  clustered hub across the user's own machines is ruled out: JetStream
  clustering needs a majority, so two machines cannot form one.
- **Hosted hub** = the same design with our server in the roster's hub row.

### Accounts and routing (ALF, CALLO)

- **One hub account per user.** Box accounts stay local-server accounts that the
  leaf binds into it. They cannot become hub accounts, because each box has its
  own local operator and the hub trusts only the hub operator; re-signing would
  rename the account. This also matches what pairing proves: "machines this user
  paired" is the trust unit, and callosum has no authority to put a boundary
  between one user's own machines.
- **`{acct}` in a subject names the recipient box.** A publishes
  `ck.{b}.peer.…`; the hub routes on subject interest inside the user's account;
  B's local stream binding catches it unchanged. A hosted hub isolates users by
  that one account.
- **Leaf grant, tighter than the foundation's "`ck.{acct}.>` for the linked
  accounts":** a box's leaf may subscribe only to its own `ck.{own}.>`, and may
  publish to another box only on the cross-machine families below. Otherwise A's
  leaf could subscribe to B's whole namespace (ciphertext, but the whole
  envelope).
- **Which families cross machines: ROOM, WAKE and PEER. EFFECT and EFFECT_DEAD
  never do.** An effect intent means "run this where the session lives"; the
  work-queue claim does not survive a leaf hop. Enforced by the leaf publish
  grant (never `ck.{other}.effect.>`), not by convention.

### A stable box id is missing (CALLO)

`{acct}` must never change for the life of a box, because every stream, bucket
and durable cursor embeds it. But callosum has **no durable per-box id**: the
roster keys on the X25519 transport key, and a re-key or compromise recovery
makes a new peer; `fed_identity.incarnation` is re-minted on store recreation and
on trust import. Deriving `{acct}` from either would rename the box on every
re-key. So a stable box id must be minted once per machine and carried through
re-keys and trust export/import. See "Decisions needed".

## Streams across a split (ALF)

The foundation sets ROOM, WAKE and PEER to max-age 24 h, max-bytes 1 GiB,
discard old. Under store and forward that is **loss**: a laptop closed for a
weekend comes back to a hub that has aged its messages out, and nobody is told.
Whatever holds cross-machine traffic during a split needs:

- **max-age sized to realistic splits**: days, not hours;
- **discard new**: a full outbox refuses the sender with `Unavailable` naming the
  limit, instead of dropping the oldest message silently;
- **a stated bound** on how long a machine may be away before it must re-sync
  from a store rather than from the stream;
- **the recipient's idempotent insert on delivery id as the dedupe authority**:
  after a long split, sourcing can redeliver past JetStream's duplicate window
  (2 min by default).

## End-to-end sealing: the hub carries ciphertext only

Two layers, both keyed per machine:

| Layer | Between | Protects against | Source |
| --- | --- | --- | --- |
| Hop encryption | each machine ↔ hub | the network | TLS on the leaf link |
| End-to-end seal | sending machine → receiving machine | **the hub** | this design |

**The recipient is a machine, not an agent.** Every agent and module on B shares
B's keys; B's ck-bus opens the body and delivers it over B's local bus. A message
to one agent on B is sealed once; a broadcast to all of a user's machines is
sealed once per machine.

### What gets sealed

For any delivery whose recipient is on another machine, **the body travels inline
and sealed** (ALF accepts this change to the foundation's id-plus-digest rule).
Resolving the body from the sender's store would need the sender online, which
breaks store and forward. The recipient's store becomes the store of record once
it opens the body, so no stream outlives its owning store. Same-machine
deliveries keep the foundation's shape.

**The digest moves inside the seal.** A plaintext digest in a header would give
the hub a confirmation oracle for low-entropy bodies ("yes", "approved", an
option label). The hub needs only the delivery id, for dedupe and purge.

### Keys: exchanged inside the paired session, not certified by a signature

r1 proposed binding each machine's seal key to its Ed25519 device key. CALLO
showed that does not work: the pairing code covers only the X25519 statics and
the roster stores no Ed25519 key, so an Ed25519-signed seal key would inherit the
registry's trust, which is exactly the service the seal must exclude.

What does inherit the pairing is the Noise IK session. So:

- Each machine holds **two operational keys**, both in its vault: a **seal key**
  (X25519, for HPKE) and a **message-signing key** (Ed25519). Neither is the
  Noise static, the Ed25519 enrollment key, or the phone push-seal key.
- A machine sends its peers a **key record** — both public keys plus a
  **generation** — **inside a Noise session with each paired peer** (a hello field
  or a local management op). The recipient stores it on the roster beside
  `peer_pubkey_hex` and **accepts it from no other source**. No signature is
  needed to bind it: the session already proves which paired machine sent it.
  The hub and the registry never carry key records.
- **Highest generation wins.** A record with a lower generation than one already
  held is refused, so a stale record (a restored backup, a replay) cannot roll a
  rotated key back.
- Both removal paths — the tombstone and a local `roster retire` — clear the key
  record, and it joins the trust export/import set so a restored machine keeps
  its peers' keys under the same rules.

### Per message (CKCRED)

- **Sender authenticity is an Ed25519 signature, not HPKE auth mode.** Auth mode
  is key-compromise-impersonable (RFC 9180 §9.1.1): whoever steals B's seal key
  could forge messages to B from any sender. With a signature, a stolen seal key
  lets the thief read B's mail and impersonate nobody.
- **Both identities are bound into both layers**: the HPKE `info` carries
  `sender_machine_id | recipient_machine_id`, and the signature covers
  `version | sender | recipient | enc | ct`. Without the recipient in the signed
  bytes, a signed body could be re-sealed to someone else; without the sender in
  `info`, a sealed blob could be re-signed by someone who cannot read it.
- **Vault custody.** The seal private key is a new Claustrum kind (working name
  `KemKey`), refused by `get` and `sign`, usable only by a new in-vault
  `credential.open` that returns plaintext. `open` gets its own grant operation,
  given only to `reserved:ckbus`, because it is a decryption oracle for
  everything sealed to the machine. The message-signing key is an ordinary
  `SigningKey` used through `credential.sign`.
- **No forward secrecy.** A static recipient key means one leak of B's seal key
  opens every ciphertext still sealed to B, including what the hub holds for
  split tolerance. So the key stays in the vault, and the hub deletes a message
  once the recipient acks it, with a stated upper bound on retention.
- **Liveness.** Opening goes through the vault, so while Claustrum is down,
  received messages wait unopened. Store and forward absorbs that (late, not
  lost). Sealing needs only the recipient's public key.

### What the hub can still see

- **Subjects**, which carry opaque registry ids (`agent_…`, `rm_…`, `ses_…`) and
  the recipient box's `{acct}`. Tokens are required to be opaque ids; no name,
  hostname or path may ever appear in one. The stable box id (above) must be
  opaque for the same reason — it lives in `{acct}`, the most visible token.
- **Headers**, if any are set; nothing sensitive may go in one, including the
  digest.
- **Sizes, timing and the machine graph.**

Hashing the ids was considered and dropped for now (ALF): the tokens are already
pseudonyms, so hashing swaps one stable pseudonym for another. It would only help
if our hub operator could join the raw ids from another service we run, and it
would make every subject build need a per-user key, rename every stream binding
on key rotation, and make logs unreadable. If it is ever done, it must be one
form everywhere, local subjects included.

## Leaf signing

The vault never exports a signing key (`get` on `SigningKey` is refused by
design), and stock `nats-server` config gives a leaf remote only `credentials` (a
creds file) or `nkey`. But `nats-server`'s Go API has a leaf **`SignatureCB`**
on `RemoteLeafOpts`, which signs the connect nonce through a callback instead of
a file — available when the server is embedded in a Go program, not through the
config file. So the foundation's three shapes become:

1. **Rust bridge** holding an `async-nats` connection to each side: seed stays in
   the vault, costs a copy and a hop.
2. **Memory-backed creds fd**: only if the leaf key is ephemeral and never a
   vault record, with a short-lived JWT; a bearer in the server's heap until
   expiry.
3. **A thin Go binary embedding `nats-server` with `SignatureCB`** calling
   `credential.sign` over subc: seed stays in the vault, no extra hop, and no
   upstream change needed. Costs a Go component in a Rust fleet.

CKCRED prefers 1 or 3 on custody. Signing happens only at connect and reconnect,
which split tolerance already absorbs. Shape 3 is the recommendation, pending a
check that `SignatureCB` is in the `nats-server` version we pin.

## Callosum and NATS: who does what

| Concern | Owner | State |
| --- | --- | --- |
| Which machines are the user's; pairing; removal | Callosum pairing ledger | built |
| Operational key records, exchanged in the paired session | Callosum roster + session | new |
| Telling each leaf its hub and pinning the hub's identity | `callosum.hub_read` | designed |
| Direct machine-to-machine calls, LAN, low latency | Callosum fed-wire and dial ladder | built |
| Moving and storing messages between machines, catch-up | NATS leaf + hub | new |
| Getting through NAT for messaging | NATS: machines dial out to the hub | new |

## Setup and the hosted opt-in

- **Default, self-hosted hub**: during pairing the user marks one machine as the
  hub. The ceremony mints the hub operator and the user's hub account on that
  machine and each machine's leaf credential. The user is the operator.
- **Hosted hub (opt-in)**: CortexKit is the operator, in our infrastructure, never
  in a user's vault. The user's account is created on our cluster and the roster's
  hub row points at our endpoint with our pinned identity. Bodies are sealed to
  keys we never hold, so the hosted hub sees what a self-hosted one sees.
- Switching between the two is a roster change plus a leaf re-issue and must not
  change any box id.

## Decisions needed

1. **Rooms across machines (ALF; the largest item).** Store and forward delivers
   every post body to B, but `room_wait` reads the transcript and board from the
   room's owning store, which lives on one machine. B can receive every post while
   the owner is offline and still be unable to read the room.
   - **(a) Replicate the room log to member machines.** Rooms work fully offline
     from the owner. A stream or a per-machine replica becomes a state of record,
     which is a foundation change, and board state needs a merge rule.
   - **(b) Cross-machine rooms need the owner online to read.** Posts still arrive
     late but safe; reading the room waits for the owner. No foundation change.
2. **Who mints the stable box id.** It must survive re-keys and trust
   export/import, be opaque, and exist before any stream is created. Callosum
   (it owns machine identity and export/import) or ck-bus (it owns `{acct}`).

## Remaining checks

- `SignatureCB` in the pinned `nats-server` version (SUBC).
- ck-bus presents `ConsumerIdentity` on its `route.open`, since without it
  `sign` and `open` answer `not_found`, identical to "no such key" (ck-bus spec
  already requires it; slice 0 verifies it).
- Athena's threat-model findings, folded into r3.

## Changes from r1

- Key binding: exchanged inside the Noise session with each paired peer, not
  certified by the Ed25519 device key (the pairing never covered that key).
- Two operational keys per machine, a generation with highest-wins, removal on
  both paths, part of trust export/import.
- Sender authenticity by Ed25519 signature over both identities; HPKE auth mode
  rejected. Seal key as a new vault kind with a separate `open` grant.
- One hub account per user; `{acct}` names the recipient box; leaf subscribe
  only to its own namespace; EFFECT never crosses machines.
- Stream limits for split tolerance: days of max-age, discard new, a re-sync
  bound, recipient-side dedupe.
- Digest moves inside the seal. No subject hashing for now.
- Leaf signing: `SignatureCB` in an embedded Go server as a third, upstream-free
  shape.
- Stated facts corrected: callosum has no stable box id, seal keys and `hub_read`
  are not built.
- New decisions: rooms across machines, and who mints the stable box id.
