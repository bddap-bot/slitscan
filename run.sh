#!/usr/bin/env bash
# The installation's launch line: fullscreen on whatever display it is given,
# reading the webcam. Arguments are passed through, so `run.sh --sweep
# top-to-bottom` flips the line without a rebuild.
#
# Nothing here evaluates nix or cargo: build.sh already wrote what they had to
# say into target/launch.env, and a launch that had to ask again is twelve
# seconds of dark screen after the shortcut is clicked (bddap-bot/slitscan#4).
#
# `until` is the supervisor, and it is what makes the piece's own error
# handling worth anything: slitscan stops on a dead camera or a lost surface
# rather than degrading, and a fresh process re-negotiates the camera's mode
# and rebuilds every GPU resource — which is the only thing that would have
# worked anyway. Quitting on purpose exits zero and ends the loop; the sleep
# keeps a permanent failure to one attempt every five seconds, with its reason
# on the terminal each time.
set -euo pipefail
cd "$(dirname "$0")"
if [ ! -x target/release/slitscan ] || [ ! -f target/launch.env ]; then
    echo "run.sh: not built — run ./build.sh first" >&2
    exit 1
fi
# shellcheck source=/dev/null
. target/launch.env
until target/release/slitscan "$@"; do
    sleep 5
done
