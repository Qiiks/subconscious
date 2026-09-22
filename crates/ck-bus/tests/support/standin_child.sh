#!/bin/sh
# Ignore daemon arguments and never speak the module protocol.
trap 'exit 0' TERM
while :; do
    sleep 1 &
    wait $!
done
