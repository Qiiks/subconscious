# The daemon owns the machine id, and required capabilities are enforced at routing

Status: decided by the operator on 2026-09-23. Two daemon changes, then the
consumers move. This supersedes callosum minting a box id (callosum
`docs/design/n1-box-identity-and-key-records.md` §2.2) and the "required-need
violations are loud reports" line of `docs/specs/capability-grammar.md` r2.

## Why

Two operator concerns, one principle: nothing should depend on a module it
does not need.

- If callosum mints the machine id, prefrontal's local copies and ck-bus's
  per-machine account depend on the cross-machine module, even for a user with
  one machine. The daemon is on every machine and every module already depends
  on it, so the daemon owning the id adds no dependency edge. It is the usual
  layering: the operating system owns `/etc/machine-id`, not the networking
  service.
- There were already three machine identities: prefrontal's local
  `machine_<8 hex>` row (`machine_binding`), callosum's proposed box id, and
  ck-bus's `{acct}`. One owner ends that.
- Optional dependencies double the code paths a feature must keep correct.
  Prefrontal was built to run without entorhinal because nothing enforced a
  required dependency, so optional was the only honest choice. Enforcing
  required dependencies lets a module declare what it needs and delete the
  fallbacks.

## 1. The machine id

**What.** 16 random bytes, rendered as 32 lowercase hex characters. Opaque: no
structure, no derivation from a key, a hostname or any host fact.

**A name, never an authority.** Nothing may admit a peer, grant trust or skip
a check because two messages carry the same machine id. Authority stays on keys
(callosum's roster row, the vault). The id deliberately outlives a key rotation,
so treating it as identity would let a revoked key's history vouch for its
replacement. This rule goes in the doc comment on every type that carries it.

**Where it lives.** `<data home>/cortexkit/machine-id`, one line, written once
through a temporary file and a rename, never rewritten by the daemon. The daemon
mints it at startup if the file is absent, before it accepts a connection, so
every module that registers sees the same value. A present file that does not
parse as 32 lowercase hex characters stops the daemon at boot with a named
error: a corrupt identity is not silently replaced.

**How modules learn it.** A new optional field on the registration reply,
`ModuleHelloAckBody.machine_id: Option<String>` (`serde(default)`, omitted when
absent). Also on `server.describe` and in `ck daemon`. A module built against an
older daemon sees nothing and must treat that as "daemon predates the machine
id", never as "no machine".

**Restore.** Restoring a machine means restoring this file. Two paths:

- the file is part of whatever backs up the data home;
- callosum's trust export records the machine id as data. On import, if the
  recorded id differs from the daemon's, callosum refuses and names the fix:
  `ck machine adopt <id>`, then a daemon restart. `adopt` is an operator
  command, not a module operation, because it changes the identity every module
  on the machine reports; it rewrites the file and takes effect at the next
  daemon start, never under running modules.

**Clone detection stays callosum's.** Two machines restored from one backup
carry one machine id with two different transport keys. Only callosum sees both
keys, so its §2.5 rule stands unchanged, keyed on the daemon's id instead of a
callosum-minted one.

**Phones** run no daemon and have no machine id (callosum r2: a phone never
mints or announces one).

## 2. Required capabilities hold a module not-ready

**Today.** A module declares `capabilities.requires` with `need: required` or
`need: optional`. The daemon evaluates each requirement as `provided`,
`pending` or `never_provided` and reports a missing required provider loudly,
but routing is unaffected.

**New rule.** A module's effective readiness is its declared readiness (rung 2)
AND every one of its `required` capabilities being `provided`. While any is
`pending` or `never_provided`, `route.open` to that module is refused with the
existing retryable `module_warming`, `detail.reason =
"required_capability_unprovided"` and `detail.capability` naming the first
unprovided capability in lexicographic order, a distinct log field, and a
distinct counter key (`module_warming_required_capability_unprovided`).
Callers retry inside their deadline exactly as they do for any warming module,
so no SDK changes.

What it does not do, deliberately:

- **No spawn ordering and no boot block.** The module starts, registers and
  can make its own calls. Ordering is a promise that cannot be kept after a
  provider crashes, and a boot refusal stops a whole machine, including the
  tools needed to fix its configuration.
- **No teardown of existing routes.** When a provider goes away at runtime, new
  opens to the consumer are refused until it returns; routes already bound stay
  bound and the consumer answers them as it can.
- **No deadlock.** A capability counts as `provided` when its claimant has
  registered, not when the claimant is ready. So two modules that require each
  other's capabilities are both provided as soon as both register.

Optional needs are unchanged: silent, and the consumer degrades by declaration.

## 3. Rollout

1. Daemon: the machine id (mint, `HELLO_ACK` field, `server.describe`,
   `ck daemon`, `ck machine adopt`). Golden vector for the new field.
2. Daemon: required-capability readiness. Mutation proofs: removing the check
   lets an open reach a module whose required provider is absent; counting
   readiness instead of registration for "provided" deadlocks a mutual pair.
3. entorhinal (mine) declares `provides` for its project-identity surface
   (`resolve`, `resolve_project_id`, `enumerate`, register), names settled when
   the change is written.
4. prefrontal (ALF) declares entorhinal's capability `required`, deletes the
   no-entorhinal paths once the daemon from step 2 is deployed, and moves its
   `machine_binding` local row to the daemon's id.
5. callosum (CALLO) reads the daemon's id instead of minting one; announce and
   clone detection unchanged; trust export records the id and import refuses on
   mismatch with the `adopt` instruction.
6. ck-bus: `{acct}` derives from the daemon's machine id (spec amendment).
7. Other modules that depend on entorhinal declare it the same way (broca asked).
