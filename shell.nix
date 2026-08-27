# Self-contained dev shell: `nix-shell --run "cargo build"`.
# nixpkgs is pinned by hash so the shell is reproducible without channels
# ($NIX_PATH is empty on some hosts, so `nix-shell -p` is not an option).
let
  pkgs = import (fetchTarball {
    url = "https://github.com/NixOS/nixpkgs/archive/d6c71932130818840fc8fe9509cf50be8c64634f.tar.gz";
    sha256 = "1klgyhj98j3gfsql5sn9rapyx62qk5g8adk5zh9mnc4d0fj61gdr";
  }) { };

  # wgpu and winit dlopen these at run time; nothing links them at build time,
  # so they belong on the library path and not in buildInputs.
  runtimeLibs = with pkgs; [
    vulkan-loader
    libxkbcommon
    wayland
    libx11
    libxcursor
    libxi
    libxrandr
  ];
in
pkgs.mkShell {
  buildInputs = with pkgs; [
    cargo
    rustc
    clippy
    rustfmt
    # The launcher is shell; shellcheck is the only thing that reads it.
    shellcheck
    # The camera is an ffmpeg reading /dev/video0 and writing raw RGBA down a
    # pipe, so the binary needs one on PATH to see anything at all.
    ffmpeg
  ];

  LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath runtimeLibs;
}
