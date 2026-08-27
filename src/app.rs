//! Window, surface and the run loop.

use std::sync::Arc;

use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Fullscreen, Window, WindowId};

use crate::args::Args;
use crate::camera::Camera;
use crate::field::Field;

struct Live {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    field: Field,
}

pub struct App {
    args: Args,
    camera: Camera,
    live: Option<Live>,
}

/// The camera is opened before the window is, so a device that will not open
/// is an error on the terminal rather than a black screen on a television.
pub fn run(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    let camera = Camera::open(
        &crate::camera::v4l2(&args.device, args.capture),
        args.capture,
    )?;
    log::info!("camera: {} at {:?}", args.device, args.capture);
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    Ok(event_loop.run_app(&mut App {
        args,
        camera,
        live: None,
    })?)
}

impl Live {
    async fn new(event_loop: &ActiveEventLoop, args: &Args) -> Live {
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
            // Auto resolves to sRGB for an sRGB format, which is the round trip
            // the format was chosen for.
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: size.width.max(1),
            height: size.height.max(1),
            // The display's cadence *is* the sweep's tempo — one line per
            // presented frame — so the piece runs at the refresh rate and
            // waiting for the vertical blank is what paces it. Anything that
            // drops or repeats frames would make the sweep stutter.
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&device, &config);

        let field = Field::new(
            &device,
            (config.width, config.height),
            args.capture,
            args.sweep,
            format,
        );
        log::info!(
            "field: {}x{}, sweeping {}, one pass every {} frames",
            config.width,
            config.height,
            args.sweep.name(),
            field.span(),
        );

        Live {
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
    /// a change of how many there are — and the installation never resizes.
    fn resize(&mut self, args: &Args, width: u32, height: u32) {
        if width == 0 || height == 0 || (width, height) == self.field.size() {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
        self.field = Field::new(
            &self.device,
            (width, height),
            args.capture,
            args.sweep,
            self.config.format,
        );
    }

    fn render(&mut self, camera: &mut Camera) {
        use wgpu::CurrentSurfaceTexture as Cst;
        let frame = match self.surface.get_current_texture() {
            // Suboptimal still hands back a usable texture, and the next
            // resize reconfigures the surface anyway.
            Cst::Success(frame) | Cst::Suboptimal(frame) => frame,
            // The surface goes stale on a monitor change and on compositor
            // restarts. Reconfiguring and skipping one frame is the whole
            // recovery.
            Cst::Outdated | Cst::Lost => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
            other => {
                log::warn!("dropped a frame: {other:?}");
                return;
            }
        };
        let target = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        // The camera runs slower than the display, so most frames have
        // nothing new and write the line from the frame already up there.
        if let Some(pixels) = camera.frame() {
            self.field.upload(&self.queue, &pixels);
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
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.live.is_some() {
            return;
        }
        // The one place this program blocks.
        let live = pollster::block_on(Live::new(event_loop, &self.args));
        live.window.request_redraw();
        self.live = Some(live);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => live.resize(&self.args, size.width, size.height),
            WindowEvent::RedrawRequested => {
                live.render(&mut self.camera);
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
