// One full-screen triangle sampling a texture through a crop rectangle. Both
// passes are this: the write pass, scissored down to a single line of the
// field, and the present pass, which is the whole field, uncropped.

struct Crop {
    scale: vec2f,
    offset: vec2f,
};

@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var source_sampler: sampler;
@group(0) @binding(2) var<uniform> crop: Crop;

struct Vertex {
    @builtin(position) position: vec4f,
    @location(0) uv: vec2f,
};

@vertex
fn vs_fullscreen(@builtin(vertex_index) index: u32) -> Vertex {
    // One triangle twice the size of the target rather than two covering it:
    // no seam down the diagonal, and no vertex buffer to bind.
    let corner = vec2f(f32((index << 1u) & 2u), f32(index & 2u));
    var vertex: Vertex;
    vertex.position = vec4f(corner * 2.0 - 1.0, 0.0, 1.0);
    // Clip space counts y up from the bottom; a texture counts v down from
    // the top.
    vertex.uv = vec2f(corner.x, 1.0 - corner.y);
    return vertex;
}

@fragment
fn fs_crop(vertex: Vertex) -> @location(0) vec4f {
    let rgb = textureSample(source, source_sampler, vertex.uv * crop.scale + crop.offset).rgb;
    // Opaque whatever the camera said, so a compositor cannot see through
    // the piece and the field a test reads back has no alpha to explain.
    return vec4f(rgb, 1.0);
}
