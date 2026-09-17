# global-dispatch/v1

Review artifact for cerebellum's global-dispatch plane: pointer capabilities
(`computer.move`, `computer.drag`, `computer.scroll`) delivered through the
global HID event stream rather than per-process posting. Minted by subc after
an evidence review of the packet run over the wire on the macOS alpha VM
against `ck-cerebellum` at commit `1e67568552c66eac6d9ec15aa151a4ca5de91430`
(origin/main, gate green, 1210 tests). Cerebellum binds its release-build
review revision to this revision by name.

This is the "new revision" the synthetic-input review said a pointer delivery
path would require: a new delivery mechanism is a new containment story, and
the story here is different in kind — a global stream has no per-pid rung, so
containment is `global_none` / tier 4 and the review's weight rests entirely
on the gate that decides whether a person is at the machine.

## What the review read (measured, not asserted)

1. **Rung and tier on the wire and in the journal**: `global_none` / tier 4
   for move, drag and scroll. Wire v8 omits `containment_rung` on move/drag
   and declares it on scroll; the journal carries it on all three.
2. **Focus TOCTOU**: with Finder fronted, `target_focus_mismatch` on the wire,
   journal `abort.reason=focus_mismatch`, no post delivered. Positive control
   with TextEdit fronted completes.
3. **Idle, both polarities**: idle after 2 s of no input completes; idle after
   a person's input refuses `not_idle` at `hid_idle_ms` 1 and 9 across two
   hands runs.
4. **Seize mid-sequence, by count not by age**: a second process (a child of
   the probe, its own pid) posts one mouse-move 20 ms and 70 ms after the
   module's heartbeat; both stop the drag at the next post with
   `sourced_input_kind=other_process{pid=<child>}` and detection latency 0 ms
   (event timestamp to drain). A poke 150 ms after the heartbeat lands in the
   tail and the sequence completes. Earlier hands runs produced the same stop
   with `source=hardware` (3 of 5 rounds), so both witnesses are on the record.
5. **Latency bound**: worst loop share 0 ms against a 50 ms bound.
6. **The ordering mutation, red by name** (the one mutation that passes every
   other assertion): with `create_tap` moved after `read_idle` in
   `begin_observed_sequence`, `dispatch::seize::tests::tap_is_created_before_the_idle_read`
   fails with `idle ran before tap create: ["idle", "tap", "heartbeat", "post"]`
   while the 22 others — including `a_non_self_event_inside_the_window_aborts`
   — stay green, because the in-window control's fake tap delivers whatever was
   queued regardless of when the tap was created. That is why the ordering
   test exists, and why its red line was required rather than its existence.

## Why the gate reads counts and not ages

The HID idle table is an aggregate age with no attribution. A gate that read
"the newest event is ours within N ms" was measured (2026-09-17) to read the
age of the module's OWN posts — 328 ms was the previous action's session-tap
post; 7–84 ms at first dispatch was the heartbeat marker — so a `move` 94 ms
after a completed move was refused `not_idle`, and move-then-drag, the normal
shape of pointer use, could never pass. Two holes in the age design, both in
the dangerous direction: a hardware event between the idle read and the
heartbeat is masked by the module's own later post; a hardware event within
the tolerance either side of a post is indistinguishable from it by timing.

The shipped gate: tap created FIRST, then idle read, heartbeat, focus, posts;
every non-self event from the idle read until 100 ms after the last post is
counted (in-sequence it aborts; in the tail it clears the carried idle); the
user idle is carried across sequences only on a zero count; at the next start
a raw reading younger than (now − last_post − 100 ms) means a hardware event
landed after the tail and the raw reading stands. The 100 ms is a bound on how
late the module's own event shows in the table, never a bound on how close a
human is allowed to be.

## Defects the packet found and fixed (each with a test that reds on the old shape)

1. The gate read the age of the module's own posts (above).
2. The idle estimator was owned by the per-dispatch driver, so it could never
   carry across sequences — a correct component with no reachable lifetime.
   Now one estimator per plane lifetime, shared into each driver.
3. `frontmost_process_id` was osascript at 170–260 ms per post. Replaced by
   the on-screen window list at 0.7 ms; NSWorkspace (31 µs, stale without a
   run loop) and AX system-wide (CannotComplete) were measured under the
   daemon's attribution and rejected.
4. The loop slept the interval and drained after, so a seize just after a
   post waited the whole interval. Now waits on the tap and wakes on the event.

## Scope and boundaries

- Covers `computer.move`, `computer.drag`, `computer.scroll` on macOS through
  the global stream. Per-process keyboard delivery stays under
  `synthetic-input-review/v1`.
- Windows has no per-process input targeting and operates semantic-first at
  tier 4 (banked 2026-09-01); this review's evidence is macOS-only and a
  Windows delivery path is a new revision.
- Containment is `global_none` by construction: nothing in this plane can
  claim a per-pid rung, and the journal must never carry one for these ops.

## Revision-advance policy

v2 is required when: the idle gate's discriminator changes mechanism (count →
anything else); a delivery path other than the global HID stream is added; a
platform other than macOS is served; or evidence shows a seize that the count
did not catch (a hardware event inside the window that did not abort), which
is a taxonomy change and not a tuning change.
