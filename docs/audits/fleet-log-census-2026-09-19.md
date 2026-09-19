# Fleet log census — every producer, every format

Measured 2026-09-19 on this host, against the normative spec
[`docs/specs/fleet-logging.md`](../specs/fleet-logging.md) (operator-approved
2026-09-05). Every line below is a `tail -1` of a live file, not a recollection.

## The one-line answer

**Two producers of nineteen conform. The daemon and synapse.** Everything else
carries one of five other formats, and three of them have no level field at all.

The spec was approved, both crates were built and landed, eleven seats took
`cortexkit-log` as a **dependency** — and one routes output through it.

## One census-method correction, from THALAMUS, the same day

My earlier wave roster used a **manifest**-shaped question (`grep subc-core
Cargo.toml`) and read thalamus as carrying nothing. THALAMUS re-ran it with the
lock-derived test and found they **were** in the population:

```
grep -c '^name = "subc-core"' Cargo.lock    1
subc_core:: in *.rs                          2 sites
```

A git-rev pin made the row read clean: a *version bump* could not reach them, so
"not a path-dep consumer" was true and "not a consumer" was false. **Two
different claims, and the manifest question answered the wrong one.** ASTRO hit
the mirror image — a `[workspace.dependencies]` line no member used, invisible to
cargo and visible to grep, which would have put them in the wave falsely.

The authority is the lock, because it is cargo's own answer to *what does this
build link*. Recorded here because this document is itself a census and the same
error is available to it.

## Format census

Sorted by conformance. `X` = spec violation.

| producer | live sample (truncated) | stamp | level | id |
|---|---|---|---|---|
| **subc** (daemon) | `2026-09-19T07:58:58.970Z INFO  subc module reported non-ok health module_id=engram` | ok | ok | ok |
| **synapse** | `2026-09-18T19:00:18.670Z INFO  synapse tag=perf job done model_id=qwen3-0.6b` | ok | ok | ok |
| wernicke | `2026-09-19T07:26:10.276313Z  INFO ck_wernicke::discord: Discord gateway session resumed` | µs `X` | ok | crate path `X` |
| callosum | `[2026-09-19T07:49:26.374Z DEBUG fed_module::wan_mapping] UPnP-IGD discovery unavailable` | bracketed `X` | ok | crate path `X` |
| aft | `2026-09-19T07:59:14Z [aft] index_event kind=build_ready plane=tier2 root=/Users/…` | sec `X` | **none** | bracketed `X` |
| magic-context (plugin) | `[2026-09-19T04:46:30.522Z] [magic-context][01a0b7fc-…] note-nudge: trigger fired` | bracketed `X` | **none** | bracketed `X` |
| alfonso / ALF (plugin) | `[2026-09-19T07:59:15.651Z] [host-provider] heartbeat ok {"providerId":"…"}` | bracketed `X` | **none** | sub-component `X` |
| magic-context (module) | `mc-pass-timing session=ses_0758f6ce7ffe total=23.1 handler_total=47.1` | **none** | **none** | implicit `X` |
| plexus | `[plexus] reservation retention pruned 1 row(s)` | **none** | **none** | bracketed `X` |
| insula | `[insula] codex reset tick raw_percents=[23.0] credit_count=0 armed=false` | **none** | **none** | bracketed `X` |
| prefrontal-core | `[prefrontal-core] recovery-owner-wake summary elapsed=22ms spec_recovery=22ms/2rows` | **none** | **none** | bracketed `X` |
| prefrontal-routing | `[prefrontal-routing] provider openrouter walled on empty spendable pools` | **none** | **none** | bracketed `X` |
| astrocyte | `[ck-astrocyte] fusiform history boundary discovered at 1786529249396` | **none** | **none** | `ck-` prefix `X` |
| engram | `engram scheduler: capture=HALTED after 3 consecutive failures` | **none** | **none** | bare `X` |
| fusiform | `fusiform: poll Changed { new_version: 1789803498549 }, 33 eras` | **none** | **none** | bare + Rust Debug `X` |
| broca | `broca: catalog override anthropic/claude-sonnet-4-5 limit.context — upstream 1000000, serving 200000` | **none** | **none** | `broca:` prefix `X` |

*Broca's row was first published as "no id either" from a `tail -1` that landed
on the SECOND line of a two-line record. BROCA corrected it and found the real
defect underneath: their prefix rule was satisfied per record while every reader
is per line, so 8 of 29 captured lines were unattributable continuations. Fixed
at broca 73891e6b. A census reading a line-oriented file inherits every
multi-line record as a phantom producer.*
| thalamus | `thalamus attach epoch value-skew (transform ENABLED): profile_epoch mine=2 theirs=3` | **none** | **none** | bare, no delimiter `X` |

*Two producers were added after first publication, both by seats reading the
census and finding themselves absent (ASTRO, THALAMUS). The count is a floor.*

THALAMUS's id shape is worth singling out because **it is the hardest to parse
while looking like the simplest**: a bare `thalamus ` prefix with no delimiter at
all — not `[thalamus]` like plexus/insula, not `thalamus:` like engram/fusiform.
A parser keyed on `[` or `:` extracts nothing; the lines are separable only by
knowing the string in advance.

Seven producers emit lines with **no timestamp of any kind**. Nine have no
level. Six distinct id conventions: bare, bracketed, `ck-` prefixed, Rust crate
path, sub-component name, and absent.

## File locations

### Conforming (spec: `<module data dir>/logs/<module_id>.log`)

```
~/.local/share/cortexkit/synapse/logs/synapse.log          the only one
~/.local/share/cortexkit/run/logs/subc.log                 daemon's own, per spec
```

### Daemon stderr captures — the residue that became the main lane

`run/logs/<module_id>.stderr.log`. The spec says of this file:

> *"A module that logs through the crate writes nothing there, so that file
> being non-empty is itself a finding."*

Applied today, twelve findings:

```
22.3 MB  aft                 1.9 MB  plexus            15.3 KB  insula
15.9 MB  prefrontal-core   421.6 KB  prefrontal-routing  6.1 KB  wernicke
12.9 MB  magic-context     105.9 KB  engram              5.7 KB  broca
                            64.9 KB  callosum            2.9 KB  fusiform
                                                         2.5 KB  astrocyte
```

Rotation is wired (`LineSink` + `Retention`, `stderr_tail.rs:345`), so these are
bounded — they are not a disk risk. They are a **format** risk: the daemon
appends module bytes verbatim, so whatever the module emits is what a reader
gets, and seven modules emit no timestamp.

### Harness-hosted plugin logs in `$TMPDIR` — outside the data dir entirely

```
19.0 MB  $TMPDIR/alfonso.log                            ALF, all harnesses in one file
17.0 MB  $TMPDIR/pi/magic-context/magic-context.log
15.0 MB  $TMPDIR/opencode/magic-context/magic-context.log
11.0 MB  $TMPDIR/omp/magic-context/magic-context.log
 9.3 MB  $TMPDIR/opencode2/magic-context/magic-context.log
324  KB  $TMPDIR/opencode-anthropic-auth.log
```

Spec location is `<module data dir>/logs/<module_id>.<harness>.log`. MC has the
per-harness split right and the **directory** wrong; ALF has neither — one file
for every harness, so `session=` is the only way to tell them apart.

`$TMPDIR` is purged by the OS. These are the logs a user is asked for when
something breaks, and they do not survive a reboot.

### Pid-suffixed files — the shape the spec retired

```
~/.local/share/cortexkit/aft/logs/aft-<pid>.log          many generations
```

The spec calls this a *"read-only compatibility input, never written again after
the cut."* It is still the write path.

## What this costs, concretely

1. **`ck module logs` cannot merge by time.** Its `--since`, `--tag` and
   `--level` filters are specified against fields seven producers do not emit.
   The verb works; the data does not support it.
2. **No level filtering anywhere but two producers.** `CK_LOG` is read by the
   crate. A module not on the crate ignores it, so there is no fleet-wide way to
   raise or lower verbosity.
3. **A grep for one module's lines is unreliable.** `[insula]`, `insula`,
   `[ck-astrocyte]`, `ck_wernicke::discord` — the 2026-09-05 census found tag
   collisions with repository paths, and the same collisions are live.
4. **Plugin logs die on reboot** and are invisible to `ck module logs`, which
   reads the data dir.

## Why adoption stalled, and the trap in fixing it the easy way

Eleven seats link `cortexkit-log`; ten of them still `eprintln!`. Linking is
cheap and routing is a per-seat change nobody is blocked on.

The daemon capture is what removed the pressure. It was specified as a safety
net, it is well-behaved (rotation, retention, one file per module, rendered by
`ck module logs`), and it works **well enough that not adopting has no visible
cost to the module's owner** — the lines land somewhere, in a file the verb
shows.

> **That paragraph is an assertion, not a measurement, and it is the only
> unmeasured claim in this document.** What is measured: eleven seats link the
> crate, one routes through it. Why the other ten have not is a motive I
> attributed to ten owners without asking one of them. It is plausible and it is
> also exactly the kind of explanation that gets carried forward as a fact by
> whoever reads it next — PLEX named this shape the same hour: *an explanation
> offered for a measured result becomes a load-bearing claim the moment the
> reader uses it to predict.*
>
> The discriminator is cheap and I have not run it: **ask the ten seats.** If
> the answer is "the capture is good enough", the recommendation below holds. If
> it is "the crate is missing something I need" — a tag vocabulary, a sink shape,
> an init signature that does not fit a plugin — then the fix is in the crate and
> the adoption wave would have failed for a reason no one wrote down.

That reframes the open request on [#106](https://github.com/cortexkit/subconscious/issues/106):
adding a daemon-side timestamp prefix to capture lines would make the fallback
lane *more* usable and remove the last pressure to adopt. The residue would
become permanent infrastructure, and the finding test in the spec would never
fire again.

## The shape of the fix

Not a new spec. The 2026-09-05 spec is correct and nothing measured here
contradicts it.

1. **Adoption wave.** Ten seats already link the crate; each needs `eprintln!`
   replaced with the crate's macros and `init` with its module id. Per-seat, not
   hard, and it is the only step that changes the format.
2. **The finding test becomes a gate.** `run/logs/<id>.stderr.log` non-empty for
   a module that has adopted is a defect, reportable by `fleet-pulse`.
3. **Plugin lanes move into the data dir** — MC's per-harness split is already
   right, only the directory is wrong. ALF needs the split as well.
4. **Then stamp the capture**, once it is genuinely exceptional, so a stray line
   from a module in trouble carries a time. Doing this step first inverts the
   incentive.

Step 1 has a precondition this document cannot satisfy: **ask the ten linking
seats why they have not routed.** An adoption wave built on my guess about their
motive would fail in whatever way the guess is wrong, and the question costs one
channel post.

Open question for the operator, recorded rather than decided: whether adoption
is mandatory fleet-wide (in which case a seat that declines needs a reason on
the record) or whether the capture lane is an acceptable permanent home for
small modules that emit a handful of lines a day.
