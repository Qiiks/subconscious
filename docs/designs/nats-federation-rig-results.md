# NATS federation rig: measured results against stock nats-server v2.15.0

Status: measured 2026-09-24 on darwin with `/opt/homebrew/bin/nats-server`
(`nats-server: v2.15.0`), stock binary, documented configuration only. This
answers the "Remaining checks" rig arm and the `SignatureCB` check in
`docs/designs/nats-federation.md` (r3).

Rig: `crates/ck-bus/tests/nats_federation_rig.rs`, six `#[ignore]`d tests, one
per behaviour. Run:

```
cargo test -p ck-bus --test nats_federation_rig -- --ignored --nocapture
```

A missing binary fails every test with `NATS RIG NOT RUN: no nats-server
binary ...` rather than passing (checked with `PATH=/usr/bin:/bin` and with a
bogus `NATS_SERVER_BIN`).

## Summary

| # | Behaviour | Design assumption holds? |
| --- | --- | --- |
| 1 | Leaf routing from per-box federation accounts into one per-user hub account | **Holds** |
| 2 | A box's local-account subjects never cross the leaf, even under hub `>` | **Holds** |
| 3 | Store and forward across a split, exactly once, in order | **Holds for a recipient split. Does not hold for a sender split unless the sender has a local outbox stream** (added requirement) |
| 4 | Subject-filtered purge of one recipient's inbox on the hub | **Holds** |
| 5 | `SignatureCB` in v2.15.0 | **Exists, Go API only**; stock config needs key material readable at every (re)connect |
| 6 | Revoking one user by account-JWT revocation | **Holds**, for client users and for a leaf connection |

Plus one finding the design did not anticipate: **the hub advertises its own
leaf address to every leaf, and leafs redial that address** (see
"Finding: leafs dial the hub's advertised address").

## Common configuration

- Three `nats-server` processes on loopback with free ports, one temp directory
  per test: `hub`, `box-a`, `box-b`.
- **JWT mode everywhere.** `nsc` is not installed, and the rig does not use the
  `nats` CLI. Operator, account and user keys are generated in the test with the
  `nkeys` crate (0.4.5), and JWTs are assembled in the nats-io/jwt v2 format
  (`{"typ":"JWT","alg":"ed25519-nkey"}`, claims, ed25519 signature by the
  issuer). Every account limit is set explicitly (`conn`, `leaf`, `subs`, ... =
  -1; JetStream accounts also `mem_storage`, `disk_storage`, `streams`,
  `consumer` = -1), because a missing limit decodes as 0 and the server
  enforces it as "none allowed".
- **Hub**: its own operator; system account; one per-user account `user-u` with
  JetStream; `resolver { type: full, dir: ..., allow_delete: false }` with
  `resolver_preload`; JetStream domain `hub`;
  `leafnodes { listen: 127.0.0.1:P, no_advertise: true }`.
- **Each box**: its own operator (the hub does not trust it); system account;
  `box-X-local` and `box-X-fed` accounts, both with JetStream;
  `resolver: MEMORY` with preload; JetStream domain `a` or `b`; one leaf remote:

  ```
  leafnodes { remotes: [ { url: "nats-leaf://127.0.0.1:<proxy>",
                           credentials: "<dir>/leaf-X.creds",
                           account: "<box-X-fed public key>" } ] }
  ```

  The creds file holds a user JWT issued by the hub's `user-u` account. Only the
  FED account is bound to the leaf; the LOCAL account has no remote.
- Each leaf link runs through an in-test TCP relay, so a split is cut and
  restored without touching either server ("cut" drops every relayed
  connection and accepts-then-closes new ones until "restore").
- Leaf users carry no publish/subscribe permission restrictions; the design's
  per-leaf grant (subscribe own inbox only, publish other inboxes only) was not
  part of this rig.

## 1. Leaf routing into a per-user hub account: holds

Configuration: as above. B's FED client subscribes `ck.b.peer.>`; A's FED client
publishes `ck.b.peer.agent_rig1`.

Observed:

```
[rig 1] hub /leafz: leaf box-a bound to hub account ADVY4YOMKXUAM4RUMQT7SSLD3MALNEMUX5HGH2VYPBM6KYV3TEMX7AS3
[rig 1] hub /leafz: leaf box-b bound to hub account ADVY4YOMKXUAM4RUMQT7SSLD3MALNEMUX5HGH2VYPBM6KYV3TEMX7AS3
[rig 1] A FED -> B FED on ck.b.peer.agent_rig1 arrived after 2 publish attempt(s)
[rig 1] steady-state messages at B: ["steady-0", "steady-1", "steady-2", "steady-3", "steady-4"]
hub log: [INF] 127.0.0.1:50604 - lid:5 - Leafnode connection created
box log: [INF] ... Leafnode connection created for account: AD3D...LEDP/box-a-fed
```

Two leafs from two different local operators bind into one hub account, and a
FED-to-FED message crosses A → hub → B. The first publish is lost while B's
subscription interest propagates across two leaf hops (about 200 ms here);
core NATS gives no delivery guarantee, which is why the cross-machine path must
be JetStream (behaviour 3), not core publish.

## 2. Local subjects stay off the leaf: holds

Configuration: hub client in `user-u` subscribes `>`; B FED and B LOCAL
subscribe `>`. A's LOCAL client publishes 20 each of `ck.a.peer.agent_local`,
`ck.b.peer.agent_local` and `ck.a.room.rm_local` (the second one matches B's
inbox family). A positive control from A's FED account is published before and
after, so a silent subscriber is proven live.

Observed:

```
[rig 2] control: A FED publish reached hub `>` after 1 attempt(s)
[rig 2] after 60 LOCAL publishes on A: hub `>` saw 1 message(s) (0 LOCAL), B FED `>` saw 0 LOCAL, B LOCAL `>` saw 0 LOCAL
[rig 2] reverse: hub publish reached A FED after 2 attempt(s); A LOCAL `>` saw 0 message(s)
```

The one hub message is the trailing FED control. The reverse direction holds
too: a hub publish on `ck.a.peer.agent_local` reaches A's FED account and never
A's LOCAL account. The separate federation account does what the design
(Athena) asked of it.

## 3. Stream sourcing across a split

The design (§"The shape") says cross-machine messages "travel A's leaf → hub →
B's leaf and are held in JetStream through a split, then caught up (mirroring
and sourcing across JetStream domains)", and that A publishes `ck.{b}.peer.…`.
The rig follows that:

- **Hub** (`user-u`, domain `hub`): stream `CK_HUB_PEER`, subjects `ck.*.peer.>`,
  file storage, discard new, max-age 3 days. It also sources A's outbox (below)
  with a subject transform `ckout.a.>` → `ck.>` over `$JS.a.API`.
- **B** (`box-b-fed`, domain `b`): stream `CK_B_INBOX` with **no subjects of its
  own**, one source: `CK_HUB_PEER`, filter `ck.b.peer.>`, external API
  `$JS.hub.API`. Binding no subjects means a message reaches B's inbox by one
  path only.
- **A** (`box-a-fed`, domain `a`): stream `CK_A_OUTBOX`, subjects `ckout.a.>`
  (sender-namespaced, so no other stream's interest overlaps it).

All publishes use JetStream publish with ack. Observed (full run):

```
[rig 3] connected: m01 acked stream=CK_HUB_PEER seq=2 domain=hub           (m02, m03 likewise)
[rig 3] split: B's leaf link cut; hub /leafz shows only box-a
[rig 3] during B split: m04 acked stream=CK_HUB_PEER seq=5 domain=hub      (... m08 seq=9)
[rig 3] B inbox during split holds 3 message(s)
[rig 3] link restored: B caught up to 8 in 1.033057875s
[rig 3] while B's server is stopped: m09 acked stream=CK_HUB_PEER seq=10 domain=hub
[rig 3] while B's server is stopped: m10 acked stream=CK_HUB_PEER seq=11 domain=hub
[rig 3] B restarted: caught up to 10 in 104.309292ms
[rig 3] A split, direct publish to ck.b.peer.agent_rig3: Err("ack: no stream found for given subject") (no stream on A binds it)
[rig 3] connected: o00 into A outbox acked stream=CK_A_OUTBOX seq=1 domain=a
[rig 3] during A split: o01 into A outbox acked stream=CK_A_OUTBOX seq=2 domain=a   (o02, o03 likewise)
[rig 3] A restored: B caught up to 14 in 1.03337275s
[rig 3] B inbox seq 1: ck.b.peer.agent_rig3 m01
...
[rig 3] B inbox seq 10: ck.b.peer.agent_rig3 m10
[rig 3] B inbox seq 11: ck.b.peer.agent_rig3 o00
...
[rig 3] B inbox seq 14: ck.b.peer.agent_rig3 o03
[rig 3] hub stream holds 15 message(s); `lost-11` present: false
box-b log: [INF] ... Leafnode connection closed: Server Shutdown - Remote: hub
box-b log: [INF] ... Leafnode connection created for account: AB3V...IK75/box-b-fed
```

(`async-nats` renders a 503 no-responders reply to a JetStream publish as "no
stream found for given subject", `jetstream/context.rs` `next_with_timeout`.)

After a 3 s settle the test reads B's inbox back and asserts it equals
`m01..m10, o00..o03` exactly: every message once, in publish order.

What holds:

- **Recipient split, link cut** (both servers up): the hub accepted and
  acknowledged m04..m08 while B was unreachable; B caught up within about 1 s of
  the link returning, no duplicates, in order.
- **Recipient split, B's server stopped (SIGTERM) and restarted from its store**:
  the source cursor survived the restart; m09, m10 arrived once, in order.
- **Sender split through a local outbox**: o01..o03 were acknowledged by A's own
  domain while A was cut off, then sourced by the hub (with the subject
  transform) and on to B, once, in order.

What does NOT hold, and what the design needs instead:

- **A sender split with the design's direct publish loses the message.** When A
  publishes `ck.b.peer.…` while its leaf is down, no stream anywhere binds the
  subject from A's side: the publish gets no responder (`lost-11` above) and the
  message is never stored. A caller that treats that error as retryable would
  hold it; ck-bus as sketched does not say so. The split rule "late is fine,
  loss is not" therefore needs **a sender-side outbox stream in the FED
  account**, sourced by the hub, exactly as phase D measures. Two constraints
  observed while building it:
  - The outbox subject must be sender-namespaced (the rig used `ckout.{sender}.>`,
    rewritten to `ck.>` by the hub's source transform). If each box's outbox
    bound `ck.*.peer.>`, the stream subscriptions would pull other boxes'
    publishes across the leaf into every outbox, and the hub would source each
    message more than once. (Reasoned from how stream interest propagates over
    leafs; the rig did not build the overlapping variant.)
  - The recipient's inbox must bind no subjects and only source from the hub;
    otherwise a live message arrives twice (once by subject interest, once by
    sourcing). The design text "B's local stream binding catches it unchanged"
    should read "B's inbox sources its filter from the hub stream".
- Not measured: splits longer than JetStream's 2-minute duplicate window, or
  larger than the stream limits. The design's recipient-side dedupe on delivery
  id still stands as the authority there.

## 4. Subject-filtered purge of one recipient's inbox: holds

Configuration: hub stream `CK_HUB_PEER` (`ck.*.peer.>`) with 12 messages
round-robin to recipients b, c, d. Purge with filter `ck.b.peer.>`.

```
[rig 4] before purge, messages per recipient: {"b": 4, "c": 4, "d": 4}
[rig 4] purge filter ck.b.peer.> -> success=true purged=4
[rig 4] after purge, messages per recipient: {"c": 4, "d": 4}
[rig 4] remaining seq:payload: ["2:c-1", "3:d-2", "5:c-4", "6:d-5", "8:c-7", "9:d-8", "11:c-10", "12:d-11"]
```

Only b's messages go; c and d keep their sequence numbers and payloads.

## 5. `SignatureCB` in v2.15.0: exists, Go API only

Source, tag `v2.15.0` of `github.com/nats-io/nats-server`:

- `server/opts.go` lines 249-262: `type SignatureHandler func([]byte) (string, []byte, error)`
  ("used to sign a nonce from the server while authenticating with Nkeys ...
  return the JWT and the raw signature") and `RemoteLeafOpts.SignatureCB
  SignatureHandler` tagged `json:"-"`.
- `server/leafnode.go` lines 1098-1112: "If a signature callback is specified,
  this takes precedence over anything else" — the server calls `cb(nonce)` on
  every leaf connect and puts the returned JWT and signature in CONNECT.
- `server/opts.go` `parseRemoteLeafNodes` (about lines 3099-3270): the config
  file accepts only `creds`/`credentials` (a path) or `nkey`/`seed` (an inline
  seed) for key material; any other key is an `unknownConfigFieldErr`
  (line 3264).
- `server/leafnode.go` lines 1114-1140: the creds file is read with
  `os.ReadFile` on **every** connect and reconnect, then wiped from memory.

Measured on the binary:

```
[rig 5] binary: /opt/homebrew/bin/nats-server reports `nats-server: v2.15.0`
[rig 5] config remote key `signature_cb`: exit=Some(1) output=nats-server: .../signature_cb.conf:2:58: unknown field "signature_cb"
[rig 5] config remote key `signature`: exit=Some(1) ... unknown field "signature"
[rig 5] config remote key `sign_callback`: exit=Some(1) ... unknown field "sign_callback"
[rig 5] A connected; its creds file is now deleted; cutting A's link
[rig 5] 4s after restoring the link with no creds file, hub leafs: ["box-b"]
box-a log: [ERR] 127.0.0.1:51075 - lid:7 - open .../leaf-a.creds: no such file or directory
[rig 5] creds file written back: leaf reconnected in 108.006125ms
```

So the design's statement is right: `SignatureCB` is in the pinned version and
reachable only by embedding the server in a Go program (shape 3). A stock
server leaf cannot authenticate without the key readable at every (re)connect;
deleting the file after the first connect breaks the next reconnect. Stock
options for keeping a long-lived key off disk:

- **Shape 1, Rust bridge**: `async-nats` 0.50 has
  `ConnectOptions::with_jwt(jwt, sign_cb)` (`src/options.rs` line 354); Go
  clients have `nats.UserJWT(userCB, sigCB)` (`nats.go` v1.51.0, the version
  v2.15.0's `go.mod` pins, line 1450). Both sign the nonce through a callback,
  so the seed can stay in the vault. Not measured here.
- **Shape 2, ephemeral creds**: the file must exist at every reconnect (measured
  above), so it has to live for the life of the leaf, not only at start.
- The leaf remote also sends a URL's user/password or token in CONNECT
  (`leafnode.go` around line 1158, commented "to allow auth callout"), so a
  hub-side auth callout is a further stock option. Not measured.

## 6. Revoking one user: holds

Configuration: hub with the `full` resolver. Two client users u1, u2 in
`user-u`, both connected. The account JWT is re-signed with `revocations:
{<u1 public key>: now}` and pushed as a request on `$SYS.REQ.CLAIMS.UPDATE`
from the hub's system-account user. Then the same for box A's leaf user.

```
[rig 6] before revocation: u2 received [("rev.check", "before")] from u1
[rig 6] revoke u1: $SYS.REQ.CLAIMS.UPDATE reply {... "data":{"account":"AB5V...","code":200,"message":"jwt updated"}}
[rig 6] u1 client events: ["connected", "server error: nats: User Authentication Revoked", "disconnected", "client error: nats: authorization violation", ...]
[rig 6] u1 connection state: Disconnected
[rig 6] fresh connect as u1: Some("authorization violation: nats: authorization violation")
[rig 6] u2 after u1's revocation: existing connection received [("rev.check", "after")], fresh connect ok=true
[rig 6] revoke leaf-a: $SYS.REQ.CLAIMS.UPDATE reply {... "code":200,"message":"jwt updated"}
[rig 6] hub /leafz shows only box-b 667.625µs after the update
[rig 6] 4s later hub leafs: ["box-b"]
[rig 6] box-b still receives hub traffic (after 2 attempt(s))
hub log:   [INF] 127.0.0.1:50629 - lid:5 - Leafnode connection closed: Credentials Revoked - Remote: box-a
box-a log: [ERR] 127.0.0.1:50621 - lid:6 - Leafnode Error 'User Authentication Revoked'
box-a log: [ERR] 127.0.0.1:50621 - lid:7 - Leafnode Error 'Authorization Violation'   (repeats every ~1 s)
```

The server closes the revoked user's existing connection immediately (for a
client and for a leaf), refuses every reconnect, and leaves the other users in
the same account, including the other machine's leaf, untouched. This is what
Design D and the removal ceremony need; the ceremony has to push the updated
account JWT, as the design already says. Note for the ceremony: revocation is
by issue time (`iat` at or before the revocation timestamp), so a new leaf
credential for the same key must be issued after that timestamp.

## Finding: leafs dial the hub's advertised address

The first run of behaviour 3 failed: with B's proxied link cut and hub `/leafz`
showing only box-a, B's inbox still received m04..m08. Cause (confirmed in
`server/leafnode.go` v2.15.0, lines 1045-1047 and 1852-1870): unless
`leafnodes { no_advertise: true }`, the hub puts its own leaf listen address in
the INFO it sends each leaf, and the leaf adds that address to its remote's URL
list. B reconnected directly to the hub's real port, bypassing the relay. The
rig now sets `no_advertise: true` on the hub.

For the design: a leaf does not dial only the address `callosum.hub_read` gave
it; it also dials whatever the hub advertises. A self-hosted hub behind NAT or a
port forward will advertise an address its peers may not reach (harmless but
noisy), and a hub's advertised address is attacker-influenced input. The hub
config should set `no_advertise: true` (or an explicit `advertise` address), and
hub pinning must rest on TLS verification of every leaf dial, not on the URL.

## Full rig output summary

```
test rig1_leaf_routing_into_per_user_hub_account ... ok
test rig6_revoking_one_user ... ok
test rig3_stream_sourcing_across_a_split ... ok
test rig4_subject_filtered_purge_of_one_inbox ... ok
test rig5_leaf_signature_callback_and_key_on_disk ... ok
test rig2_local_subjects_stay_off_the_leaf ... ok
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 31.07s
```
