#!/bin/bash
# Keep the shared target/ from filling while parallel reviewers build.
# Debug test binaries here are 400-600MB each and cargo keeps every variant, so
# deps/ dominates. Acts at 5G free, well before a build actually dies.
T=/home/user/codypendent/target/debug
for _ in $(seq 1 400); do
  free=$(df --output=avail -BG / | tail -1 | tr -dc '0-9')
  if [ "${free:-99}" -lt 5 ]; then
    rm -rf "$T/incremental"
    find "$T/deps" -maxdepth 1 -type f -name '*_it-*' -size +50M -delete 2>/dev/null
    echo "$(date +%T) reclaimed at ${free}G -> $(df --output=avail -BG / | tail -1 | tr -d ' ')"
  fi
  sleep 20
done
