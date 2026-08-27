//! The piece, on a real GPU and read back: that a line of camera lands where
//! the sweep says, that everything else keeps what an earlier frame put there,
//! that the wrap overwrites rather than stops, and that a camera the wrong
//! shape for the field is cropped rather than squashed.
//!
//! It also writes the evidence strip — `target/evidence/*.png`, the field at
//! intervals through two passes of the line — because what this piece has to
//! get right is what it looks like, and a picture of it is the only assertion
//! a person can check.
//!
//! On a machine with no adapter each test prints why and returns. The message
//! goes straight to the process's stderr, since libtest swallows `eprintln!`
//! from a passing test and a skip nobody sees is a silent pass.

use std::f64::consts::TAU;
use std::io::Write;
use std::sync::OnceLock;

use slitscan::camera::frame_bytes;
use slitscan::field::{Field, FORMAT};
use slitscan::sweep::Sweep;

/// Small enough that a whole pass of the line is a few hundred frames, and
/// 16:9 so a camera that is not has something to be cropped by.
const FIELD: (u32, u32) = (256, 144);

/// Why there is no device. Only the first is a reason to let a test pass.
#[derive(Debug)]
enum NoGpu {
    NoAdapter(String),
    DeviceRefused(String),
}

/// One device for the whole suite. Tests run in parallel, and standing up
/// several wgpu devices at once — then tearing them all down at once — is
/// enough to crash some drivers outright.
fn gpu() -> &'static Result<(wgpu::Device, wgpu::Queue), NoGpu> {
    static GPU: OnceLock<Result<(wgpu::Device, wgpu::Queue), NoGpu>> = OnceLock::new();
    GPU.get_or_init(|| {
        // No display handle: these render to a texture, so a machine with a
        // GPU and no screen is exactly what they are expected to run on.
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: slitscan::BACKENDS,
            ..wgpu::InstanceDescriptor::new_without_display_handle_from_env()
        });
        let adapter = pollster::block_on(instance.request_adapter(&Default::default()))
            .map_err(|e| NoGpu::NoAdapter(e.to_string()))?;
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("slitscan tests"),
            ..Default::default()
        }))
        .map_err(|e| NoGpu::DeviceRefused(e.to_string()))
    })
}

/// `None` when there is no adapter to run on, having said so on stderr. A
/// device that exists and *refuses* is a failure, not a skip: that is a bug
/// here, not an absent GPU.
fn device() -> Option<&'static (wgpu::Device, wgpu::Queue)> {
    match gpu() {
        Ok(gpu) => Some(gpu),
        Err(NoGpu::NoAdapter(why)) => {
            let _ = writeln!(std::io::stderr(), "SKIPPED: no GPU adapter ({why})");
            None
        }
        Err(NoGpu::DeviceRefused(why)) => panic!("an adapter exists but refused a device: {why}"),
    }
}

/// A field driven by `frame(step)` for `frames` frames, then read back.
/// Exactly what the installation does per displayed frame — upload the newest
/// camera frame, write one line, move on — minus the surface.
fn run(
    sweep: Sweep,
    camera: (u32, u32),
    frames: u64,
    mut frame: impl FnMut(u64) -> Vec<u8>,
    mut watch: impl FnMut(u64, &Field),
) -> Vec<u8> {
    let (device, queue) = device().expect("every caller checks first");
    let mut field = Field::new(device, FIELD, camera, sweep, FORMAT);
    for step in 0..frames {
        field.upload(queue, &frame(step));
        let mut encoder = device.create_command_encoder(&Default::default());
        field.advance(&mut encoder);
        queue.submit([encoder.finish()]);
        watch(step, &field);
    }
    field.read_back(device, queue)
}

/// The pixel at `(x, y)` of a read-back field, RGB.
fn at(pixels: &[u8], (x, y): (u32, u32)) -> [u8; 3] {
    let i = ((y * FIELD.0 + x) * 4) as usize;
    [pixels[i], pixels[i + 1], pixels[i + 2]]
}

/// Read-back colours are compared with a pixel of slack. Everything in the
/// field has been through an sRGB decode and re-encode, an identity on paper
/// and a rounding on hardware; the stamps below are five apart, so a slack of
/// two still tells any two of them apart.
#[track_caller]
fn same(got: [u8; 3], want: [u8; 3], what: std::fmt::Arguments) {
    assert!(
        (0..3).all(|c| got[c].abs_diff(want[c]) <= 2),
        "{what}: {got:?} is not {want:?}"
    );
}

/// A frame of one colour, so what the field kept can be read straight off a
/// pixel with none of the crop's arithmetic in the way.
fn flat(size: (u32, u32), rgb: [u8; 3]) -> Vec<u8> {
    [rgb[0], rgb[1], rgb[2], 255]
        .iter()
        .copied()
        .cycle()
        .take(frame_bytes(size))
        .collect()
}

/// A distinct colour per frame: green counts in fives and red carries the
/// tens, which stays inside a byte for the few hundred frames these run.
fn stamp(step: u64) -> [u8; 3] {
    [(step / 51) as u8 * 30 + 10, (step % 51) as u8 * 5, 128]
}

#[test]
fn the_line_lands_where_the_sweep_says_and_the_rest_holds() {
    if device().is_none() {
        return;
    }
    let camera = (64, 36);
    // Fewer frames than the field is wide, so nothing has wrapped: every
    // column left of the line is a different frame's colour, and every column
    // right of it has never been written.
    let frames = 100;
    let field = run(
        Sweep::LeftToRight,
        camera,
        frames,
        |step| flat(camera, stamp(step)),
        |_, _| {},
    );
    for step in 0..frames {
        let x = step as u32;
        same(
            at(&field, (x, 70)),
            stamp(step),
            format_args!("column {x} is not the frame that wrote it"),
        );
    }
    for x in frames as u32..FIELD.0 {
        same(
            at(&field, (x, 70)),
            [0, 0, 0],
            format_args!("column {x} was written before the line reached it"),
        );
    }
}

#[test]
fn the_line_wraps_and_writes_over_its_own_first_pass() {
    if device().is_none() {
        return;
    }
    let camera = (64, 36);
    // A quarter into a second pass, so the field carries both: the overwritten
    // head, and the tail of the first pass the line has not come back to.
    let wrapped = FIELD.0 / 4;
    let frames = FIELD.0 as u64 + wrapped as u64;
    let field = run(
        Sweep::LeftToRight,
        camera,
        frames,
        |step| flat(camera, stamp(step)),
        |_, _| {},
    );
    for x in 0..wrapped {
        same(
            at(&field, (x, 70)),
            stamp(FIELD.0 as u64 + x as u64),
            format_args!("column {x} kept the first pass instead of the second"),
        );
    }
    for x in wrapped..FIELD.0 {
        same(
            at(&field, (x, 70)),
            stamp(x as u64),
            format_args!("column {x} lost the first pass before the line came back"),
        );
    }
}

#[test]
fn a_downward_sweep_writes_rows_instead_of_columns() {
    if device().is_none() {
        return;
    }
    let camera = (64, 36);
    let frames = FIELD.1 as u64 / 2;
    let field = run(
        Sweep::TopToBottom,
        camera,
        frames,
        |step| flat(camera, stamp(step)),
        |_, _| {},
    );
    for step in 0..frames {
        let y = step as u32;
        // Both ends of the row, since a sweep that wrote a column instead
        // would have one of them right by luck.
        for x in [7, 200] {
            same(
                at(&field, (x, y)),
                stamp(step),
                format_args!("row {y} at {x}"),
            );
        }
    }
    same(
        at(&field, (7, frames as u32)),
        [0, 0, 0],
        format_args!("the sweep wrote past the line it was on"),
    );
}

#[test]
fn a_camera_the_wrong_shape_is_cropped_rather_than_squashed() {
    if device().is_none() {
        return;
    }
    // 1:1 into 16:9: the widths already fit, so nine sixteenths of the
    // camera's height survives, centred — v from 0.219 to 0.781. Stripes in
    // the outer eighths are outside that band and stripes inside it are not,
    // so which happened is legible in the field rather than inferred.
    let camera = (64, 64);
    let stripes = |_| {
        let mut frame = vec![0u8; frame_bytes(camera)];
        for y in 0..camera.1 {
            let rgb = match y * 8 / camera.1 {
                0 => [255, 0, 0], // top eighth, cropped away
                7 => [0, 0, 255], // bottom eighth, cropped away
                _ => [0, 255, 0], // the middle, which must fill the field
            };
            for x in 0..camera.0 {
                let i = ((y * camera.0 + x) * 4) as usize;
                frame[i..i + 3].copy_from_slice(&rgb);
                frame[i + 3] = 255;
            }
        }
        frame
    };
    let field = run(Sweep::LeftToRight, camera, 40, stripes, |_, _| {});
    for y in 0..FIELD.1 {
        let rgb = at(&field, (20, y));
        assert!(
            rgb[1] > 200 && rgb[0] < 40 && rgb[2] < 40,
            "row {y} is {rgb:?}: a cropped stripe reached the field"
        );
    }
    // Squashing would have fitted the whole 1:1 image into the field, putting
    // the red and blue eighths on the field's own outermost rows — which the
    // rows above say are green.
}

#[test]
fn a_camera_whose_rows_are_not_a_round_number_of_bytes_still_uploads() {
    if device().is_none() {
        return;
    }
    // 66 pixels is 264 bytes a row, which the 256-byte copy pitch does not
    // divide. A texture-to-buffer copy would need that padded; `write_texture`
    // stages it instead, and this is what says so rather than a comment
    // claiming it.
    let camera = (66, 50);
    let field = run(
        Sweep::LeftToRight,
        camera,
        3,
        |step| flat(camera, stamp(step)),
        |_, _| {},
    );
    for step in 0..3u64 {
        same(
            at(&field, (step as u32, 70)),
            stamp(step),
            format_args!("column {step}"),
        );
    }
}

/// The evidence strip: a camera pointed at something moving, run through a
/// pass of the line and a quarter of the next, with the field saved at
/// intervals.
///
/// What the camera shows is a bright bar sliding up and down over a still
/// background. One camera frame has one bar in it; the field ends up with a
/// whole wave, because the field's x axis is time — which is the piece, and
/// is what no single frame could produce. The wave breaking at a quarter of
/// the way across the last picture is the wrap writing over the first pass.
#[test]
fn the_field_looks_like_a_slit_scan() {
    let Some((device, queue)) = device() else {
        return;
    };
    let camera = (128, 72);
    // A period that does not divide the field's width, so the second pass
    // arrives out of phase with the first and the wrap leaves a step in the
    // wave rather than joining it invisibly.
    let period = 96.0;
    let moving = |step: u64| {
        let mut frame = vec![0u8; frame_bytes(camera)];
        let swing = (camera.1 as f64 / 2.0) - 5.0;
        let bar = (camera.1 as f64 / 2.0) + swing * (step as f64 * TAU / period).sin();
        for y in 0..camera.1 {
            for x in 0..camera.0 {
                // The background is still: everything that moves in the
                // camera is the bar, so everything that varies along the
                // field's x axis came from a different moment.
                let rgb = if (y as f64 - bar).abs() < 2.5 {
                    [255, 255, 220]
                } else {
                    [24, (x * 60 / camera.0) as u8, (y * 90 / camera.1) as u8]
                };
                let i = ((y * camera.0 + x) * 4) as usize;
                frame[i..i + 3].copy_from_slice(&rgb);
                frame[i + 3] = 255;
            }
        }
        frame
    };

    let dir = target_dir().join("evidence");
    std::fs::create_dir_all(&dir).expect("make the evidence directory");
    let mut written = Vec::new();
    let frames = FIELD.0 as u64 * 5 / 4;
    let every = FIELD.0 as u64 / 8;
    let field = run(Sweep::LeftToRight, camera, frames, moving, |step, field| {
        // Every eighth of a pass, and the last frame, which is a quarter into
        // the second pass and is where the wrap shows.
        if (step + 1) % every != 0 && step + 1 != frames {
            return;
        }
        let path = dir.join(format!("frame-{:04}.png", step + 1));
        write_png(&path, FIELD, &field.read_back(device, queue));
        written.push(path);
    });

    // The strip is evidence, not the assertion. This is: a quarter into the
    // second pass the wave is at a different height either side of the line,
    // because those two columns were written a whole pass apart. No frame of
    // this camera has a step in it.
    let wrapped = FIELD.0 / 4;
    let height = |x: u32| {
        (0..FIELD.1)
            .find(|&y| at(&field, (x, y))[1] > 200)
            .expect("the bar is somewhere in every column")
    };
    assert!(
        height(wrapped - 1).abs_diff(height(wrapped + 1)) > 10,
        "the wrap left no step in the wave: {} then {}",
        height(wrapped - 1),
        height(wrapped + 1)
    );
    assert_eq!(written.len(), 10, "wrong number of evidence frames");
    let _ = writeln!(std::io::stderr(), "evidence: {}", dir.display());
}
/// Cargo's target directory, found from this binary rather than from an
/// environment variable Cargo only sets when someone else already has:
/// `<target>/<profile>/deps/<this test>`.
fn target_dir() -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("this test's own path");
    exe.ancestors()
        .nth(3)
        .expect("a test binary is three deep in the target directory")
        .to_path_buf()
}

/// Written without the alpha channel. The field starts out transparent black
/// and only what has been written is opaque, so a viewer compositing the strip
/// over white would show the part nothing has reached yet as the brightest
/// thing in the picture.
fn write_png(path: &std::path::Path, (width, height): (u32, u32), rgba: &[u8]) {
    let rgb: Vec<u8> = rgba
        .chunks_exact(4)
        .flat_map(|p| &p[..3])
        .copied()
        .collect();
    let file = std::fs::File::create(path).expect("create png");
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .expect("png header")
        .write_image_data(&rgb)
        .expect("png data");
}
