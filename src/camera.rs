//! The live camera: an `ffmpeg` reading a v4l2 node and writing raw RGBA
//! frames down a pipe, and the thread draining it.
//!
//! ffmpeg rather than v4l2 directly because a webcam's own output is MJPEG or
//! YUYV depending on the mode it lands in, and decoding either one here would
//! be this piece's largest component by far for no visible difference.
//!
//! It is asked for a *mode* rather than sent through a scaler, so the frames
//! arrive at the sensor's own shape: fitting that shape to the display's is
//! [`crate::sweep::Cover`]'s job on the GPU. The mode is only a *request*,
//! which is what [`granted`] is for.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{sync_channel, Receiver, TryRecvError};
use std::time::Duration;

use crate::frame_bytes;

/// How long the camera has to prove it works — separately for each of the two
/// processes it takes. A device negotiates a format, so this is generous; it
/// only has to be shorter than a performer's patience, since an ffmpeg that
/// cannot open the device says so and exits at once rather than waiting it
/// out. What it bounds is the other case: a node that opens and then never
/// streams, which without a deadline is a startup that hangs in silence.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);

/// ffmpeg's input arguments for a webcam on `device`, asking for its `size`
/// mode as MJPEG at sixty frames a second.
///
/// The pixel format is named rather than left open because leaving it open is
/// how the piece ended up at 640x480: libavdevice walks its conversion table
/// and keeps the first entry the driver accepts *at whatever size the driver
/// then substitutes*, and a USB 2.0 webcam has the isochronous bandwidth for a
/// raw mode only at the smallest sizes — so the walk lands on YUYV and 1920
/// becomes 640. A substituted size is not an error to libavdevice; a rejected
/// pixel format is. Naming MJPEG turns a camera that cannot do the mode into a
/// failure at startup rather than a quiet shrink. Sixty is the display's own
/// rate: one fresh frame per written column, which is the piece's premise.
///
/// Every piece is written here rather than taken from the command line: the
/// device name lands as the argument of `-i`, which ffmpeg reads positionally,
/// so nothing a `--device` value can say becomes a flag.
pub fn v4l2(device: &str, size: (u32, u32)) -> Vec<String> {
    // Before -i, so these are the *input's* options: after it, ffmpeg reads
    // them as the output's, which scales rather than negotiates.
    let (width, height) = size;
    [
        "-f",
        "v4l2",
        "-input_format",
        "mjpeg",
        "-framerate",
        "60",
        "-video_size",
        &format!("{width}x{height}"),
        "-i",
        device,
    ]
    .map(String::from)
    .to_vec()
}

/// What the camera had when it was asked.
pub enum Frame {
    /// A new frame, tightly packed RGBA8 at [`Camera::size`].
    New(Vec<u8>),
    /// Nothing since the last call. The field's copy is already the most
    /// recent one and wants no upload — the normal case, since a camera runs
    /// at half the display's rate or less.
    Same,
    /// The camera is gone and will not come back: unplugged, or its ffmpeg
    /// died. Terminal, and deliberately not survivable — a field that keeps
    /// writing one frozen frame paints it across the whole screen inside a
    /// minute and looks exactly like a working installation.
    Ended,
}

pub struct Camera {
    /// The size frames actually arrive at, which is what the pipe is framed
    /// on and what the field's crop is built from.
    size: (u32, u32),
    child: Child,
    /// One frame in flight. That bound is the throttle: ffmpeg blocks on its
    /// own pipe once the reader is holding a frame nobody has collected, so
    /// nothing queues up ahead of what is on the glass.
    ///
    /// `None` only while [`Camera::drop`] is releasing the reader.
    frames: Option<Receiver<Vec<u8>>>,
    /// The frame [`Camera::open`] took off the channel to prove the device
    /// works before a window ever opened.
    first: Option<Vec<u8>>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl Camera {
    /// Starts an ffmpeg with `input` arguments, blocking until a frame has
    /// arrived — so a device that will not open is an error here, at startup,
    /// rather than a black field nobody can explain.
    ///
    /// `input` is a whole argument list rather than a device path so that the
    /// tests can point this at one of ffmpeg's own generators: what is worth
    /// testing is the negotiation, the pipe and the shutdown, none of which
    /// cares what ffmpeg opened.
    pub fn open(input: &[String]) -> Result<Camera, String> {
        let size = granted(input)?;
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
            if stdout.read_exact(&mut frame).is_err() {
                log::warn!("{ended} ended");
                return;
            }
            if send.send(frame).is_err() {
                return;
            }
        });

        let mut camera = Camera {
            size,
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
                .recv_timeout(STARTUP_TIMEOUT)
                .map_err(|e| format!("{what}: no frame ({e}); ffmpeg's own error is above"))?,
        );
        Ok(camera)
    }

    /// The size frames arrive at — the driver's answer, not the request.
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// The newest frame the camera has produced since the last call, older
    /// ones dropped: what belongs on the next line is the present, and a
    /// backlog would put the past there instead.
    pub fn frame(&mut self) -> Frame {
        // The proof-of-life frame is a *fallback*, not the head of the queue:
        // by the time the GPU is up, seconds have passed and newer frames are
        // waiting. Draining first makes the opening column the present, like
        // every column after it.
        let first = self.first.take();
        let Some(frames) = self.frames.as_ref() else {
            return first.map_or(Frame::Ended, Frame::New);
        };
        let mut newest = None;
        loop {
            match frames.try_recv() {
                Ok(frame) => newest = Some(frame),
                Err(TryRecvError::Empty) => {
                    return newest.or(first).map_or(Frame::Same, Frame::New)
                }
                // The reader is gone. Anything it had already handed over is
                // still the newest thing there is, so it goes out first and
                // the end is reported on the next call.
                Err(TryRecvError::Disconnected) => {
                    return newest.or(first).map_or(Frame::Ended, Frame::New)
                }
            }
        }
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

/// The size ffmpeg will deliver for `input`, asked of ffprobe rather than
/// assumed from the request.
///
/// `-video_size` is advisory. libavdevice's v4l2 demuxer calls `VIDIOC_S_FMT`,
/// and when the driver answers with a different size it takes that size and
/// logs the substitution at info level — which `-loglevel error` hides. Only a
/// rejected *pixel format* is an error. So a camera that cannot do the mode it
/// was asked for produces frames of some other size down a pipe that carries
/// no framing, and reading them at the requested size shears every frame with
/// nothing anywhere able to notice.
///
/// This is a *second request with the same arguments*, not a look at the one
/// the capture will make: ffprobe opens the node, negotiates, and closes, and
/// ffmpeg then negotiates again. A driver whose answer depends on something
/// that changed in between could still disagree — the honest way to close that
/// would be a container that carries its own framing, and y4m, the only one
/// ffmpeg offers that does, is YUV-only and would cost a colour conversion in
/// the shader. Two identical requests to the same driver removes the failure
/// people actually hit; the residual is written down here rather than papered
/// over.
fn granted(input: &[String]) -> Result<(u32, u32), String> {
    let mut argv: Vec<String> = ["-v", "error", "-select_streams", "v:0"]
        .map(String::from)
        .to_vec();
    argv.extend_from_slice(input);
    argv.extend(["-show_entries", "stream=width,height", "-of", "csv=s=x:p=0"].map(String::from));
    let what = format!("ffprobe {}", input.join(" "));

    let mut probe = Command::new("ffprobe")
        .args(&argv)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("{what}: cannot run ffprobe: {e}"))?;
    // On a deadline, because ffprobe has none of its own: a node that opens
    // and never streams leaves it waiting for a packet forever, and this runs
    // before there is a window, a log line or anything else to look at.
    let deadline = std::time::Instant::now() + STARTUP_TIMEOUT;
    let status = loop {
        match probe.try_wait() {
            Ok(Some(status)) => break status,
            Err(e) => return Err(format!("{what}: {e}")),
            Ok(None) if std::time::Instant::now() >= deadline => {
                let _ = probe.kill();
                let _ = probe.wait();
                return Err(format!("{what}: no answer in {STARTUP_TIMEOUT:?}"));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    if !status.success() {
        return Err(format!("{what}: {status}; ffprobe's own error is above"));
    }
    let mut stdout = String::new();
    probe
        .stdout
        .take()
        .expect("stdout is piped")
        .read_to_string(&mut stdout)
        .map_err(|e| format!("{what}: {e}"))?;
    let out = stdout;
    let bad = || format!("{what}: cannot read a size out of {out:?}");
    let (width, height) = out.trim().split_once('x').ok_or_else(bad)?;
    let size = (
        width.parse().map_err(|_| bad())?,
        height.parse().map_err(|_| bad())?,
    );
    if size.0 == 0 || size.1 == 0 {
        return Err(bad());
    }
    Ok(size)
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
        assert!(argv.windows(2).any(|w| w == ["-input_format", "mjpeg"]));
        assert!(argv.windows(2).any(|w| w == ["-framerate", "60"]));
        // After -i these would be the output's options: -video_size scales the
        // frames instead of asking the driver for the mode, and -input_format
        // is not an output option at all.
        assert!(at(&argv, "-video_size") < at(&argv, "-i"));
        assert!(at(&argv, "-input_format") < at(&argv, "-i"));
        assert!(at(&argv, "-framerate") < at(&argv, "-i"));
        // A scaler here would throw away the sensor's shape, which is the one
        // thing the zoom-to-fill needs to know.
        assert!(!argv.iter().any(|a| a == "-vf"), "{argv:?}");
    }

    /// One of ffmpeg's own generators, which is a capture device in every
    /// sense this module cares about: ffmpeg opening something named by `-f`
    /// and `-i` and handing over frames until it is killed.
    fn generator(spec: &str) -> Vec<String> {
        ["-f", "lavfi", "-i", spec].map(String::from).to_vec()
    }

    #[test]
    fn a_camera_delivers_whole_frames_of_the_size_it_negotiated() {
        // 97x53: nothing here says that size, so it can only have come from
        // the stream — which is the point, since a size taken from the request
        // instead is what shears every frame. Odd on both axes as well, so a
        // stride rounded anywhere would not divide it.
        let size = (97, 53);
        // Red, so a channel swap between ffmpeg and here cannot pass.
        let mut camera = Camera::open(&generator("color=c=red:s=97x53:r=30")).unwrap();
        assert_eq!(camera.size(), size);
        let Frame::New(frame) = camera.frame() else {
            panic!("open waits for a first frame")
        };
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
        // Half the contract is the "at once": ffprobe cannot open this and
        // exits, so the wait ends there instead of running out the ten-second
        // first-frame timeout.
        let began = std::time::Instant::now();
        let input = ["-f", "v4l2", "-i", "/dev/definitely-not-a-camera"].map(String::from);
        let Err(why) = Camera::open(&input) else {
            panic!("a device that is not there opened")
        };
        assert!(why.contains("definitely-not-a-camera"), "{why}");
        assert!(
            began.elapsed() < STARTUP_TIMEOUT / 2,
            "{:?}",
            began.elapsed()
        );
    }

    #[test]
    fn a_camera_keeps_producing_frames_after_the_first() {
        let mut camera = Camera::open(&generator("testsrc2=size=64x48:rate=30")).unwrap();
        let mut seen = 0;
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while seen < 3 && std::time::Instant::now() < deadline {
            match camera.frame() {
                Frame::New(frame) => {
                    assert_eq!(frame.len(), frame_bytes(camera.size()));
                    seen += 1;
                }
                Frame::Same => {}
                Frame::Ended => panic!("the camera ended after {seen} frames"),
            }
        }
        assert_eq!(seen, 3, "the pipe stopped after the first frame");
    }

    #[test]
    fn a_camera_that_ends_says_so_rather_than_going_quiet() {
        // A source with a duration: ffmpeg reaches the end of it and exits,
        // which is what the reader thread sees when a webcam is unplugged.
        let mut camera = Camera::open(&generator("color=c=blue:s=32x32:r=30:d=0.2")).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match camera.frame() {
                Frame::Ended => break,
                _ if std::time::Instant::now() > deadline => panic!("never reported the end"),
                _ => {}
            }
        }
    }
}
