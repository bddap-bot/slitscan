# slitscan

The television shows the live camera, but each displayed frame replaces only
one line of it: a single pixel column, one column further right every frame,
starting over at the left edge when it runs off the right. Everything the line
is not on holds whatever second it was last written in, so the screen is one
column of now beside a minute of the recent past — a slit-scan camera with the
screen as its film. At 3840 columns and 60 Hz a pass takes sixty-four seconds,
and that slowness is the piece.

```
./run.sh                             # the webcam, fullscreen, left to right
./run.sh --sweep top-to-bottom       # a horizontal line, sweeping down
./run.sh --device /dev/video2 --capture 1280x720
```

Esc quits. `--sweep` takes `left-to-right`, `right-to-left`, `top-to-bottom` or
`bottom-to-top`; the line is always perpendicular to its travel.

## How it works

Two textures and one shader. The *field* is a texture the size of the display
that is never cleared; each frame, one render pass draws the camera over the
whole of it with a scissor rectangle one pixel wide, so only that line changes
and the rest of the field is whatever earlier frames put there. A second pass
draws the field to the screen. The sweep has no clock — the line moves one
step per presented frame, and the surface presents in `Fifo`, so the display's
refresh *is* the tempo.

The camera is an `ffmpeg` reading the v4l2 node and writing raw RGBA down a
pipe, asked for a mode with `-video_size` rather than put through a scaler:
that keeps the sensor's own shape, and fitting that shape to the display's is
the shader's job. A camera and a display of different shapes zoom to fill — the
overhanging axis is cropped evenly from both ends, never letterboxed.

The camera is uploaded at capture size rather than display size on purpose.
Only one line of it is ever looked at, so a 4K frame sixty times a second would
be two gigabytes a second down a pipe for no visible difference.

## Run it without Nix

`shell.nix` pins nixpkgs, carries the `ffmpeg` the camera runs, and puts the
Vulkan loader and the windowing libraries — which wgpu and winit open at run
time — on `LD_LIBRARY_PATH`. Without Nix: a Rust toolchain recent enough for
wgpu 30 and winit 0.30, a working Vulkan driver, and an ffmpeg on `PATH`.

## Tests

```
nix-shell --run "cargo test"
```

The sweep's arithmetic and the zoom-to-fill are pure and tested without a GPU:
that a pass touches every line of the field exactly once before repeating, that
the two backwards sweeps are the forwards ones read from the far end, and that
the crop keeps all of one axis, is centred on the other, and so can never leave
a bar. The camera is tested against a real ffmpeg — that whole frames of the
size it was opened at keep arriving, and that a device that will not open is an
error at once rather than after the first-frame timeout.

The tests in `tests/` run on a real GPU and read the field back: that the line
lands on the column the sweep names and nothing else moves, that columns the
line has not reached yet are still black, that coming back round writes over
the first pass rather than stopping, that a downward sweep writes rows, and
that a camera the wrong shape arrives cropped rather than squashed. On a
machine with no adapter each one prints why and returns; libtest still counts
them as passed.

They also leave `target/evidence/*.png`: the field every eighth of a pass,
against a camera showing one bright bar sliding up and down. One camera frame
has one bar in it and the field ends up with a whole wave, because the field's
x axis is time — which is the piece, and is the thing no single frame could
produce.
