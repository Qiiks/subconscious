#!/usr/bin/env bash
# Answer "did THIS PUSH pass CI on this exact sha" — the question a tag asks.
#
# WHY THE EVENT FILTER IS LOAD-BEARING: a sha is not unique across triggers. This
# repo runs CI on push AND on a 06:00 cron, so any commit sitting at tip across
# that hour carries two runs of the same workflow. `gh run list --commit <sha>`
# returns them newest-first, so a bare `.[0]` answers with whichever fired LAST —
# usually the scheduled one. Measured here 2026-09-15: 93ebad32 carries
# `schedule:success push:success`, with the schedule run first.
#
# The two runs answer different questions. The push run asks "is this change
# good". The scheduled run asks "is what already landed still good against a world
# that moved" — a red there is usually toolchain drift, not a regression. Reading
# one as the other is undecidable from the conclusion alone, which is why the
# filter is in the query rather than in the reader's head.
#
# Both are reported. Only the push verdict gates a tag.
#
# usage: ci-verdict.sh <sha> [repo] [workflow]
set -euo pipefail

sha="${1:?usage: ci-verdict.sh <sha> [repo] [workflow]}"
repo="${2:-cortexkit/subconscious}"
workflow="${3:-CI}"

# `gh run list --commit` matches on the FULL sha and returns an empty list for an
# abbreviated one -- silently, with exit 0. An empty list then reads as "no run
# exists" when the true statement is "you asked with the wrong key". Resolve here
# so a short sha is usable at the command line without being a trap.
if full=$(git rev-parse "$sha^{commit}" 2>/dev/null); then
  sha="$full"
fi

runs=$(gh run list --repo "$repo" --commit "$sha" --workflow "$workflow" \
  --json databaseId,event,status,conclusion 2>/dev/null) || {
  echo "UNCHECKED: cannot list runs for ${sha:0:8} in $repo"
  exit 2
}

n=$(printf '%s' "$runs" | jq -r 'length')
if [ "$n" = "0" ]; then
  # No run is not a pass. A commit can have zero runs because the workflow is
  # still queuing, because a path filter excluded it, or because a conflicted PR
  # never produced a merge ref — none of which is "green".
  echo "NO_RUN: no $workflow run exists for ${sha:0:8}"
  exit 3
fi

printf '%s' "$runs" | jq -r '.[] | "  \(.event):\(.status):\(.conclusion // "pending")"'

push_status=$(printf '%s' "$runs" | jq -r '[.[] | select(.event=="push")][0].status // "absent"')
push_concl=$(printf '%s' "$runs" | jq -r '[.[] | select(.event=="push")][0].conclusion // "none"')

case "$push_status" in
  absent)     echo "VERDICT ${sha:0:8}: NO PUSH RUN — $n run(s) exist but none from a push"; exit 3 ;;
  completed)  ;;
  *)          echo "VERDICT ${sha:0:8}: PUSH RUN STILL $push_status — no verdict yet"; exit 4 ;;
esac

# A run whose top-level conclusion is success can still contain a cancelled or
# skipped leg; the tag rule is that every defined leg completed with success on
# this exact sha.
id=$(printf '%s' "$runs" | jq -r '[.[] | select(.event=="push")][0].databaseId')
jobs=$(gh run view "$id" --repo "$repo" --json jobs 2>/dev/null) || {
  echo "VERDICT ${sha:0:8}: push=$push_concl (jobs unreadable)"; exit 2; }

printf '%s' "$jobs" | jq -r '.jobs[] | "    \(.name) = \(.conclusion // .status)"'
all_green=$(printf '%s' "$jobs" | jq -r '[.jobs[] | select(.conclusion != "success")] | length')

if [ "$all_green" = "0" ] && [ "$push_concl" = "success" ]; then
  echo "VERDICT ${sha:0:8}: PUSH GREEN — every leg completed success"
  exit 0
fi
echo "VERDICT ${sha:0:8}: PUSH NOT GREEN (conclusion=$push_concl, non-success legs=$all_green)"
exit 1
