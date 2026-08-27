//! The whole piece on the GPU: a persistent image the size of the display,
//! one line of which is replaced with the live camera each displayed frame.
//!
//! Two textures and two passes. The camera texture holds whatever frame
//! arrived most recently. The *field* holds everything ever written and is
//! never cleared, which is what makes the piece: a scissor rectangle one
//! pixel wide is the only part of it a pass may touch, so the rest keeps what
//! some earlier second put there. Present is the same pass again with no crop
//! and no scissor.
//!
//! The tempo is the display's. One line per presented frame is the spec, so
//! nothing here has a clock — [`Field::advance`] is called once per frame and
//! the sweep is however fast the surface presents. At 3840 columns and 60 Hz
//! a pass across the screen takes just over a minute, which is the piece.

use wgpu::util::DeviceExt;

use crate::camera::frame_bytes;
use crate::sweep::{Cover, Sweep};

/// The field's own format, and the camera texture's. sRGB on both ends of the
/// write pass, so sampling decodes and the target re-encodes and the camera's
/// bytes land in the field unchanged; the piece stores pixels rather than
/// accumulating light, so eight bits is all there is to keep.
pub const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

pub struct Field {
    size: (u32, u32),
    camera_size: (u32, u32),
    sweep: Sweep,
    /// Frames presented since startup. Never reset: the wrap is a remainder,
    /// so there is no edge for anything to be off by one about.
    step: u64,
    camera: wgpu::Texture,
    field: wgpu::Texture,
    view: wgpu::TextureView,
    write: Pass,
    present: Pass,
}

/// A pipeline and what it samples. The two differ only in what is bound and
/// what format they draw to.
struct Pass {
    pipeline: wgpu::RenderPipeline,
    bind: wgpu::BindGroup,
}

impl Field {
    /// `target` is the format of whatever the present pass draws to — the
    /// surface's choice in the installation, a plain texture in the tests.
    pub fn new(
        device: &wgpu::Device,
        size: (u32, u32),
        camera_size: (u32, u32),
        sweep: Sweep,
        target: wgpu::TextureFormat,
    ) -> Field {
        let shader = device.create_shader_module(wgpu::include_wgsl!("shaders/slit.wgsl"));
        let layout = bind_group_layout(device);
        // Linear, because zoom-to-fill is a resample: the camera's pixels and
        // the field's do not line up, and nearest turns that into stair-steps
        // along every edge. Clamped, because the crop never samples outside
        // the image, so the wrap mode only shows up on the boundary texel.
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("source"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        // wgpu zero-initialises a texture before its first use, so the field
        // starts black without a clear pass and the present pass can load it
        // on the very first frame.
        let camera = texture(
            device,
            "camera",
            camera_size,
            FORMAT,
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        );
        let field = texture(
            device,
            "field",
            size,
            FORMAT,
            wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC,
        );
        let view = field.create_view(&wgpu::TextureViewDescriptor::default());

        Field {
            size,
            camera_size,
            sweep,
            step: 0,
            write: Pass::new(
                device,
                &shader,
                &layout,
                &sampler,
                &camera,
                Cover::new(size, camera_size),
                FORMAT,
                "write",
            ),
            present: Pass::new(
                device,
                &shader,
                &layout,
                &sampler,
                &field,
                // The field is already exactly the shape of the display; the
                // camera's shape was dealt with on the way in.
                Cover::WHOLE,
                target,
                "present",
            ),
            camera,
            field,
            view,
        }
    }

    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// One full pass of the writing line, in frames.
    pub fn span(&self) -> u32 {
        self.sweep.span(self.size)
    }

    /// Hands over the newest camera frame, tightly packed RGBA8 at the size
    /// this field was built for. Called only when one has arrived: a field
    /// whose camera has not produced anything keeps writing the last frame,
    /// which is what a camera slower than the display means.
    pub fn upload(&self, queue: &wgpu::Queue, frame: &[u8]) {
        assert_eq!(
            frame.len(),
            frame_bytes(self.camera_size),
            "a camera frame that is not {:?}",
            self.camera_size
        );
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.camera,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            frame,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(self.camera_size.0 * 4),
                rows_per_image: Some(self.camera_size.1),
            },
            extent(self.camera_size),
        );
    }

    /// Writes the camera onto the one line the sweep is on and moves it along.
    /// Everything else in the field is untouched, which is the whole trick:
    /// the pass draws over the entire field and the scissor is what stops it.
    pub fn advance(&mut self, encoder: &mut wgpu::CommandEncoder) {
        let (x, y, width, height) = self.sweep.line(self.size, self.step);
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("write"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    // The one load op this piece could not be written with a
                    // clear: everything outside the scissor is the past.
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_scissor_rect(x, y, width, height);
        self.write.draw(&mut pass);
        drop(pass);
        self.step += 1;
    }

    /// Draws the field onto `target`, which the caller has sized to match.
    pub fn present(&self, encoder: &mut wgpu::CommandEncoder, target: &wgpu::TextureView) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("present"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        self.present.draw(&mut pass);
    }

    /// The field itself, tightly packed RGBA8, row by row from the top. What
    /// the tests assert on and what the evidence strip is drawn from — the
    /// installation never reads it back.
    pub fn read_back(&self, device: &wgpu::Device, queue: &wgpu::Queue) -> Vec<u8> {
        let (width, height) = self.size;
        let row = width as usize * 4;
        // A texture-to-buffer copy writes rows on a 256-byte pitch whatever
        // the row's own length is, so the padding is stripped below rather
        // than wished away here.
        let pitch = row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize;
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("read back"),
            size: (pitch * height as usize) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("read back"),
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.field,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(pitch as u32),
                    rows_per_image: Some(height),
                },
            },
            extent(self.size),
        );
        queue.submit([encoder.finish()]);

        let slice = readback.slice(..);
        slice.map_async(wgpu::MapMode::Read, |r| r.expect("map read back buffer"));
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("poll");
        let mapped = slice.get_mapped_range().expect("map read back range");
        let pixels = mapped
            .chunks(pitch)
            .flat_map(|r| r[..row].to_vec())
            .collect();
        drop(mapped);
        readback.unmap();
        pixels
    }
}

impl Pass {
    #[allow(clippy::too_many_arguments)]
    fn new(
        device: &wgpu::Device,
        shader: &wgpu::ShaderModule,
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        source: &wgpu::Texture,
        crop: Cover,
        format: wgpu::TextureFormat,
        label: &str,
    ) -> Pass {
        // Written once at build time: the crop only changes when the camera
        // or the display does, and either of those rebuilds the field.
        let crop = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(label),
            contents: bytemuck::cast_slice(&[
                crop.scale[0],
                crop.scale[1],
                crop.offset[0],
                crop.offset[1],
            ]),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(
                        &source.create_view(&wgpu::TextureViewDescriptor::default()),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: crop.as_entire_binding(),
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(label),
            bind_group_layouts: &[Some(layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: shader,
                entry_point: Some("vs_fullscreen"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: shader,
                entry_point: Some("fs_crop"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        Pass { pipeline, bind }
    }

    fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind, &[]);
        pass.draw(0..3, 0..1);
    }
}

fn bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("source"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    })
}

fn texture(
    device: &wgpu::Device,
    label: &str,
    size: (u32, u32),
    format: wgpu::TextureFormat,
    usage: wgpu::TextureUsages,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: extent(size),
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage,
        view_formats: &[],
    })
}

fn extent((width, height): (u32, u32)) -> wgpu::Extent3d {
    wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    }
}
