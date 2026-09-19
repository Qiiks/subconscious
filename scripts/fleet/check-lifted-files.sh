#!/usr/bin/env bash
# Refuse when a LIFTED file has drifted from the upstream it was copied from.
#
# A lifted file is a copy, and a copy is N instances that drift independently.
# Nobody tells you the original moved; you find out when a defect that was fixed
# upstream months ago bites you locally.
#
# THE REFERENCE IS origin/<branch>, NEVER THE UPSTREAM CHECKOUT'S HEAD OR WORKING
# TREE. AVA walked into both halves of that on 2026-09-19: lifting from the
# upstream checkout's HEAD produced a blob matching no other seat and no
# fetchable sha, because that HEAD carried an unpushed commit; then comparing a
# correctly-lifted pin against HEAD reported "stale forever". Pinning published
# while comparing unpublished is incoherent in both directions.
#
#   PUBLISHED IS THE ONLY REFERENCE THAT MEANS THE SAME THING ON EVERY MACHINE.
#   A LIFT FROM ANYTHING ELSE IS A COPY OF SOMEONE'S DESK.
#
# Convergence is not currency: four seats matching one blob says they agree, not
# that they are current. This checks currency against the publisher.
set -euo pipefail

fail=0
check() {  # check <local path> <upstream repo> <branch> <upstream path> <pinned blob>
  local local_path="$1" repo="$2" branch="$3" up_path="$4" pinned="$5"
  local dir="$HOME/Work/Projects/CortexKit/$repo"
  if [ ! -d "$dir/.git" ]; then
    echo "SKIP $local_path: upstream $repo not present on this host"
    return
  fi
  ( cd "$dir" && git fetch -q origin 2>/dev/null ) || {
    echo "SKIP $local_path: could not fetch $repo (offline?) -- currency UNKNOWN, not clean"
    return
  }
  local upstream_blob
  upstream_blob=$(cd "$dir" && git rev-parse "origin/$branch:$up_path" 2>/dev/null || true)
  [ -n "$upstream_blob" ] || { echo "SKIP $local_path: $up_path absent at $repo origin/$branch"; return; }

  local local_blob
  local_blob=$(git hash-object "$local_path")

  if [ "$local_blob" != "$pinned" ]; then
    echo "REFUSED $local_path: LOCALLY MODIFIED (blob ${local_blob:0:8}, pinned ${pinned:0:8})"
    echo "        A lifted file edited in place diverges silently and can never be re-lifted cleanly."
    echo "        Send the change upstream to $repo, then re-lift and repin."
    fail=1
  elif [ "$local_blob" != "$upstream_blob" ]; then
    echo "REFUSED $local_path: UPSTREAM HAS MOVED (ours ${local_blob:0:8}, $repo origin/$branch ${upstream_blob:0:8})"
    echo "        Re-lift:  (cd $dir && git show origin/$branch:$up_path) > $local_path"
    echo "        then update the pin in this script to ${upstream_blob:0:8}..."
    fail=1
  else
    echo "OK $local_path: byte-identical to $repo origin/$branch (${local_blob:0:8})"
  fi
}

check scripts/fleet/train-push.sh aft main scripts/train-push.sh 01fe12e59b1c77b70c31ce51b7491871ba024179

exit "$fail"
