//! Window, surface and the run loop.

use std::sync::Arc;
use std::time::Duration;

use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Fullscreen, Window, WindowId};

use crate::args::Args;
use crate::camera::{Camera, Frame};
use crate::field::Field;

/// How long to wait before trying again when the surface will not hand over a
/// texture. `Fifo` blocking in `get_current_texture` is the loop's only pacing
/// — every path that returns without presenting skips it — so a screen that
/// has gone away would otherwise be a busy loop reconfiguring a 4K swapchain
/// thousands of times a second.
const RETRY: Duration = Duration::from_millis(16);

struct Live {
    /// The two settings the render path needs, held here rather than reached
    /// for through the command line: a resize rebuilds the field from them.
    capture: (u32, u32),
    sweep: crate::sweep::Sweep,
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    field: Field,
}

/// Why the piece stopped by itself. Reported by exit status, because a
/// television has no other channel and something has to be able to notice.
struct Stopped(String);

pub struct App {
    capture: (u32, u32),
    sweep: crate::sweep::Sweep,
    camera: Camera,
    live: Option<Live>,
    stopped: Option<Stopped>,
}

/// The camera is opened before the window is, so a device that will not open
/// is an error on the terminal rather than a black screen on a television.
pub fn run(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    let camera = Camera::open(&crate::camera::v4l2(&args.device, args.capture))?;
    log::info!(
        "camera: {} at {:?} (asked for {:?})",
        args.device,
        camera.size(),
        args.capture
    );
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App {
        capture: camera.size(),
        sweep: args.sweep,
        camera,
        live: None,
        stopped: None,
    };
    event_loop.run_app(&mut app)?;
    match app.stopped {
        Some(Stopped(why)) => Err(why.into()),
        None => Ok(()),
    }
}

impl Live {
    /// Every `expect` here is a device that is not there at all — no window
    /// system, no adapter, no surface. There is nothing to recover to and
    /// `ApplicationHandler` cannot return an error, so they are panics; the
    /// failures that can happen to a *running* piece go through [`Stopped`].
    async fn new(
        event_loop: &ActiveEventLoop,
        capture: (u32, u32),
        sweep: crate::sweep::Sweep,
    ) -> Live {
        let window = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title("slitscan")
                        // Whatever the compositor gives is the field: the
                        // piece is the screen it is installed on.
                        .with_fullscreen(Some(Fullscreen::Borderless(None))),
                )
                .expect("create window"),
        );

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: crate::BACKENDS,
            // Some backends need a display handle before any surface exists.
            ..wgpu::InstanceDescriptor::new_with_display_handle_from_env(Box::new(window.clone()))
        });
        let surface = instance
            .create_surface(window.clone())
            .expect("create surface");
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                ..Default::default()
            })
            .await
            .expect("no GPU adapter can draw to this window");
        log::info!("adapter: {:?}", adapter.get_info());

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("slitscan"),
                ..Default::default()
            })
            .await
            .expect("request device");

        let caps = surface.get_capabilities(&adapter);
        // The field is sRGB and the present pass writes what it read, so an
        // sRGB surface makes that a round trip. A linear one would take the
        // decoded values raw and wash the picture out.
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or_else(|| panic!("no sRGB surface format among {:?}", caps.formats));
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            // Auto resolves to sRGB for an sRGB format, which is the round
            // trip the format was chosen for.
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: size.width.max(1),
            height: size.height.max(1),
            // The display's cadence *is* the sweep's tempo — one line per
            // presented frame — so the piece runs at the refresh rate and
            // waiting for the vertical blank is what paces it. It is also the
            // only thing pacing the run loop; see RETRY.
            present_mode: wgpu::PresentMode::Fifo,
            // With one line written per present, this is how many frames
            // behind the moment the live line is. Two is the usual balance of
            // latency against throughput; here it is also an artistic choice,
            // and a smaller number would put a fresher line on the glass.
            desired_maximum_frame_latency: 2,
            // Every fragment the present pass writes is opaque, so what the
            // compositor does with alpha cannot change the picture.
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&device, &config);

        let field = Field::new(
            &device,
            (config.width, config.height),
            capture,
            sweep,
            format,
        );
        log::info!(
            "field: {}x{}, sweeping {}, one pass every {} frames",
            config.width,
            config.height,
            sweep.name(),
            field.span(),
        );

        Live {
            capture,
            sweep,
            window,
            surface,
            device,
            queue,
            config,
            field,
        }
    }

    /// A resize rebuilds the field at the new size, which empties it. The
    /// field is the screen's own pixels — there is no meaning to carry across
    /// a change of how many there are. It happens once, when the compositor
    /// hands the window its real size at startup; anything later is a display
    /// mode change and worth a log line, since it wipes the piece.
    fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 || (width, height) == self.field.size() {
            return;
        }
        log::info!("resized to {width}x{height}; the field starts again from black");
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
        self.field = Field::new(
            &self.device,
            (width, height),
            self.capture,
            self.sweep,
            self.config.format,
        );
    }

    fn render(&mut self, camera: &mut Camera) -> Result<(), Stopped> {
        use wgpu::CurrentSurfaceTexture as Cst;
        let frame = match self.surface.get_current_texture() {
            // Suboptimal still hands back a usable texture; the compositor
            // scales what it is given until something reconfigures.
            Cst::Success(frame) | Cst::Suboptimal(frame) => frame,
            // The surface goes stale on a display mode change. Reconfiguring
            // and skipping one frame is the whole recovery.
            Cst::Outdated => {
                self.surface.configure(&self.device, &self.config);
                std::thread::sleep(RETRY);
                return Ok(());
            }
            // Not recoverable by reconfiguring: wgpu's own advice for a lost
            // surface is to build a new one, or a new device. Stopping is
            // louder than an unpaced loop that will never come back.
            Cst::Lost => return Err(Stopped("the surface was lost".into())),
            other => {
                log::warn!("dropped a frame: {other:?}");
                std::thread::sleep(RETRY);
                return Ok(());
            }
        };
        let target = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        match camera.frame() {
            Frame::New(pixels) => self.field.upload(&self.queue, &pixels),
            // The camera is slower than the display, so most frames write
            // their line from the frame already up there.
            Frame::Same => {}
            Frame::Ended => return Err(Stopped("the camera ended".into())),
        }
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame"),
            });
        self.field.advance(&mut encoder);
        self.field.present(&mut encoder, &target);
        self.queue.submit([encoder.finish()]);
        self.queue.present(frame);
        Ok(())
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.live.is_some() {
            return;
        }
        // The one place this program blocks.
        let live = pollster::block_on(Live::new(event_loop, self.capture, self.sweep));
        live.window.request_redraw();
        self.live = Some(live);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => live.resize(size.width, size.height),
            WindowEvent::RedrawRequested => {
                if let Err(stopped) = live.render(&mut self.camera) {
                    log::error!("stopping: {}", stopped.0);
                    self.stopped = Some(stopped);
                    event_loop.exit();
                    return;
                }
                live.window.request_redraw();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed
                    && event.physical_key == PhysicalKey::Code(KeyCode::Escape)
                {
                    event_loop.exit();
                }
            }
            _ => {}
        }
    }
}
