//! The live camera: an `ffmpeg` reading a v4l2 node and writing raw RGBA
//! frames down a pipe, and the thread draining it.
//!
//! ffmpeg rather than v4l2 directly because a webcam's own output is MJPEG or
//! YUYV depending on the mode it lands in, and decoding either one here would
//! be this piece's largest component by far for no visible difference.
//!
//! It is asked for a *mode* (`-video_size`) rather than sent through a
//! scaler, so the frames arrive at the sensor's own shape: fitting the
//! camera's shape to the display's is [`crate::sweep::Cover`]'s job on the
//! GPU, and a scaler here would have already thrown that shape away.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{sync_channel, Receiver};
use std::time::Duration;

/// How long the camera has to hand over its first frame before it counts as
/// broken. A capture device negotiates a format, so this is generous; it only
/// has to be shorter than a performer's patience, since an ffmpeg that cannot
/// open the device says so and exits at once rather than waiting it out.
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(10);

/// ffmpeg's input arguments for a webcam on `device` in its `size` mode.
///
/// Every piece is written here rather than taken from the command line: the
/// device name lands as the argument of `-i`, which ffmpeg reads positionally,
/// so nothing a `--device` value can say becomes a flag.
pub fn v4l2(device: &str, size: (u32, u32)) -> Vec<String> {
    // Before -i, so it is the *input's* option: after it, ffmpeg reads it as
    // the output's and the camera is left in whatever mode it defaulted to
    // while this still expects frames of `size`.
    let (width, height) = size;
    [
        "-f",
        "v4l2",
        "-video_size",
        &format!("{width}x{height}"),
        "-i",
        device,
    ]
    .map(String::from)
    .to_vec()
}

/// Bytes in one tightly packed RGBA8 frame.
pub fn frame_bytes(size: (u32, u32)) -> usize {
    size.0 as usize * size.1 as usize * 4
}

pub struct Camera {
    child: Child,
    /// One frame in flight. That bound is the throttle: ffmpeg blocks on its
    /// own pipe once the reader is holding a frame nobody has collected, so
    /// nothing queues up ahead of what is on the glass. A camera paces itself
    /// well under the display's rate, so in practice it never blocks.
    ///
    /// `None` only while [`Camera::drop`] is releasing the reader.
    frames: Option<Receiver<Vec<u8>>>,
    /// The frame [`Camera::open`] took off the channel to prove the device
    /// works before a window ever opened.
    first: Option<Vec<u8>>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl Camera {
    /// Starts an ffmpeg with `input` arguments producing frames of `size`,
    /// blocking until one has arrived — so a device that will not open is an
    /// error here, at startup, rather than a black field nobody can explain.
    ///
    /// `input` is a whole argument list rather than a device path so that the
    /// tests can point this at one of ffmpeg's own generators: what is worth
    /// testing is the pipe, the framing and the shutdown, none of which cares
    /// what ffmpeg opened.
    pub fn open(input: &[String], size: (u32, u32)) -> Result<Camera, String> {
        let mut argv: Vec<String> = ["-nostdin", "-loglevel", "error"]
            .map(String::from)
            .to_vec();
        argv.extend_from_slice(input);
        argv.extend(["-an", "-f", "rawvideo", "-pix_fmt", "rgba", "-"].map(String::from));
        let what = format!("ffmpeg {}", input.join(" "));

        let mut child = Command::new("ffmpeg")
            .args(&argv)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            // ffmpeg's own diagnosis of a device that will not open is better
            // than anything this could write about it.
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("{what}: cannot run ffmpeg: {e}"))?;
        let mut stdout = child.stdout.take().expect("stdout is piped");

        let bytes = frame_bytes(size);
        let (send, frames) = sync_channel(1);
        let ended = what.clone();
        let reader = std::thread::spawn(move || loop {
            let mut frame = vec![0u8; bytes];
            // A short read is the stream ending — a killed child, or a camera
            // unplugged — and a send that fails is this Camera being dropped.
            // Either way the last whole frame stays on the field and this
            // thread is done.
            if stdout.read_exact(&mut frame).is_err() {
                log::warn!("{ended} ended; the field keeps its last frame");
                return;
            }
            if send.send(frame).is_err() {
                return;
            }
        });

        let mut camera = Camera {
            child,
            frames: Some(frames),
            first: None,
            reader: Some(reader),
        };
        // Built first, so every way out of here runs Camera's Drop and reaps
        // the child. A dead ffmpeg drops its end of the channel, so this
        // returns as soon as it exits rather than waiting out the timeout.
        camera.first = Some(
            camera
                .frames
                .as_ref()
                .expect("just built")
                .recv_timeout(FIRST_FRAME_TIMEOUT)
                .map_err(|e| format!("{what}: no frame ({e}); ffmpeg's own error is above"))?,
        );
        Ok(camera)
    }

    /// The newest frame since the last call, tightly packed RGBA8, or `None`
    /// when nothing has arrived — in which case the field's copy is already
    /// the most recent one and wants no upload. A camera that has ended
    /// returns `None` for good.
    pub fn frame(&mut self) -> Option<Vec<u8>> {
        if let Some(first) = self.first.take() {
            return Some(first);
        }
        // Nothing yet and never again are the same answer here: either way
        // the field keeps what it has.
        self.frames.as_ref()?.try_recv().ok()
    }
}

impl Drop for Camera {
    fn drop(&mut self) {
        // The receiver goes first: a reader blocked handing over a frame is
        // not reading, so killing the child would not wake it. Dropping the
        // channel fails that send; killing the child then ends the read the
        // reader would otherwise be blocked in. Only once it is joined is
        // there nothing left to reap.
        self.frames.take();
        let _ = self.child.kill();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Where `arg` sits in an argument list, for the assertions about order.
    fn at(argv: &[String], arg: &str) -> usize {
        argv.iter()
            .position(|a| a == arg)
            .unwrap_or_else(|| panic!("{arg} is not in {argv:?}"))
    }

    #[test]
    fn the_camera_is_asked_for_a_mode_rather_than_scaled_into_one() {
        let argv = v4l2("/dev/video0", (1280, 720));
        assert!(argv.windows(2).any(|w| w == ["-f", "v4l2"]));
        assert!(argv.windows(2).any(|w| w == ["-i", "/dev/video0"]));
        assert!(argv.windows(2).any(|w| w == ["-video_size", "1280x720"]));
        // After -i it would be the output's option and the camera would stay
        // in its default mode.
        assert!(at(&argv, "-video_size") < at(&argv, "-i"));
        // A scaler here would throw away the sensor's shape, which is the one
        // thing the zoom-to-fill needs to know.
        assert!(!argv.iter().any(|a| a == "-vf"), "{argv:?}");
    }

    /// `false` when there is no ffmpeg to test with. `shell.nix` has one, so
    /// this only fires outside the pinned shell — printed to stderr, since
    /// libtest eats a passing test's output and a skip nobody sees is a
    /// silent pass.
    fn have_ffmpeg() -> bool {
        let ok = Command::new("ffmpeg")
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok();
        if !ok {
            use std::io::Write;
            let _ = writeln!(std::io::stderr(), "SKIPPED: no ffmpeg on PATH");
        }
        ok
    }

    /// One of ffmpeg's own generators, which is a capture device in every
    /// sense this module cares about: ffmpeg opening something named by `-f`
    /// and `-i` and handing over frames until it is killed.
    fn generator(spec: &str) -> Vec<String> {
        ["-f", "lavfi", "-i", spec].map(String::from).to_vec()
    }

    #[test]
    fn a_camera_delivers_whole_frames_of_the_size_it_was_opened_at() {
        if !have_ffmpeg() {
            return;
        }
        let size = (64, 48);
        // Red, so a channel swap between ffmpeg and here cannot pass.
        let mut camera = Camera::open(&generator("color=c=red:s=64x48:r=30"), size).unwrap();
        let frame = camera.frame().expect("open waits for the first frame");
        assert_eq!(frame.len(), frame_bytes(size));
        assert!(
            frame
                .chunks_exact(4)
                .all(|p| p[0] > 200 && p[1] < 40 && p[2] < 40 && p[3] == 255),
            "not opaque red"
        );
    }

    #[test]
    fn a_camera_that_will_not_open_says_so_at_once() {
        if !have_ffmpeg() {
            return;
        }
        // Half the contract is the "at once": ffmpeg cannot open this and
        // exits, which closes the channel, so the wait ends there instead of
        // running out the ten-second timeout.
        let began = std::time::Instant::now();
        let input = ["-f", "v4l2", "-i", "/dev/definitely-not-a-camera"].map(String::from);
        let Err(why) = Camera::open(&input, (64, 48)) else {
            panic!("a device that is not there opened")
        };
        assert!(why.contains("definitely-not-a-camera"), "{why}");
        assert!(
            began.elapsed() < FIRST_FRAME_TIMEOUT / 2,
            "{:?}",
            began.elapsed()
        );
    }

    #[test]
    fn a_camera_keeps_producing_frames_after_the_first() {
        if !have_ffmpeg() {
            return;
        }
        let size = (64, 48);
        let mut camera = Camera::open(&generator("testsrc2=size=64x48:rate=30"), size).unwrap();
        let mut seen = 0;
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while seen < 3 && std::time::Instant::now() < deadline {
            if let Some(frame) = camera.frame() {
                assert_eq!(frame.len(), frame_bytes(size));
                seen += 1;
            }
        }
        assert_eq!(seen, 3, "the pipe stopped after the first frame");
    }
}
