#!/usr/bin/env bash
# Everything nix has to say, said once here so that run.sh never has to ask:
# the binary, a gc root over the shell's closure, and the two environment
# values wgpu, winit and the camera need at run time.
set -euo pipefail
cd "$(dirname "$0")"

nix-shell --run 'cargo build --release'

# nix-shell roots the shell's inputs only while it runs, so without this a
# store collection between launches would put a download in front of a piece
# that is supposed to come up on mains power alone.
nix-build shell.nix -A inputDerivation -o target/shell-root

# Single-quoted on purpose: this is the *inner* shell's expansion, read from
# inside the shell whose values are being captured.
# shellcheck disable=SC2016
nix-shell --run 'printf "export LD_LIBRARY_PATH=%s\nexport PATH=%s:\$PATH\n" "${LD_LIBRARY_PATH:?}" "$(dirname "$(command -v ffmpeg)")"' > target/launch.env.new
grep -qE '^export LD_LIBRARY_PATH=/nix/store/' target/launch.env.new
grep -qE '^export PATH=/nix/store/[^:]*/bin:' target/launch.env.new
mv target/launch.env.new target/launch.env
