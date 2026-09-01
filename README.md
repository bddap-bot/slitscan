# slitscan

The television shows the live camera, but each displayed frame replaces only
one line of it: a single pixel column, one column further right every frame,
starting over at the left edge when it runs off the right. Everything the line
is not on holds whatever second it was last written in, so the screen is one
column of now beside a minute of the recent past — a slit-scan camera with the
screen as its film. At 3840 columns and 60 Hz a pass takes sixty-four seconds,
and that slowness is the piece.

```
./build.sh                           # once, after a checkout or a pull
./run.sh                             # the webcam, fullscreen, left to right
./run.sh --sweep top-to-bottom       # a horizontal line, sweeping down
./run.sh --device /dev/video2 --capture 1280x720
```

Esc quits. `--sweep` takes `left-to-right` or `top-to-bottom`; the line is
always perpendicular to its travel.

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

`--capture` is a request, not a promise. `-video_size` is advisory: the v4l2
demuxer takes whatever mode the driver answers with and mentions the
substitution at a log level `-loglevel error` hides. Raw video down a pipe
carries no framing, so reading it at the wrong size shears every frame with
nothing able to notice — which is why an `ffprobe` runs first and the size the
stream is really in is the size everything downstream is built from. The
startup line prints both. That is two identical requests to the same driver
rather than one look at one negotiation; the only way to close the gap outright
is a container that carries its own framing, and the one ffmpeg offers is
YUV-only and would cost a colour conversion in the shader.

The camera is uploaded at capture size rather than display size on purpose.
Only one line of it is ever looked at, so a 4K frame sixty times a second would
be two gigabytes a second down a pipe for no visible difference.

A camera that ends — unplugged, or its ffmpeg killed — stops the piece with a
message and a non-zero exit, rather than degrading. It has to: the field would
otherwise keep writing the last frame it saw, and inside a minute the whole
screen is that one frozen image, which looks exactly like a working
installation. A lost surface stops it the same way. `run.sh` is the other half
of that: it restarts on a non-zero exit, and a fresh process re-negotiates the
camera's mode and rebuilds every GPU resource, which is the only thing that
would have worked anyway. Quitting on purpose exits zero and ends the loop.

## Run it without Nix

`shell.nix` pins nixpkgs, carries the `ffmpeg` the camera runs, and puts the
Vulkan loader and the windowing libraries — which wgpu and winit open at run
time — on `LD_LIBRARY_PATH`. `build.sh` writes those two values into
`target/launch.env`, which is why `run.sh` evaluates neither nix nor cargo: the
installation comes up in about a second instead of waiting out a build.

Without Nix: a Rust toolchain recent enough for wgpu 30 and winit 0.30, a
working Vulkan driver, ffmpeg on `PATH`, and `cargo build --release` plus an
empty `target/launch.env` in place of `build.sh`.

## Tests

```
nix-shell --run "cargo test"
```

The sweep's arithmetic and the zoom-to-fill are pure and tested without a GPU:
that a pass writes every line of the field in order and wraps at the edge, and
that the crop keeps all of one axis, is centred on the other, and so can never
leave a bar. The camera is tested against a real ffmpeg — that the size comes
from the stream rather than from the request, that whole frames keep arriving,
that a device that will not open is an error at once rather than after the
first-frame timeout, and that a camera which ends says so rather than going
quiet.

The tests in `tests/` run on a real GPU and read the field back: that the line
lands on the column the sweep names and nothing else moves, that columns the
line has not reached yet are still black, that coming back round writes over
the first pass rather than stopping, that a downward sweep writes rows, that
the camera arrives the right way up and the right way round, that a camera the
wrong shape arrives cropped rather than squashed, and that the present pass
puts the field unchanged onto a target in a surface's format rather than the
field's — a BGRA one, since that is what a real display hands back and nothing
else here would have exercised it. There is no skip when there is no adapter:
this piece is a GPU program, and a suite that passes having rendered nothing is
worth less than one that fails.

They also leave `target/evidence/*.png`: the field every eighth of a pass,
against a camera showing one bright bar sliding up and down. One camera frame
has one bar in it and the field ends up with a whole wave, because the field's
x axis is time — which is the piece, and is the thing no single frame could
produce.

`docs/evidence.png` is a snapshot of that strip — the field at six points
through a pass and a bit, top to bottom. The wave is one bar photographed at
250 different moments; the step near the left of the last two is the line
coming back round and writing over where it started.
