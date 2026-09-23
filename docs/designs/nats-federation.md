# NATS across a user's machines: hub topology and end-to-end sealing

Status: draft for review by CALLO (callosum), CKCRED (keys), ALF (ck-bus and
the foundation spec), then an Athena feasibility and threat-model pass. The
ck-bus spec's credential slices are held until this settles, because the hub
side is what decides the credential shape.

## What the operator settled

1. **Topology.** Hub by default. A user can self-host the hub, with a setup
   ceremony that has good UX, and can opt in to a hub CortexKit hosts.
2. **Split tolerance.** A cross-machine message sent during a network split is
   delivered once both sides reconnect. Late delivery is acceptable; loss is not.
3. **The hub must not read message contents** — if that is feasible, it is worth
   the investment. This note argues it is.
4. **Callosum stays.** What it built is kept and has a defined role below.
5. **Auth is shape D**: signed NATS operator, account and user keys. With a hub
   (and certainly a hosted one) the hub must verify credentials without calling
   back into a machine, and a hosted hub must isolate users by account; signed
   identities are the standard NATS answer to both.

## What already exists, so this note adds rather than replaces

From the foundation spec (prefrontal `docs/specs/nats-message-plane-foundation.md`)
and the ck-bus spec (`docs/specs/ck-bus-module.md`):

- One `nats-server` per machine, supervised by subc with `protocol: "none"`;
  ck-bus owns credentials and the plane. Local traffic never leaves the machine.
- **Cross-host is a NATS leaf connection on its own TCP, never Callosum's
  fed-wire** (fed-wire is request/response only). The leaf learns its hub from
  `callosum.hub_read`: the roster row marked as hub, its pinned identity, and
  candidate addresses. Exactly one hub; a tombstoned or changed hub identity
  disconnects the leaf.
- Accounts are per box: `{acct}` = `box_<roster_host_id>`, fixed for the life of
  the box. Pairing links a box account to a hub and never renames it.
- The leaf credential is issued by the hub's account, scoped to `ck.{acct}.>`
  for the linked accounts, with no census or system-account rights. No process
  credential ever crosses a host.
- **The broker carries deliveries, not state of record**: a message is an id plus
  a SHA-256 digest, and the consumer resolves the body from the owning module's
  store. Some port slices carry the body inline instead and say so.
- Subjects are built from registry ids only (`agent_id`, `session_id`,
  `room_id`, `acct`), never paths, device keys or credentials.
- Leaf-side signing (how a stock `nats-server` authenticates its outbound leaf
  without a seed on disk) is an open decision owed by SUBC and CKCRED, with three
  candidate shapes.
- The hosted hub is named as a later phase and not designed.

From Callosum (`callosum:docs/rdv-wire.md`, `docs/specs/push-sealed-payload.md`):

- Each device enrolls an X25519 (Noise transport) and an Ed25519 key, proves
  possession of both, and appears on an account-scoped, signed registry. Removal
  writes a signed tombstone and closes the device's live connections; a
  tombstoned key re-enrolling is flagged and must not be quietly re-trusted.
- Per the Device-Axis design, **the local pairing ledger is the trust root**;
  cloud copies are for discovery only.
- A dedicated HPKE recipient key (`push_seal_pubkey_hex`) already exists for
  sealed phone pushes: RFC 9180 base mode, DHKEM(X25519, HKDF-SHA256),
  HKDF-SHA256, ChaCha20-Poly1305, with a self-describing envelope
  (`version | enc | ct`). It is deliberately separate from the Noise static so
  the two keys never share a protocol or a rotation.

## The shape

```
 machine A                     hub (self-hosted or ours)            machine B
 agents ── nats-server(leaf) ══ TLS ══ nats-server(hub) ══ TLS ══ nats-server(leaf) ── agents
 ck-bus seals body to B's key     stores + forwards ciphertext      ck-bus opens with B's key
```

- **Local messages** stay on the machine's own server, exactly as today.
- **Cross-machine messages** travel A's leaf → hub → B's leaf.
- **During a split**, each machine keeps working locally. Messages addressed to
  another machine are held in a JetStream stream and caught up when the link
  returns (NATS mirroring and sourcing across JetStream domains is store and
  forward). Order is kept per sender; there is no global order across senders.
- **Hub choices**: one of the user's always-on machines, or our hosted hub. A
  clustered hub across the user's own machines is ruled out: JetStream
  clustering needs a majority, so two machines cannot form one, and a split
  stops writes.
- **The hosted hub is the same thing with our server in the roster's hub row.**
  Each user is one account on our cluster, machines dial out to it (so no NAT
  traversal for messaging), and the same store and forward applies.

## End-to-end sealing: the hub carries ciphertext only

Two separate layers, both keyed per machine:

| Layer | Between | Protects against | Source |
| --- | --- | --- | --- |
| Hop encryption | each machine ↔ hub | the network | TLS on the leaf link, stock NATS |
| End-to-end seal | sending machine → receiving machine | **the hub** | sealed body, this design |

If encryption ended at the hub, the hub would read everything, so the seal is
machine to machine. **The recipient is a machine, not an agent.** Every agent
and module on machine B shares B's key; B's ck-bus opens the body and delivers
it over the machine-local bus, which is already inside B's trust boundary. A
message to one agent on B is sealed once. A broadcast to all of a user's
machines is sealed once per machine.

### What gets sealed

The foundation's rule is that a delivery carries an id and a digest while the
body stays in the owning store. **Across machines that rule breaks store and
forward**: resolving the body means calling the sender's machine, which may be
the offline one, so the message could not be read until both machines are up at
the same time. So for any delivery whose recipient is on another machine, the
body travels **inline and sealed**, and the digest is computed over the
plaintext so the recipient can still verify it after opening. Same-machine
deliveries keep the foundation's id-plus-digest shape unchanged.

### Keys

- Each machine holds a **dedicated seal keypair**, the same role as
  `push_seal_pubkey_hex`, never the Noise static and never the Ed25519 identity.
  The private half never leaves the machine (Claustrum on desktops, keychain on
  phones).
- **Sender authenticity.** HPKE base mode proves integrity but not who wrote the
  blob; anyone with the recipient's public key can produce one. For machine to
  machine messages that is not enough, so each sealed body is also **signed by
  the sending machine's Ed25519 device key** (or uses HPKE auth mode with the
  sender's seal key; the choice is open, see Q3). The recipient checks the
  signature against the sender's key from its own pairing ledger.
- **Where a machine learns another's seal key.** This is the part that decides
  whether the hub can read anything. If the hub, or any service we run, could
  supply "B's key", it could supply its own and read everything. So the seal key
  must be **bound to the device identity that the user verified at pairing**: B's
  Ed25519 key signs B's seal public key, and A accepts a seal key only with a
  valid signature by the Ed25519 key in A's local pairing ledger. The registry
  and the hub may relay that signed record; neither can forge it.
- **Removing a machine** (tombstone) removes its seal key from every ledger.
  Senders stop sealing to it at once, so a lost laptop receives nothing new.
  Messages already sealed to it before removal and still held at the hub can be
  dropped by the hub on the tombstone, but not recalled from a copy already
  delivered.

### What the hub can still see

Sealing hides bodies, not the envelope. The hub sees:

- **Subjects.** Today these carry `acct`, `agent_id`, `session_id` and
  `room_id`. They are ids, not content, but an agent name or a room name can be
  meaningful. Q4 asks whether cross-machine subjects should carry keyed hashes
  of those ids instead.
- **Headers**, if any are set. Anything sensitive must move into the sealed body.
- **Sizes, timing and which machine talks to which.**

So a hub operator could learn that the laptop and the desktop exchanged 40
messages at 03:00, but not what they said. That is the same line Signal-style
systems draw.

## Callosum and NATS: who does what

| Concern | Owner |
| --- | --- |
| Which machines belong to the user; their identity, seal keys, pairing, removal | Callosum pairing ledger (built) |
| Telling each leaf where its hub is, and pinning the hub's identity | Callosum `hub_read` (named in the foundation) |
| Direct machine-to-machine calls, LAN and low latency | Callosum fed-wire and dial ladder (built) |
| Carrying and storing messages between machines, catch-up after splits | NATS leaf + hub (new) |
| Getting through NAT for messaging | NATS: machines dial out to the hub |

NATS replaces Callosum's rendezvous and relay role *for messaging only*. The
pairing ledger and device identity are what make the seal trustworthy, and NATS
has nothing that could replace them.

## Setup and the hosted opt-in

- **Default, self-hosted hub**: during pairing the user marks one machine as the
  hub. The ceremony mints that machine's hub account keys and each other
  machine's leaf credential (the foundation already makes the leaf credential a
  consequence of the pairing act). The user is the NATS operator.
- **Hosted hub (opt-in)**: CortexKit is the NATS operator; the user's hub account
  is created on our cluster and its roster row points at our endpoint with our
  pinned identity. Because bodies are sealed to device keys we never hold, the
  hosted hub has the same visibility as a self-hosted one. Switching between the
  two is a roster change plus a leaf re-issue, and must not rename any `{acct}`.

## Open questions for review

1. **CALLO.** Does the pairing verification (the code the user compares) cover the
   Ed25519 device key, so an Ed25519-signed seal key inherits the pairing's
   trust? Where should the seal key record live on the roster, and can desktops
   publish one the way phones publish `push_seal_pubkey_hex`?
2. **CALLO / ALF.** The foundation fixes `{acct}` per box and links box accounts
   to a hub. Is a user one hub account containing several box accounts, or one
   account per box with cross-account exports? This decides how subjects route
   between machines and how a hosted hub isolates users.
3. **CKCRED.** Seal keys and hub account keys in Claustrum: new record kinds, or
   existing SigningKey plus a new KEM kind? Sender authenticity: Ed25519 signature
   over the sealed blob, or HPKE auth mode?
4. **ALF.** Should cross-machine subjects carry keyed hashes of `agent_id`,
   `session_id` and `room_id`, so a hosted hub does not learn agent and room
   names? What would that cost the naming crate and the consumers' filters?
5. **ALF.** The inline-sealed-body rule for cross-machine deliveries: which of the
   five streams can carry a cross-machine delivery, and does any consumer need the
   body to stay out of the stream for retention reasons?
6. **SUBC / CKCRED.** The leaf-signing shape (Rust bridge, memory-backed creds fd,
   or upstream signer callback) is already open in the foundation; a hosted hub
   does not change it, but it needs answering before any leaf arm gates.
7. **All.** What else does the hub, or anyone running it, learn beyond subjects,
   headers, sizes, timing and the machine graph?
