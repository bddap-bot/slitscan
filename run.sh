#!/usr/bin/env bash
# The installation's launch line: fullscreen on whatever display it is given,
# reading the webcam. Arguments are passed through, so `run.sh --sweep
# top-to-bottom` flips the line without a rebuild.
#
# Through nix-shell rather than the binary directly: wgpu and winit dlopen the
# Vulkan loader and the windowing libraries at run time, and the shell is what
# puts them on the library path.
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
until nix-shell --run "cargo run --release -- ${*@Q}"; do
    sleep 5
done
