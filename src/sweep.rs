//! Where the writing line is, and how the camera's shape is fitted to the
//! field's. Both are plain arithmetic with no GPU in them, which is the point:
//! the two things this piece can get visibly wrong are testable without one.

/// Which way the writing line travels across the field. The line is
/// perpendicular to its travel, so `LeftToRight` writes a one-pixel *column*
/// and `TopToBottom` a one-pixel *row*.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sweep {
    LeftToRight,
    RightToLeft,
    TopToBottom,
    BottomToTop,
}

impl Sweep {
    /// The spelling the command line accepts, and the one [`Sweep::name`]
    /// prints. One table, so the two cannot disagree.
    const NAMES: [(&'static str, Sweep); 4] = [
        ("left-to-right", Sweep::LeftToRight),
        ("right-to-left", Sweep::RightToLeft),
        ("top-to-bottom", Sweep::TopToBottom),
        ("bottom-to-top", Sweep::BottomToTop),
    ];

    pub fn parse(s: &str) -> Option<Sweep> {
        Sweep::NAMES.iter().find(|(n, _)| *n == s).map(|(_, s)| *s)
    }

    pub fn name(self) -> &'static str {
        Sweep::NAMES
            .iter()
            .find(|(_, s)| *s == self)
            .expect("every variant is in NAMES")
            .0
    }

    pub fn spellings() -> String {
        Sweep::NAMES
            .iter()
            .map(|(n, _)| *n)
            .collect::<Vec<_>>()
            .join(" | ")
    }

    /// How many distinct positions the line has on a field of `size`, which
    /// is also how many frames one full pass takes.
    pub fn span(self, size: (u32, u32)) -> u32 {
        match self {
            Sweep::LeftToRight | Sweep::RightToLeft => size.0,
            Sweep::TopToBottom | Sweep::BottomToTop => size.1,
        }
    }

    /// The line to write on frame `step`, as a scissor rectangle in the
    /// field's own pixels — `(x, y, width, height)`, origin top left.
    ///
    /// `step` counts frames since startup and never resets: the wrap is the
    /// remainder, so nothing has to notice the edge and there is no state to
    /// get out of step with the field.
    pub fn line(self, size: (u32, u32), step: u64) -> (u32, u32, u32, u32) {
        let span = self.span(size);
        let along = (step % span as u64) as u32;
        // The two "backwards" sweeps are the same walk read from the far end,
        // rather than a second set of cases to keep in agreement with these.
        let from_end = span - 1 - along;
        match self {
            Sweep::LeftToRight => (along, 0, 1, size.1),
            Sweep::RightToLeft => (from_end, 0, 1, size.1),
            Sweep::TopToBottom => (0, along, size.0, 1),
            Sweep::BottomToTop => (0, from_end, size.0, 1),
        }
    }
}

/// Maps a point of the field to a point of the camera image: `uv * scale +
/// offset`, both in the 0..1 texture coordinates the shader works in.
///
/// Zoom to fill, so the overhanging axis of the camera image is thrown away
/// rather than shown as bars — the same crop on both sides, so what survives
/// is what the camera was pointed at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cover {
    pub scale: [f32; 2],
    pub offset: [f32; 2],
}

impl Cover {
    /// The identity: every camera pixel, none cropped. What the present pass
    /// wants, since the field is already exactly the shape of the display.
    pub const WHOLE: Cover = Cover {
        scale: [1.0, 1.0],
        offset: [0.0, 0.0],
    };

    pub fn new(field: (u32, u32), camera: (u32, u32)) -> Cover {
        let ratio = |(w, h): (u32, u32)| w as f32 / h as f32;
        // How much of the camera's width fits the field once its height does.
        // Above 1.0 the camera is the wider of the two and loses its sides;
        // below, it is the taller and loses its top and bottom.
        let overhang = ratio(camera) / ratio(field);
        let (mut scale, mut offset) = ([1.0f32, 1.0], [0.0f32, 0.0]);
        if overhang > 1.0 {
            scale[0] = 1.0 / overhang;
            offset[0] = (1.0 - scale[0]) / 2.0;
        } else {
            scale[1] = overhang;
            offset[1] = (1.0 - scale[1]) / 2.0;
        }
        Cover { scale, offset }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIELD: (u32, u32) = (8, 4);

    /// Every position, exactly once, in order — and then again from the top.
    fn walk(sweep: Sweep, frames: u64) -> Vec<(u32, u32, u32, u32)> {
        (0..frames).map(|step| sweep.line(FIELD, step)).collect()
    }

    #[test]
    fn a_sideways_sweep_writes_one_column_and_wraps_at_the_edge() {
        let lines = walk(Sweep::LeftToRight, 10);
        assert!(lines.iter().all(|&(_, y, w, h)| (y, w, h) == (0, 1, 4)));
        let xs: Vec<u32> = lines.iter().map(|&(x, ..)| x).collect();
        // Eight columns, then back to the first: the wrap is the whole of the
        // ninth frame's behaviour.
        assert_eq!(xs, [0, 1, 2, 3, 4, 5, 6, 7, 0, 1]);
    }

    #[test]
    fn a_downward_sweep_writes_one_row_and_wraps_at_the_bottom() {
        let lines = walk(Sweep::TopToBottom, 6);
        assert!(lines.iter().all(|&(x, _, w, h)| (x, w, h) == (0, 8, 1)));
        let ys: Vec<u32> = lines.iter().map(|&(_, y, ..)| y).collect();
        assert_eq!(ys, [0, 1, 2, 3, 0, 1]);
    }

    #[test]
    fn the_reversed_sweeps_are_the_forward_ones_backwards() {
        for (forward, back) in [
            (Sweep::LeftToRight, Sweep::RightToLeft),
            (Sweep::TopToBottom, Sweep::BottomToTop),
        ] {
            let span = forward.span(FIELD) as u64;
            for step in 0..span * 2 {
                assert_eq!(
                    back.line(FIELD, step),
                    forward.line(FIELD, span - 1 - step % span),
                    "{back:?} at {step}"
                );
            }
        }
    }

    #[test]
    fn a_sweep_touches_every_line_of_the_field_before_repeating() {
        for sweep in [
            Sweep::LeftToRight,
            Sweep::RightToLeft,
            Sweep::TopToBottom,
            Sweep::BottomToTop,
        ] {
            let span = sweep.span(FIELD);
            let mut seen: Vec<(u32, u32, u32, u32)> = walk(sweep, span as u64);
            seen.sort();
            seen.dedup();
            assert_eq!(seen.len(), span as usize, "{sweep:?} missed a line");
        }
    }

    #[test]
    fn every_spelling_round_trips() {
        for (name, sweep) in Sweep::NAMES {
            assert_eq!(Sweep::parse(name), Some(sweep));
            assert_eq!(sweep.name(), name);
        }
        assert_eq!(Sweep::parse("sideways"), None);
    }

    /// Asserts the fractions of the camera image that survive the crop —
    /// which is what the numbers mean and what a reader can check by eye.
    /// Approximately, because the fit is a ratio of ratios: 0.75 comes out of
    /// it as 0.75000006 and the claim is about the crop, not about float.
    #[track_caller]
    fn keeps(field: (u32, u32), camera: (u32, u32), scale: [f32; 2], offset: [f32; 2]) {
        let cover = Cover::new(field, camera);
        let near = |a: [f32; 2], b: [f32; 2]| (0..2).all(|i| (a[i] - b[i]).abs() < 1e-5);
        assert!(
            near(cover.scale, scale) && near(cover.offset, offset),
            "{field:?} from {camera:?}: {cover:?}"
        );
    }

    #[test]
    fn a_camera_the_shape_of_the_field_is_not_cropped_at_all() {
        for camera in [(16, 9), (1280, 720), (3840, 2160)] {
            keeps((1920, 1080), camera, [1.0, 1.0], [0.0, 0.0]);
        }
    }

    #[test]
    fn a_narrow_camera_on_a_wide_field_loses_its_top_and_bottom() {
        // 4:3 into 16:9: the widths already match, so three quarters of the
        // height survives, centred — an eighth off each end.
        keeps((1920, 1080), (640, 480), [1.0, 0.75], [0.0, 0.125]);
    }

    #[test]
    fn a_wide_camera_on_a_narrow_field_loses_its_sides() {
        // 16:9 into 4:3: three quarters of the width survives.
        keeps((640, 480), (1920, 1080), [0.75, 1.0], [0.125, 0.0]);
    }

    #[test]
    fn zoom_to_fill_never_leaves_a_bar() {
        // The property the two cases above are instances of: the crop keeps
        // all of one axis and is centred on the other, so the sampled
        // rectangle is inside the camera image and touches its edges on one
        // axis. Neither can be widened without sampling past the image, which
        // is what a bar is.
        for field in [(1920, 1080), (640, 480), (1000, 1000), (3840, 2160)] {
            for camera in [(1920, 1080), (640, 480), (1000, 1000), (720, 1280)] {
                let Cover { scale, offset } = Cover::new(field, camera);
                let filled = (scale[0] - 1.0).abs() < 1e-6 || (scale[1] - 1.0).abs() < 1e-6;
                assert!(filled, "{field:?} from {camera:?}: {scale:?}");
                for axis in 0..2 {
                    assert!(scale[axis] <= 1.0 + 1e-6, "{field:?} from {camera:?}");
                    assert!(offset[axis] >= -1e-6, "{field:?} from {camera:?}");
                    assert!(
                        offset[axis] + scale[axis] <= 1.0 + 1e-6,
                        "{field:?} from {camera:?}"
                    );
                    // Centred: as much thrown away on one side as the other.
                    assert!(
                        (offset[axis] - (1.0 - scale[axis]) / 2.0).abs() < 1e-6,
                        "{field:?} from {camera:?}"
                    );
                }
            }
        }
    }
}
