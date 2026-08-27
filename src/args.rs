//! The command line. Three settings, so there is no argument parser here:
//! every flag takes exactly one value and an unknown one is an error, which
//! is the whole grammar.

use crate::sweep::Sweep;

#[derive(Clone, Debug, PartialEq)]
pub struct Args {
    /// The v4l2 node the camera is on.
    pub device: String,
    /// The mode to ask the camera for, in its own pixels. A *request*: a
    /// driver may answer with a different size, and that answer is what gets
    /// used and what the startup line reports.
    ///
    /// Not the display's size. The field is written one line at a time, so
    /// the whole camera frame is uploaded sixty times a second and never
    /// looked at at more than one line's worth of detail; 4K of that is
    /// 2 GB/s down a pipe for no visible gain.
    pub capture: (u32, u32),
    pub sweep: Sweep,
}

impl Default for Args {
    fn default() -> Args {
        Args {
            device: "/dev/video0".into(),
            capture: (1920, 1080),
            sweep: Sweep::LeftToRight,
        }
    }
}

pub fn usage() -> String {
    let d = Args::default();
    format!(
        "usage: slitscan [--device PATH] [--capture WxH] [--sweep DIRECTION]\n\
         \x20 --device   v4l2 node the camera is on (default {})\n\
         \x20 --capture  mode to ask the camera for (default {}x{})\n\
         \x20 --sweep    which way the writing line travels (default {})\n\
         \x20            one of: {}\n",
        d.device,
        d.capture.0,
        d.capture.1,
        d.sweep.name(),
        Sweep::spellings(),
    )
}

/// `Ok(None)` is `--help`: nothing to run, and not an error either.
pub fn parse<I: IntoIterator<Item = String>>(argv: I) -> Result<Option<Args>, String> {
    let mut args = Args::default();
    let mut argv = argv.into_iter();
    while let Some(flag) = argv.next() {
        if flag == "--help" || flag == "-h" {
            return Ok(None);
        }
        // The value is only demanded once the flag is known, so a stray word
        // is reported as the unknown flag it is rather than as the missing
        // value of something nobody wrote.
        let mut value = || {
            argv.next()
                .ok_or_else(|| format!("{flag} wants a value after it"))
        };
        match flag.as_str() {
            "--device" => args.device = value()?,
            "--capture" => args.capture = size(&value()?)?,
            "--sweep" => {
                let name = value()?;
                args.sweep = Sweep::parse(&name).ok_or_else(|| {
                    format!("unknown sweep {name}; one of {}", Sweep::spellings())
                })?;
            }
            _ => return Err(format!("unknown flag {flag}")),
        }
    }
    Ok(Some(args))
}

fn size(value: &str) -> Result<(u32, u32), String> {
    let bad = || format!("{value} is not a size like 1920x1080");
    let (w, h) = value.split_once('x').ok_or_else(bad)?;
    let (w, h) = (w.parse().map_err(|_| bad())?, h.parse().map_err(|_| bad())?);
    // Zero is the one value that gets past `parse` and then produces a
    // division by zero in the aspect fit and a texture wgpu refuses.
    if w == 0 || h == 0 {
        return Err(format!("{value} has a zero side"));
    }
    Ok((w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(argv: &[&str]) -> Result<Option<Args>, String> {
        super::parse(argv.iter().map(|a| a.to_string()))
    }

    #[test]
    fn no_flags_is_the_piece_as_it_is_installed() {
        assert_eq!(parse(&[]), Ok(Some(Args::default())));
    }

    #[test]
    fn each_flag_sets_its_own_field_and_nothing_else() {
        let d = Args::default();
        assert_eq!(
            parse(&["--device", "/dev/video2"]),
            Ok(Some(Args {
                device: "/dev/video2".into(),
                ..d.clone()
            }))
        );
        assert_eq!(
            parse(&["--capture", "1280x720"]),
            Ok(Some(Args {
                capture: (1280, 720),
                ..d.clone()
            }))
        );
        assert_eq!(
            parse(&["--sweep", "top-to-bottom"]),
            Ok(Some(Args {
                sweep: Sweep::TopToBottom,
                ..d
            }))
        );
    }

    #[test]
    fn help_is_not_a_run_and_not_an_error() {
        assert_eq!(parse(&["--help"]), Ok(None));
        assert_eq!(parse(&["--device", "/dev/video0", "-h"]), Ok(None));
    }

    #[test]
    fn what_cannot_be_run_says_so_rather_than_being_guessed_at() {
        for argv in [
            vec!["--zoom", "2"],
            vec!["--capture"],
            vec!["--capture", "1920"],
            vec!["--capture", "1920x"],
            vec!["--capture", "0x1080"],
            vec!["--sweep", "diagonal"],
            vec!["/dev/video0"],
        ] {
            let Err(why) = parse(&argv) else {
                panic!("{argv:?} was accepted")
            };
            assert!(!why.is_empty(), "{argv:?}");
        }
    }
}
