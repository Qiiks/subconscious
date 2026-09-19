#!/usr/bin/env bash
#
# EVERY REFUSAL PRINTS THE SAME PREFIX: "REFUSED: ". There were two vocabularies
# until 2026-09-18 -- lowercase "refusal:" from argument validation, uppercase
# "REFUSED:" from the gate arms -- and a caller filtering output with
# `grep -E "...|REFUS"` saw NOTHING when a path was wrong, because the arg-
# validation path used the other spelling. An empty filter result reads as
# quiet success, so a real refusal became invisible at the exact moment the
# operator most needed it.
#
# THE RULE THIS ENCODES: the party that knows the outcome must NAME it, because
# every downstream filter is guessing at a vocabulary. A caller's grep is an
# allow-list over outcomes and inherits the allow-list defect -- the outcome
# nobody anticipated is the one that goes silent. Corollary for readers: prefer
# a position-based view (`tail`) over a pattern-based one for verdicts, since a
# verdict you failed to predict still occupies the last line.

# Place a staged module binary with every gate arm that has caught a real defect.
#
# Each arm exists because it failed once, and each failure was a TRUE statement about
# the wrong object rather than a missing check:
#   which          a --version read from the file you placed is true about that file,
#                  while PATH resolves an older one the operator actually runs
#                  (ck-models, 2026-09-14: new CLI on disk at a path nothing resolves).
#   sidecar verify a rollback nobody can verify is not a rollback, and an incident is
#                  the wrong moment to find the sidecar was written for another file.
#   marker + control  a discriminator that reads 0/0 proves nothing (it may have been
#                  dead-code-eliminated); a control that reads 1/1 proves the reader works.
#   inode          proc-vs-disk is the only proof the restarted process runs these bytes;
#                  `cp` in place preserves the inode and makes the check a tautology,
#                  so placement is always copy-to-tmp then atomic mv.
#   warm-exec      macOS first-exec assessment is per-inode and does not transfer from
#                  the staging path, so it must run on the destination before the restart.
#
# Usage:
#   place-module.sh --module <id> --staged <path> [--dest <path>] [--path-face <name>]
#                   --marker <string> [--control <string>] [--no-restart]
#
# Refuses (exit 2) before touching the destination if any pre-arm fails.
#
# MUTATION REQUIRES --place. The default runs every arm and stops before the first
# side effect, because a tool that places by DEFAULT is one distracted invocation
# from placing when you meant to test it -- which happened on 2026-09-15. The
# default should be the one that is safe to be wrong about.
set -euo pipefail

STAGING="${CK_STAGING:-$HOME/.local/share/cortexkit/staging}"
BIN_DIR="${CK_BIN_DIR:-$HOME/.local/share/cortexkit/bin}"
MODULE=""; STAGED=""; DEST=""; PATH_FACE=""; MARKER=""; CONTROL=""; GONE=""; RESTART=1; PLACE=0; OLDER=0; MIGRATES=""

while (($# > 0)); do
  case "$1" in
    --module) MODULE="$2"; shift 2 ;;
    --staged) STAGED="$2"; shift 2 ;;
    --dest) DEST="$2"; shift 2 ;;
    --path-face) PATH_FACE="$2"; shift 2 ;;
    --marker) MARKER="$2"; shift 2 ;;
    --control) CONTROL="$2"; shift 2 ;;
    --gone) GONE="$2"; shift 2 ;;
    --place) PLACE=1; shift ;;
    --older) OLDER=1; shift ;;
    # A card that MIGRATES THE STORE cannot be rolled back by binary alone: the
    # old binary meets a newer schema and refuses on store_ahead, which is the
    # correct fail-closed behaviour and also means the binary snapshot restores
    # nothing. Naming the store here snapshots it too, so the rollback is
    # BINARY + STORE. Raised by FUSI before a v5->v6 placement, after this script
    # had printed "rollback ... verified" on every migrating card it ever placed.
    --migrates) MIGRATES="$2"; shift 2 ;;
    --check-only) shift ;;  # now the default; accepted so older call sites keep working
    --no-restart) RESTART=0; shift ;;
    *) echo "REFUSED: unknown argument '$1'" >&2; exit 2 ;;
  esac
done

[ -n "$MODULE" ] || { echo "REFUSED: --module is required" >&2; exit 2; }
[ -n "$STAGED" ] || { echo "REFUSED: --staged is required" >&2; exit 2; }
[ -n "$MARKER" ] || { echo "REFUSED: --marker is required (a discriminator that separates this build from the running one); pass --marker none for a change that adds no literal" >&2; exit 2; }

# `--marker none` IS FOR A CHANGE THAT ADDS NO STRING, and it is honest rather
# than a bypass. A deletion-only fix, a removed call, a private fn that inlines
# away, or a type-level refactor leaves every literal identical: `strings` is
# then correct to report no difference, and demanding a marker would force the
# operator to invent one.
#
# IDENTITY STILL HOLDS WITHOUT IT, because the marker was never the only link:
#
#   staged sidecar verifies        the staged file is the digest the owner published
#   placed sha == staged sha       what landed is what was gated
#   running inode == disk inode    the kernel mapped that file
#
# That chain is complete on its own. What the marker adds is a second, WEAKER
# statement -- that an expected literal is compiled in -- which never proved the
# branch was reachable anyway. So its absence costs less than it appears to.
#
# What IS lost is the cross-check that the staged bytes differ from the running
# ones in the way the card claims, so the gate substitutes the strongest
# available: LC_UUID must differ. A rebuild always moves it, and two files with
# the same UUID are the same build.
#
# Raised by ENGRAM (2026-09-19) on a fix that deletes one call and adds a
# comment. They stated "no markers by strings" on the card rather than reaching
# for a literal that would have read 1/1 and looked like a passing arm.
DEST="${DEST:-$BIN_DIR/ck-$MODULE}"
[ -f "$STAGED" ] || { echo "REFUSED: staged artifact not found: $STAGED" >&2; exit 2; }
[ -f "$DEST" ] || { echo "REFUSED: destination does not exist, so this is an install rather than a placement: $DEST" >&2; exit 2; }

say() { printf '%s\n' "$*"; }
refuse() { printf 'REFUSED: %s\n' "$*" >&2; exit 2; }

say "=== gate"

# Staged sidecar, verified the way a consumer verifies it.
staged_dir=$(cd "$(dirname "$STAGED")" && pwd); staged_base=$(basename "$STAGED")
sidecar=""
for cand in "$staged_base.sha256.postsign" "$staged_base.sha256"; do
  [ -f "$staged_dir/$cand" ] && { sidecar="$cand"; break; }
done
[ -n "$sidecar" ] || refuse "no sidecar beside the staged artifact (bare-binary staging directories are refused)"
(cd "$staged_dir" && shasum -c "$sidecar" >/dev/null 2>&1) || refuse "staged sidecar does not verify its own artifact: $sidecar"
say "staged sidecar $sidecar: OK"

# ---- CURRENCY: is this the artifact its OWNER says is live? ----
#
# TWO MECHANISMS, AND ONE OF THEM IS AUTHORITATIVE.
#
#   ck-<module>.current   a manifest the module's owner writes when they hand
#                         over a card: "<sha256>  <filename>  <UTC stamp>".
#                         It STATES a fact. Authoritative.
#   newest-by-mtime       this gate INFERS from file ordering. A fallback, and
#                         a poor one -- the directory accumulates leftovers, so
#                         the newest file may be stale too.
#
# They agreed the first night the manifest existed (broca 0.3.98). THAT
# AGREEMENT IS EXACTLY WHEN TO DECIDE WHICH ONE WINS, rather than treating
# concord as validation and discovering the ordering at the moment they
# disagree -- which would be a placement, with an operator waiting.
#
# So: manifest present -> it decides, and the mtime arm is not consulted at all.
# Manifest absent -> mtime, and the line SAYS it is inferring, because a
# fallback that reads like a verdict is how a guess becomes a fact.
#
# Measured 2026-09-18, which is why any of this exists: 84 staged binaries across
# all modules, 26 for broca alone with verifying sidecars, every one passing this
# gate's only placeability test. A broca card was superseded upstream hours after
# it was staged and gated, and I learned it from its owner rather than from here.
manifest="$staged_dir/ck-$MODULE.current"
staged_base=$(basename "$STAGED")
if [ -f "$manifest" ]; then
  want_sha=$(awk 'NR==1{print $1}' "$manifest")
  want_file=$(awk 'NR==1{print $2}' "$manifest")
  have_sha=$(shasum -a256 "$STAGED" | awk '{print $1}')
  if [ "$have_sha" = "$want_sha" ]; then
    # Name matching is secondary: the sha is the identity. A renamed copy of the
    # current bytes is the current artifact.
    say "currency: matches ck-$MODULE.current (owner-declared), sha ${have_sha:0:16}"
  elif [ "$OLDER" -eq 1 ]; then
    say "currency: placing $staged_base although the owner declares $want_file current (--older given)"
  else
    echo "REFUSED: $staged_base is not what ck-$MODULE.current declares" >&2
    echo "         owner declares: $want_file  ${want_sha:0:16}" >&2
    echo "         you passed:     $staged_base  ${have_sha:0:16}" >&2
    echo "         The manifest is the module owner's statement of which artifact is" >&2
    echo "         live. If this is deliberate (a rollback), pass --older." >&2
    exit 2
  fi
else
  # `|| true` IS LOAD-BEARING AND WAS MISSING. Under `set -euo pipefail`, a grep
  # that matches nothing exits 1, pipefail propagates it to the assignment, and
  # set -e KILLS THE SCRIPT -- after the sidecar line and before any other arm.
  # The operator sees one line of output and a script that stopped, which reads
  # like a gate that finished rather than one that died.
  #
  # It fires whenever a staging directory holds no `ck-<module>.<hex>` artifact
  # -- which is every FUSI card, because they name theirs plainly `ck-fusiform`
  # inside a timestamped directory. A NAMING CONVENTION THIS GATE INVENTED,
  # silently refusing every artifact that does not follow it.
  #
  # Found 2026-09-19 by running the gate on a card and getting two lines back,
  # then `bash -x` rather than assuming the run was fine. My own check reported
  # `exit=0` because I read `$?` through a pipe and got `tail`'s status -- the
  # exit-code trap from the same evening, inside the verification of the tool
  # that catches it.
  newest=$(ls -t "$staged_dir" 2>/dev/null \
    | grep -E "^(SIGNED\.)?ck-$MODULE\.[0-9a-f]+$" \
    | head -1) || true
  if [ -n "$newest" ] && [ "$newest" != "$staged_base" ]; then
    if [ "$OLDER" -eq 1 ]; then
      say "currency: INFERRED from mtime (no ck-$MODULE.current); placing $staged_base although $newest is newer (--older given)"
    else
      echo "REFUSED: $staged_base is not the newest staged artifact for $MODULE" >&2
      echo "         newer: $newest" >&2
      echo "         INFERRED FROM MTIME -- there is no ck-$MODULE.current manifest, so" >&2
      echo "         this gate is GUESSING from file ordering. The named file may be" >&2
      echo "         stale too; it is a fact about timestamps, not a recommendation." >&2
      echo "         Ask the module owner to write ck-$MODULE.current, or pass --older." >&2
      exit 2
    fi
  else
    say "currency: INFERRED from mtime (no ck-$MODULE.current); $staged_base is newest"
  fi
fi

# Signing posture must match the running image: an ad-hoc re-sign of a Developer ID
# binary silently revokes its macOS TCC grants, and the reverse is a surprise too.
if command -v codesign >/dev/null; then
  staged_sig=$(codesign -dvv "$STAGED" 2>&1 | grep -E '^(Signature|Authority)=' | head -1 || true)
  live_sig=$(codesign -dvv "$DEST" 2>&1 | grep -E '^(Signature|Authority)=' | head -1 || true)
  [ "$staged_sig" = "$live_sig" ] || refuse "signing posture differs: staged [$staged_sig] vs running [$live_sig]"
  say "signing posture: $staged_sig (matches running)"
fi

# Marker differential. A marker that reads 0 on the staged file proves nothing about
# this build; a control that does not read on both proves the reader is broken.
#
# BOTH TABLES ARE READ, and the table is named in every line, because A MARKER COUNT
# IS SILENTLY READER-RELATIVE WITHOUT IT. A control-flow-only change adds no string
# literal, so `strings` cannot see it at all and reports a truthful 0 that means
# "wrong reader", not "wrong build" -- while a message literal is invisible to `nm`.
# Reading one table and reporting a bare count is how a valid placement gets refused
# and how an invalid one gets waved through; the pair plus the table name is decidable.
count_in() {  # count_in <table> <file> <needle>
  case "$1" in
    nm) nm -a "$2" 2>/dev/null | grep -cF "$3" || true ;;
    *)  strings "$2" | grep -cF "$3" || true ;;
  esac
}
# A MARKER CAN DISCRIMINATE IN BOTH TABLES, AND THEN THE CONTROL PICKS WHICH ONE.
#
# This loop used to assign on every discriminating table, so the LAST one won --
# `nm` -- arbitrarily. A Rust string literal that is also a symbol name (a
# migration's table name, say) reads in both; the control is usually a plain
# message literal that CANNOT appear in `nm`. So the gate chose nm and then
# refused its own valid card for a control that was never possible there.
#
# The principle: the control's job is to prove the INSTRUMENT works on the table
# the marker was counted in, so the table must be one where a control can exist.
# Collect every discriminating table, then prefer one whose control reads on both
# images. Refusing only when NO such table exists keeps the arm as strict as it
# was without failing valid cards (PLEX, 190406a, first card to hit it).
if [ "$MARKER" = "none" ]; then
  # No literal to compare. Substitute the strongest available discriminator:
  # a rebuild always moves LC_UUID, and two files sharing one are the same build.
  staged_uuid=$(dwarfdump --uuid "$STAGED" 2>/dev/null | awk '{print $2}')
  live_uuid=$(dwarfdump --uuid "$DEST" 2>/dev/null | awk '{print $2}')
  say "marker: NONE DECLARED -- this change adds no string literal"
  [ -n "$staged_uuid" ] && [ -n "$live_uuid" ] \
    || refuse "--marker none needs LC_UUID from both images and one is unreadable (not a Mach-O?); nothing has been placed"
  [ "$staged_uuid" != "$live_uuid" ] \
    || refuse "--marker none but LC_UUID is IDENTICAL ($staged_uuid): the staged artifact is the same build as the running one; nothing has been placed"
  say "identity: LC_UUID differs (staged $staged_uuid, live $live_uuid)"
  say "IDENTITY RESTS ON sidecar + placed==staged + inode; BEHAVIOUR IS UNPROVEN BY THIS GATE"
else
marker_tables=""
for t in strings nm; do
  ms=$(count_in "$t" "$STAGED" "$MARKER")
  ml=$(count_in "$t" "$DEST" "$MARKER")
  say "marker $t:\"$MARKER\" staged $ms / live $ml"
  if [ "$ms" -gt 0 ] && [ "$ml" -eq 0 ]; then marker_tables="$marker_tables $t"; fi
  if [ "$ms" -gt 0 ] && [ "$ml" -gt 0 ]; then
    refuse "marker reads on BOTH images in the $t table: it does not separate the two builds"
  fi
done
[ -n "$marker_tables" ] || refuse "marker discriminates in NEITHER table: absent from the staged artifact (dead-code-eliminated, or a phrase from a comment, or a string belonging to a different binary)"
marker_table=""
if [ -n "$CONTROL" ]; then
  for t in $marker_tables; do
    cs=$(count_in "$t" "$STAGED" "$CONTROL")
    cl=$(count_in "$t" "$DEST" "$CONTROL")
    if [ "$cs" -gt 0 ] && [ "$cl" -gt 0 ]; then marker_table="$t"; break; fi
  done
  [ -n "$marker_table" ] || refuse "the control reads on both images in NONE of the tables where the marker discriminates ($marker_tables): the marker's count is uninformative, because nothing proves the instrument can see that table at all"
else
  # --control IS REQUIRED, and the reason is the counter one layer down.
  #
  #   count_in nm  -> nm -a "$f" 2>/dev/null | grep -cF "$needle" || true
  #
  # That returns 0 when the needle is ABSENT and 0 when THE TOOL FAILED --
  # stderr suppressed, `|| true` swallowing the status. Measured: `nm -a` on a
  # non-Mach-O file returns 0 while `strings` on the same file returns 3.
  #
  # So a marker reading "staged 1 / live 0" is consistent with the live image
  # genuinely lacking it AND with the instrument failing on the live image. A
  # false PASS, in the direction that places a binary.
  #
  # The control closes it by construction: it must read >0 on BOTH images in the
  # marker's table, which proves the instrument can see that table on both files.
  # It was already implemented and merely OPTIONAL, so every card omitting one
  # ran with the hole open.
  #
  # This is PLEX's rule applied to my own gate (2026-09-19): name the observation
  # that CANNOT occur if the probe is working, and check for that rather than for
  # the answer. Here the impossible observation is a control reading zero on an
  # image that demonstrably contains it.
  refuse "--control is required: without a needle known to be present in BOTH images, a marker reading 0 on the live image is indistinguishable from the counting tool having failed on it (nm -a on a non-Mach-O returns 0, silently). Pass --marker none for a change that adds no literal."
fi
say "marker discriminates in the $marker_table table"
fi
if [ -n "$CONTROL" ]; then
  c_staged=$(count_in "$marker_table" "$STAGED" "$CONTROL")
  c_live=$(count_in "$marker_table" "$DEST" "$CONTROL")
  say "control $marker_table:\"$CONTROL\" staged $c_staged / live $c_live"
  { [ "$c_staged" -gt 0 ] && [ "$c_live" -gt 0 ]; } \
    || refuse "control must read on BOTH images in the SAME table the marker used, else the marker's count is uninformative"
fi

# --gone asserts a REMOVAL, which is the inverse of a marker: a marker asks "did the
# new thing arrive", this asks "did the old thing leave". A bare "expect 0" is
# unfalsifiable -- it passes when the field is gone, when the reader is broken, and
# when the caller typos the string. THE DEPLOYED 1 IS WHAT MAKES THE STAGED 0 MEAN
# SOMETHING: same reader, same needle, one artifact answering each way.
if [ -n "$GONE" ]; then
  for t in strings nm; do
    gs=$(count_in "$t" "$STAGED" "$GONE")
    gl=$(count_in "$t" "$DEST" "$GONE")
    [ "$gl" -gt 0 ] || continue
    say "gone $t:\"$GONE\" staged $gs / live $gl"
    [ "$gs" -eq 0 ] || refuse "\"$GONE\" still reads $gs in the staged artifact: the removal did not land"
    gone_seen=1
  done
  [ "${gone_seen:-0}" = "1" ] \
    || refuse "\"$GONE\" is absent from the RUNNING image in both tables, so its absence from the staged one proves nothing (no positive control)"
fi

# A GATE WITH SIDE EFFECTS MUST BE EXERCISABLE WITHOUT THEM. Every arm above is a
# read; everything below mutates. Without this split the only way to test a new arm
# is to place a binary -- which is how 0.3.92 reached production outside its quiet
# window on 2026-09-15, while the author was attending to the arm's logic and not to
# what the script does after the arms pass. Remembering that it places is exactly the
# thing that failed.
if [ "$PLACE" != "1" ]; then
  say "=== arms evaluated, NOTHING placed and NOTHING restarted (pass --place to mutate)"
  exit 0
fi

say "=== rollback"
ts=$(date -u +%Y%m%dT%H%M%SZ)
rb="$STAGING/ck-$MODULE.rollback-$ts"
mkdir -p "$STAGING"
cp "$DEST" "$rb"
# THE SNAPSHOT IS COMPARED AGAINST ITS SOURCE, not against a hash taken from
# itself. Writing the sidecar from the copy and then running `shasum -c` is
# SELF-CONFIRMING: a truncated or corrupt `cp` is hashed as truncated and
# matches, so the check reports success on exactly the snapshot that cannot be
# rolled back to. That was this arm until 2026-09-18, and the comment beside it
# said "verified" -- a self-confirming claim wearing the words of a measurement.
# (FUSI found the identical shape in their stage.sh sidecar check; the general
# test is: break each thing the guard CLAIMS to catch, one at a time.)
#
# Source-vs-snapshot equality is the whole proof. The sidecar is still written,
# because it is what a later operator uses to check the file has not rotted on
# disk since -- a different question, answered at a different time.
live_digest=$(shasum -a 256 "$DEST" | awk '{print $1}')
rb_digest=$(shasum -a 256 "$rb" | awk '{print $1}')
[ -n "$live_digest" ] && [ "$live_digest" = "$rb_digest" ] \
  || refuse "rollback snapshot does not match the live binary it was copied from (live $live_digest, snapshot $rb_digest); nothing has been placed"
(cd "$STAGING" && shasum -a 256 "$(basename "$rb")" > "$(basename "$rb").sha256")
say "rollback $(basename "$rb") matches live (${live_digest%"${live_digest#????????}"}), holds: $("$rb" --version 2>&1 | head -1)"

if [ -n "$MIGRATES" ]; then
  [ -f "$MIGRATES" ] || refuse "--migrates named $MIGRATES, which is not a file; nothing has been placed"
  store_rb="$STAGING/$(basename "$MIGRATES").rollback-$(date -u +%Y%m%dT%H%M%SZ)"
  # sqlite3 .backup, NOT cp: the module holds the store open with a live -wal,
  # and cp captures a torn .db beside a WAL it does not include -- a snapshot
  # that restores to a state which never existed. .backup is the online backup
  # API and is WAL-correct against a running writer.
  sqlite3 "file:$MIGRATES?mode=ro" ".backup $store_rb" 2>/dev/null \
    || refuse "store snapshot failed for $MIGRATES; nothing has been placed"
  [ -s "$store_rb" ] || refuse "store snapshot $store_rb is empty; nothing has been placed"
  (cd "$STAGING" && shasum -a 256 "$(basename "$store_rb")" > "$(basename "$store_rb").sha256")
  say "store rollback $(basename "$store_rb") ($(stat -f %z "$store_rb" 2>/dev/null || stat -c %s "$store_rb") bytes)"
  say "ROLLBACK IS BINARY + STORE: this card migrates, so restoring the binary alone would meet a newer schema and refuse"
fi

say "=== place"
cp "$STAGED" "$DEST.tmp" && mv "$DEST.tmp" "$DEST"
# THE PLACED BYTES MUST EQUAL THE STAGED BYTES, and this printed a sha without
# comparing it until 2026-09-18. Found by grepping this file for the shape of
# the rollback defect fixed forty lines up rather than by anything failing --
# the same class, three lines apart, and the audit that found it took ninety
# seconds.
#
# Without it the chain has a gap: the staged artifact is verified against its
# sidecar and the destination is proven to EXECUTE, but nothing says the thing
# executing is the thing that was verified. A short write, a full disk, or a
# racing writer lands a different binary that may still run.
placed_digest=$(shasum -a 256 "$DEST" | awk '{print $1}')
staged_digest=$(shasum -a 256 "$STAGED" | awk '{print $1}')
[ -n "$placed_digest" ] && [ "$placed_digest" = "$staged_digest" ] \
  || refuse "placed bytes differ from the staged bytes (staged $staged_digest, placed $placed_digest); the destination now holds an unverified binary"
say "placed sha ${placed_digest%"${placed_digest#????????}"} (equals staged)"
say "warm-exec at destination: $("$DEST" --version 2>&1 | head -1)"

# PATH face: the operator may invoke this by name, and that resolution is what decides
# which bytes run — not the path we just wrote.
if [ -n "$PATH_FACE" ]; then
  resolved=$(command -v "$PATH_FACE" || true)
  if [ -z "$resolved" ]; then
    say "which $PATH_FACE: not on PATH -- nothing resolves it by name"
  elif [ "$(shasum -a 256 "$resolved" | cut -d' ' -f1)" = "$(shasum -a 256 "$DEST" | cut -d' ' -f1)" ]; then
    say "which $PATH_FACE: $resolved (same bytes as the placed file)"
  else
    say "WARNING: $PATH_FACE resolves to $resolved, which is NOT the file just placed."
    say "         An operator invoking it by name runs something other than what was verified."
    say "         Place it there too, or say why the divergence is intended."
  fi
fi

if [ "$RESTART" -eq 1 ]; then
  say "=== restart"
  ck module restart "$MODULE" 2>&1 | tail -1
  say "$(date -u +%FT%TZ) restart initiated -- verify on a lane opened AFTER this point:"
  say "  ck module status $MODULE   # then inode proc-vs-disk, which is the only proof it runs these bytes"
fi
