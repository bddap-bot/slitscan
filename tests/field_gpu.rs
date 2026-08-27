//! The piece, on a real GPU and read back: that a line of camera lands where
//! the sweep says, that everything else keeps what an earlier frame put there,
//! that the wrap overwrites rather than stops, that the camera arrives the
//! right way up, and that one the wrong shape for the field is cropped rather
//! than squashed.
//!
//! It also writes the evidence strip — `target/evidence/*.png`, the field at
//! intervals through a pass and a bit — because what this piece has to get
//! right is what it looks like, and a picture of it is the only assertion a
//! person can check.
//!
//! There is no skip when there is no adapter. This piece is a GPU program on
//! one machine; a suite that passes having rendered nothing is worth less than
//! one that fails, because the acceptance gate cannot tell the two apart.

use std::f64::consts::TAU;
use std::io::Write;
use std::sync::OnceLock;

use slitscan::field::{read_back, Field, FORMAT};
use slitscan::frame_bytes;
use slitscan::sweep::Sweep;

/// Small enough that a whole pass of the line is a few hundred frames, and
/// 16:9 so a camera that is not has something to be cropped by.
const FIELD: (u32, u32) = (256, 144);

/// A row far enough from both edges that a crop or a flip cannot leave the
/// right answer there by accident. Only the sideways sweeps read it; the
/// downward one names its own rows.
const ROW: u32 = 70;

/// One device for the whole suite. Tests run in parallel, and standing up
/// several wgpu devices at once — then tearing them all down at once — is
/// enough to crash some drivers outright.
fn device() -> &'static (wgpu::Device, wgpu::Queue) {
    static GPU: OnceLock<(wgpu::Device, wgpu::Queue)> = OnceLock::new();
    GPU.get_or_init(|| {
        // No display handle: these render to a texture, so a machine with a
        // GPU and no screen is exactly what they are expected to run on.
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: slitscan::BACKENDS,
            ..wgpu::InstanceDescriptor::new_without_display_handle_from_env()
        });
        let adapter = pollster::block_on(instance.request_adapter(&Default::default()))
            .expect("no GPU adapter; this piece cannot be tested without one");
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("slitscan tests"),
            ..Default::default()
        }))
        .expect("an adapter exists but refused a device")
    })
}

/// A field driven by `frame(step)` for `frames` frames, then read back.
/// Exactly what the installation does per displayed frame — upload the newest
/// camera frame, write one line, move on — minus the surface.
fn run(
    sweep: Sweep,
    camera: (u32, u32),
    frames: u64,
    mut frame: impl FnMut(u64) -> Vec<u8>,
) -> Vec<u8> {
    let mut field = build(sweep, camera);
    for step in 0..frames {
        field.upload(&device().1, &frame(step));
        step_once(&mut field);
    }
    read_back(&device().0, &device().1, field.texture())
}

fn build(sweep: Sweep, camera: (u32, u32)) -> Field {
    Field::new(&device().0, FIELD, camera, sweep, FORMAT)
}

fn step_once(field: &mut Field) {
    let (device, queue) = device();
    let mut encoder = device.create_command_encoder(&Default::default());
    field.advance(&mut encoder);
    queue.submit([encoder.finish()]);
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

/// A frame drawn pixel by pixel.
fn draw(size: (u32, u32), pixel: impl Fn(u32, u32) -> [u8; 3]) -> Vec<u8> {
    let mut frame = vec![0u8; frame_bytes(size)];
    for y in 0..size.1 {
        for x in 0..size.0 {
            let i = ((y * size.0 + x) * 4) as usize;
            frame[i..i + 3].copy_from_slice(&pixel(x, y));
            frame[i + 3] = 255;
        }
    }
    frame
}

/// A distinct colour per frame: green counts in fives and red carries the
/// tens, which stays inside a byte for the few hundred frames these run.
fn stamp(step: u64) -> [u8; 3] {
    [(step / 51) as u8 * 30 + 10, (step % 51) as u8 * 5, 128]
}

#[test]
fn the_line_lands_where_the_sweep_says_and_the_rest_holds() {
    let camera = (64, 36);
    // Fewer frames than the field is wide, so nothing has wrapped: every
    // column left of the line is a different frame's colour, and every column
    // right of it has never been written.
    let frames = 100;
    let field = run(Sweep::LeftToRight, camera, frames, |step| {
        flat(camera, stamp(step))
    });
    for step in 0..frames {
        let x = step as u32;
        same(
            at(&field, (x, ROW)),
            stamp(step),
            format_args!("column {x} is not the frame that wrote it"),
        );
    }
    for x in frames as u32..FIELD.0 {
        same(
            at(&field, (x, ROW)),
            [0, 0, 0],
            format_args!("column {x} was written before the line reached it"),
        );
    }
}

#[test]
fn the_line_wraps_and_writes_over_its_own_first_pass() {
    let camera = (64, 36);
    // A quarter into a second pass, so the field carries both: the overwritten
    // head, and the tail of the first pass the line has not come back to.
    let wrapped = FIELD.0 / 4;
    let frames = FIELD.0 as u64 + wrapped as u64;
    let field = run(Sweep::LeftToRight, camera, frames, |step| {
        flat(camera, stamp(step))
    });
    for x in 0..wrapped {
        same(
            at(&field, (x, ROW)),
            stamp(FIELD.0 as u64 + x as u64),
            format_args!("column {x} kept the first pass instead of the second"),
        );
    }
    for x in wrapped..FIELD.0 {
        same(
            at(&field, (x, ROW)),
            stamp(x as u64),
            format_args!("column {x} lost the first pass before the line came back"),
        );
    }
}

#[test]
fn a_downward_sweep_writes_rows_instead_of_columns() {
    let camera = (64, 36);
    let frames = FIELD.1 as u64 / 2;
    let field = run(Sweep::TopToBottom, camera, frames, |step| {
        flat(camera, stamp(step))
    });
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
fn the_camera_arrives_the_right_way_up_and_the_right_way_round() {
    // A camera the field's own shape, so nothing is cropped and every corner
    // has somewhere to land. Four quadrants: a flip on either axis, or a swap
    // of the two, moves at least one of them.
    //
    // Nothing else in this suite can see an inversion — a flat frame looks the
    // same upside down, and the crop test only asks which stripe survived.
    let camera = (128, 72);
    let quadrants = |x: u32, y: u32| match (x < camera.0 / 2, y < camera.1 / 2) {
        (true, true) => [255, 0, 0],
        (false, true) => [0, 255, 0],
        (true, false) => [0, 0, 255],
        (false, false) => [255, 255, 255],
    };
    let field = run(Sweep::LeftToRight, camera, FIELD.0 as u64, |_| {
        draw(camera, quadrants)
    });
    // Well away from the quadrant edges, where the linear filter blends.
    for (corner, want) in [
        ((10, 10), [255, 0, 0]),
        ((FIELD.0 - 10, 10), [0, 255, 0]),
        ((10, FIELD.1 - 10), [0, 0, 255]),
        ((FIELD.0 - 10, FIELD.1 - 10), [255, 255, 255]),
    ] {
        same(
            at(&field, corner),
            want,
            format_args!("the camera is turned around at {corner:?}"),
        );
    }
}

#[test]
fn a_camera_the_wrong_shape_is_cropped_rather_than_squashed() {
    // 1:1 into 16:9: the widths already fit, so nine sixteenths of the
    // camera's height survives, centred — v from 0.219 to 0.781. Stripes in
    // the outer eighths are outside that band and stripes inside it are not,
    // so which happened is legible in the field rather than inferred.
    let camera = (64, 64);
    let stripes = |_: u32, y: u32| match y * 8 / camera.1 {
        0 => [255, 0, 0], // top eighth, cropped away
        7 => [0, 0, 255], // bottom eighth, cropped away
        _ => [0, 255, 0], // the middle, which must fill the field
    };
    let field = run(Sweep::LeftToRight, camera, 40, |_| draw(camera, stripes));
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
fn a_camera_slower_than_the_display_writes_the_frame_it_already_has() {
    // The normal case: thirty frames a second into sixty lines a second. One
    // upload and five lines, which must all be that upload — a field that
    // only wrote on a new frame would leave four columns black.
    let camera = (64, 36);
    let mut field = build(Sweep::LeftToRight, camera);
    field.upload(&device().1, &flat(camera, stamp(3)));
    for _ in 0..5 {
        step_once(&mut field);
    }
    let pixels = read_back(&device().0, &device().1, field.texture());
    for x in 0..5 {
        same(
            at(&pixels, (x, ROW)),
            stamp(3),
            format_args!("column {x} did not get the frame that was up"),
        );
    }
}

#[test]
fn a_camera_whose_rows_are_not_a_round_number_of_bytes_still_uploads() {
    // 66 pixels is 264 bytes a row, which the 256-byte copy pitch does not
    // divide. A texture-to-buffer copy would need that padded; `write_texture`
    // stages it instead, and this is what says so rather than a comment
    // claiming it — the whole reason there is no padding code here.
    let camera = (66, 50);
    let field = run(Sweep::LeftToRight, camera, 3, |step| {
        flat(camera, stamp(step))
    });
    for step in 0..3u64 {
        same(
            at(&field, (step as u32, ROW)),
            stamp(step),
            format_args!("column {step}"),
        );
    }
}

#[test]
fn the_present_pass_puts_the_field_on_a_surface_shaped_target() {
    // The one pass the installation shows and the tests otherwise never run:
    // everything else reads the field directly. Its target is the *surface's*
    // format, which on a real display is very likely BGRA rather than the
    // field's own RGBA — a pipeline built against a format nothing here
    // exercised is exactly the kind of thing that only fails on a television.
    let (device, queue) = device();
    let camera = (64, 36);
    let target_format = wgpu::TextureFormat::Bgra8UnormSrgb;
    let mut field = Field::new(device, FIELD, camera, Sweep::LeftToRight, target_format);
    for step in 0..40u64 {
        field.upload(queue, &flat(camera, stamp(step)));
        step_once(&mut field);
    }

    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("a stand-in for the surface"),
        size: wgpu::Extent3d {
            width: FIELD.0,
            height: FIELD.1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: target_format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    field.present(
        &mut encoder,
        &target.create_view(&wgpu::TextureViewDescriptor::default()),
    );
    queue.submit([encoder.finish()]);

    let shown = read_back(device, queue, &target);
    let held = read_back(device, queue, field.texture());
    for x in [0, 1, 20, 39] {
        let i = ((ROW * FIELD.0 + x) * 4) as usize;
        // Raw bytes of a BGRA texture are blue first; the picture is the same
        // one, written in the other order.
        same(
            [shown[i + 2], shown[i + 1], shown[i]],
            [held[i], held[i + 1], held[i + 2]],
            format_args!("column {x} came out of the present pass changed"),
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
/// is what no single frame could produce. The wave breaking a quarter of the
/// way across the last picture is the wrap writing over the first pass.
#[test]
fn the_field_looks_like_a_slit_scan() {
    let (device, queue) = device();
    let camera = (128, 72);
    // A period that does not divide the field's width, so the second pass
    // arrives out of phase with the first and the wrap leaves a step in the
    // wave rather than joining it invisibly.
    let period = 96.0;
    let moving = |step: u64| {
        let swing = (camera.1 as f64 / 2.0) - 5.0;
        let bar = (camera.1 as f64 / 2.0) + swing * (step as f64 * TAU / period).sin();
        draw(camera, |x, y| {
            // The background is still: everything that moves in the camera is
            // the bar, so everything that varies along the field's x axis came
            // from a different moment.
            if (y as f64 - bar).abs() < 2.5 {
                [255, 255, 220]
            } else {
                [24, (x * 60 / camera.0) as u8, (y * 90 / camera.1) as u8]
            }
        })
    };

    let dir = target_dir().join("evidence");
    std::fs::create_dir_all(&dir).expect("make the evidence directory");
    let frames = FIELD.0 as u64 * 5 / 4;
    let every = FIELD.0 as u64 / 8;
    let mut field = build(Sweep::LeftToRight, camera);
    let mut written = 0;
    for step in 0..frames {
        field.upload(queue, &moving(step));
        step_once(&mut field);
        // Every eighth of a pass, and the last frame, which is a quarter into
        // the second pass and is where the wrap shows.
        if (step + 1) % every == 0 || step + 1 == frames {
            let path = dir.join(format!("frame-{:04}.png", step + 1));
            write_png(&path, FIELD, &read_back(device, queue, field.texture()));
            written += 1;
        }
    }
    assert_eq!(
        written,
        frames.div_ceil(every) as usize,
        "a frame went unsaved"
    );

    // The strip is evidence, not the assertion. This is: a quarter into the
    // second pass the wave is at a different height either side of the line,
    // because those two columns were written a whole pass apart. No frame of
    // this camera has a step in it.
    let pixels = read_back(device, queue, field.texture());
    let wrapped = FIELD.0 / 4;
    let height = |x: u32| {
        (0..FIELD.1)
            .find(|&y| at(&pixels, (x, y))[1] > 200)
            .expect("the bar is somewhere in every column")
    };
    assert!(
        height(wrapped - 1).abs_diff(height(wrapped + 1)) > 10,
        "the wrap left no step in the wave: {} then {}",
        height(wrapped - 1),
        height(wrapped + 1)
    );
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
