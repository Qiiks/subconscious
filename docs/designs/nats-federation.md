# NATS across a user's machines: hub topology and end-to-end sealing

Status: r3. r1 (c9f403c8) was reviewed by CALLO (callosum), CKCRED (key custody)
and ALF (ck-bus and the foundation spec), and by an Athena feasibility and
threat-model panel (four seats). Their findings are folded in below; the changes
are listed at the end. The panel's headline: no finding makes "the hub cannot
read contents" unachievable, and the one it ranked blocking on r1 (the seal key
anchored to a key the pairing never covered) is what r2 fixed. One decision
remains open (rooms across machines). The ck-bus spec's credential slices stay
held until this settles.

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
  clustering needs a majority, so a two-machine cluster stops accepting writes
  as soon as either machine is unreachable, which is exactly a split.
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
- **The leaf carries a federation namespace, never the box's local subjects
  (Athena, critical).** A leaf forwards any message on its bound account that
  the far side shows interest in. If the leaf were bound to the box account, a
  hub-side subscription on `ck.{own}.>` would pull the box's LOCAL traffic
  across: local deliveries are unsealed (id plus plaintext digest, and some
  slices carry bodies inline). So each box has a separate local **federation
  account** bound to the leaf remote, holding only sealed cross-machine subjects
  (an outbox and an inbox family), and **ck-bus is the only component that moves
  messages between the box account and the federation account**, sealing on the
  way out and opening on the way in. The restriction lives on the local server,
  not in the hub-signed grant, because the hub signs that grant. This changes
  the foundation, which gives ck-bus no workload publish rights: ck-bus gains
  exactly the move between the two accounts and still holds no workload publish
  rights in the box account (ALF agreed).
- **Leaf grant, inside that federation account:** a box's leaf may subscribe
  only to its own inbox, and may publish only to other boxes' inboxes on the
  cross-machine families below. Effect subjects never exist in the federation
  account at all, so EFFECT staying local is structural, not a grant rule.
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
- **Removal is not instantaneous (Athena).** Each sender stops sealing to a
  removed machine when it learns of the removal, and must then durably fence
  both sealing to it and accepting from it. A sender that is offline, or shown
  stale state, keeps sealing to it until it learns. The removed machine can open
  everything sealed to any key it kept, including copies the hub still holds.
  Removal must also revoke that machine's leaf credential at the hub (the
  account's revocation list, pushed as a claims update) and close its
  connection; the callosum tombstone does not do that on its own, so the ceremony
  has to wire it. A cooperative hub can purge the removed machine's inbox; a
  hostile one cannot be forced to.
- **This machine's own generation is carried in the trust document** too (CALLO).
  A restored box that reset its counter would publish fresh keys at a low
  generation, every peer would keep the stale record, and those are exactly the
  keys lost with the old box.
- **Rotation needs the two machines to meet** (CKCRED). A new key record reaches
  A only in A's next session with B, direct or relayed through callosum; the hub
  cannot carry it, because nothing it relays is signed by anything A trusts.
  Until then A keeps sealing to B's old key. So B keeps the old generation's key
  able to open until every peer has acknowledged the new one, not until B
  rotates. An emergency rotation (a key suspected leaked) is therefore not
  immediate: senders keep using the leaked key until they next meet B, and the
  bound is how often machines meet.

### Per message (CKCRED)

- **Sender authenticity is an Ed25519 signature, not HPKE auth mode.** Auth mode
  is key-compromise-impersonable (RFC 9180 §9.1.1): whoever steals B's seal key
  could forge messages to B from any sender. With a signature, a stolen seal key
  lets the thief read B's mail and impersonate nobody.
- **Sign inside, then seal, and bind the whole destination (Athena).** Every
  agent on B shares B's key and the addressing lives in the cleartext subject,
  so binding only the two machines would let the hub move a valid blob from one
  agent or session to another on the same machine, or replay it later. The
  sender signs a purpose-tagged context covering: envelope version, sender
  machine, recipient machine, the full destination (family, `agent_id`, and
  `session_id` or `room_id`), message id, a **per-sender sequence number**, and
  the body. That signed plaintext is then sealed, with
  `sender_machine_id | recipient_machine_id` also in the HPKE `info`. Signing
  inside the seal also hides from the hub which key signed. The recipient
  rejects any mismatch between the opened destination and the subject it
  arrived on.
- **Unsigned or unverifiable blobs are refused outright.** Base mode lets anyone
  with B's public key produce a blob B can open, so a blob without a valid
  signature by a paired sender's current key is never delivered.
- **Per-sender sequence detects drop, replay and reorder.** The recipient keeps
  a durable high-water mark per paired sender, deduplicates on (sender,
  sequence), and reports gaps. With discard-new and days of max-age a gap means
  loss or hub misbehaviour, never ordinary expiry, which is what makes "loss is
  not acceptable" checkable against a hostile hub.
- **An unopenable blob is quarantined, never acked.** A blob sealed to a retired
  key generation, or failing verification, is neither `Absent` nor a digest
  mismatch; it gets its own terminal disposition and is reported.
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
- **JetStream bookkeeping (Athena):** stream and durable consumer names, ack and
  API subjects, delivery and redelivery counts, and ack timing, so the hub learns
  when a message was read, not only when it was sent; `Nats-Msg-Id` and source
  headers; and fan-out, since a broadcast sealed once per machine shows how many
  machines are in the room.
- **As operator of a hosted hub:** `$SYS` connection events, client IPs and each
  machine's online windows. The operator can also mint users in the tenant's
  account, so it can drop, delay or inject; injection is stopped by the
  signatures, and drop is surfaced by the per-sender sequence.
- **Never on the hub:** a box's census bucket and `$SYS` stay local; the
  federation account carries neither.
- **A hosted hub is never a recipient.** A broadcast to "all my machines" never
  seals a copy to the hub's own identity. A self-hosted hub that is also one of
  the user's machines is an ordinary endpoint for messages addressed to it.

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

What shape 3 commits us to (CKCRED):

- The Go binary is a vault caller, so it needs its own identity: a supervised
  module whose `route.open` carries `ConsumerIdentity`, with an exact `sign`
  grant on the leaf key, or a callback that goes through ck-bus instead. Either
  way it needs a Go subc client that sends the identity; without it `sign`
  answers `not_found`, identical to "no such key", and reads like a missing grant.
- The callback runs at every connect and reconnect, so a vault outage during a
  hub reconnect keeps the leaf down until the vault returns. The connect error
  must name the signer, so "vault unavailable" and "hub refused us" read
  differently.
- It is no longer stock `nats-server`: an upstream security fix reaches users
  only when we rebuild and ship our binary. That is a release-cadence commitment.

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

1. **Rooms across machines (ALF; the largest item). OPEN, widened by the
   operator:** the default expectation is a local copy on every member machine,
   not fetching room data from another machine, and this needs its own design
   pass on how prefrontal as a whole works across machines (rooms, boards, asks,
   peers, wakes). The two options below are kept for the record; (b) is not the
   direction. Store and forward delivers
   every post body to B, but `room_wait` reads the transcript and board from the
   room's owning store, which lives on one machine. B can receive every post while
   the owner is offline and still be unable to read the room.
   - **(a) Replicate the room log to member machines.** Rooms work fully offline
     from the owner. A stream or a per-machine replica becomes a state of record,
     which is a foundation change, and board state needs a merge rule.
   - **(b) Cross-machine rooms need the owner online to read.** Posts still arrive
     late but safe; reading the room waits for the owner. No foundation change.
2. **Who mints the stable box id. DECIDED 2026-09-23: callosum**, as the
   cross-machine module that owns machine identity. It must survive re-keys and
   trust export/import, be opaque, and exist before any stream is created.
   CALLO's case for it: it can mint the id at store creation, a
   re-key leaves a separate id column in place, the trust export already carries
   its tables, and peers learn the id inside the paired session like the keys.
   ck-bus would need its own durable store and backup contract, and could never
   tie the id to a paired machine. Two constraints either way:
   - **It is a name, never an authority.** Nothing may admit, route trust or skip
     a check because two messages carry the same box id; authority stays on the
     roster row's key. The id outlives a compromise re-key by design, so treating
     it as proof would let a revoked key's history vouch for its replacement.
   - **A clone must be detectable.** One trust export imported on two machines
     gives two live boxes with one id and different transport keys. A box id
     announced over a session whose key differs from the key already bound to it
     is a conflict to refuse and surface, not a rotation. Only callosum holds the
     key to see this.

## Remaining checks

- An executable rig arm, not more reading, for the stock `nats-server` behaviour
  every reviewer asserted from general knowledge: leaf routing from per-box
  federation accounts into a per-user hub account, cross-domain sourcing through
  a split, a subject-filtered purge of one recipient's inbox, and that local
  subjects stay off the leaf under a hub-side subscription.

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

## Changes in r3

- Callosum mints the stable box id (operator ruling); the rooms question widens
  into a prefrontal-across-machines design pass.
- Rotation needs the machines to meet; the old key stays openable until every
  peer acknowledges the new generation; the machine's own generation rides the
  trust document (CKCRED, CALLO).
- Shape 3 leaf signing: its own vault identity, a named signer in connect
  errors, and the rebuild commitment (CKCRED).
- From Athena: a separate federation account so local plaintext can never cross
  the leaf; sign-inside-then-seal over the full destination, message id and a
  per-sender sequence; unsigned blobs refused and unopenable ones quarantined; a
  fuller list of what the hub sees; a hosted hub is never a recipient; removal is
  not instantaneous and must revoke the leaf credential; a rig arm for the
  unverified NATS behaviour.
