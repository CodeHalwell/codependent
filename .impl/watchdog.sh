#!/bin/bash
T=/home/user/codypendent/target/debug
for _ in $(seq 1 900); do
  free=$(df --output=avail -BG / | tail -1 | tr -dc '0-9')
  if [ "${free:-99}" -lt 6 ]; then
    rm -rf "$T/incremental"
    find "$T/deps" -maxdepth 1 -type f -size +200M -delete 2>/dev/null
    echo "$(date +%T) reclaimed at ${free}G -> $(df --output=avail -BG / | tail -1 | tr -d ' ')"
  fi
  sleep 15
done
