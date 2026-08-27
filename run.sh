#!/usr/bin/env bash
# The installation's launch line: fullscreen on whatever display it is given,
# reading the webcam. Arguments are passed through, so `run.sh --sweep
# top-to-bottom` flips the line without a rebuild.
#
# Through nix-shell rather than the binary directly: wgpu and winit dlopen the
# Vulkan loader and the windowing libraries at run time, and the shell is what
# puts them on the library path.
#
# `${*@Q}` rather than a printf: printf runs its format once even with no
# arguments, so it would pass an empty argument the parser rightly rejects —
# i.e. the bare `./run.sh` would be the one invocation that did not work.
set -euo pipefail
cd "$(dirname "$0")"
exec nix-shell --run "cargo run --release -- ${*@Q}"
