#!/bin/bash
# Reclaim cargo build debris across the CortexKit trees when the data volume is
# low. Debug and cross-target directories rebuild in minutes; target/release is
# the staging cache and is never touched. Only the SUBCONSCIOUS seat's own trees
# are swept unconditionally; every other repo is another seat's working tree,
# so it is swept only when --all is passed (the disk-emergency shape), and never
# while a cargo process has the directory open.
#
# Sizes are KiB from `du -sk` (BSD du reports 512-byte blocks by default, which
# reads as 2x GiB -- banked 2026-09-14). Freed space is the df delta, never a
# sum of du figures, because APFS clones make the sum a claim about nothing.
#
# Usage: sweep-build-debris.sh [--all] [--floor-gib N] [--dry-run]
set -u
ALL=0; FLOOR=150; DRY=0
while [ $# -gt 0 ]; do
  case "$1" in
    --all) ALL=1; shift ;;
    --floor-gib) FLOOR="$2"; shift 2 ;;
    --dry-run) DRY=1; shift ;;
    *) echo "usage: $0 [--all] [--floor-gib N] [--dry-run]" >&2; exit 64 ;;
  esac
done
ROOT=~/Work/Projects/CortexKit
OWN="subconscious entorhinal commons"
free_gib() { df -k /System/Volumes/Data | tail -1 | awk '{print int($4/1048576)}'; }
B=$(free_gib)
if [ "$B" -ge "$FLOOR" ] && [ "$ALL" = 0 ]; then
  echo "free ${B} GiB >= floor ${FLOOR} GiB: nothing swept"; exit 0
fi
MANIFEST=~/.local/share/cortexkit/run/disk-sweep-$(date -u +%Y%m%dT%H%M%SZ).manifest
: > "$MANIFEST"
swept=0; skipped=0
for r in "$ROOT"/*/; do
  n=$(basename "$r")
  case " $OWN " in *" $n "*) ;; *) [ "$ALL" = 1 ] || continue ;; esac
  t="$r/target"; [ -d "$t" ] || continue
  if lsof +D "$t" 2>/dev/null | awk 'NR>1 && $1 ~ /cargo|rustc|ld/ {found=1} END {exit !found}'; then
    echo "  SKIP $n: a build has $t open"; skipped=$((skipped+1)); continue
  fi
  for d in "$t"/debug "$t"/*-*-*/ "$t"/flycheck* "$t"/doc "$t"/tmp; do
    [ -d "$d" ] || continue
    kib=$(du -sk "$d" 2>/dev/null | cut -f1)
    echo "$kib KiB  $d" >> "$MANIFEST"
    [ "$DRY" = 1 ] || rm -rf "$d"
    swept=$((swept+1))
  done
done
A=$(free_gib)
echo "manifest: $MANIFEST ($swept dirs, $skipped repos skipped as building)"
if [ "$DRY" = 1 ]; then echo "dry-run: would free ~$(awk '{s+=$1} END {printf "%.0f", s/1048576}' "$MANIFEST") GiB (du sum, upper bound)"
else echo "freed $((A-B)) GiB by df delta; free now ${A} GiB"; fi
