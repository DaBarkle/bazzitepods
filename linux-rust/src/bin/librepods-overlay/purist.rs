//! The purist island's body, key line and border light, drawn by one small fragment
//! shader (`purist.wgsl`) under the canvas that draws the content. The shader runs
//! only while iced redraws, which is only while something moves, so a still island
//! costs nothing.

use std::sync::atomic::{AtomicBool, Ordering};

use iced::widget::shader::{self, Viewport};
use iced::{Rectangle, mouse, wgpu};

/// Set once the GPU has drawn a purist body. Until then (or if iced fell back to its
/// software renderer, which can't run shaders) the canvas paints a plain body itself.
static GPU_DRAWS: AtomicBool = AtomicBool::new(false);

pub fn gpu_draws() -> bool {
    GPU_DRAWS.load(Ordering::Relaxed)
}

/// What one frame of the body looks like, in logical px of the surface.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Body {
    pub center: (f32, f32),
    pub size: (f32, f32),
    pub radius: f32,
    /// Logical px per island unit (the size setting).
    pub unit: f32,
    /// Overall opacity of body and light.
    pub alpha: f32,
    /// Key line colour (sRGB) and opacity.
    pub key: [f32; 4],
    /// Glow colour (sRGB) and strength (above 1.5: use the spectrum instead).
    pub glow_color: [f32; 4],
    /// 0 none, 1 sweep, 2 breathe, 3 flow, 4 ping.
    pub glow_kind: f32,
    pub glow_progress: f32,
    pub glow_age: f32,
    /// True when the island hangs from the top of the screen.
    pub top: bool,
}

impl<Message> shader::Program<Message> for Body {
    type State = ();
    type Primitive = Primitive;

    fn draw(&self, _state: &(), _cursor: mouse::Cursor, _bounds: Rectangle) -> Primitive {
        Primitive(*self)
    }
}

#[derive(Debug)]
pub struct Primitive(Body);

const UNIFORM_FLOATS: usize = 24;

impl Primitive {
    fn uniforms(&self, bounds: &Rectangle, scale: f32, srgb: bool) -> [f32; UNIFORM_FLOATS] {
        let b = &self.0;
        [
            b.center.0 * scale,
            b.center.1 * scale,
            b.size.0 * scale / 2.0,
            b.size.1 * scale / 2.0,
            b.radius * scale,
            b.unit * scale,
            b.alpha.clamp(0.0, 1.0),
            if srgb { 1.0 } else { 0.0 },
            b.key[0],
            b.key[1],
            b.key[2],
            b.key[3],
            b.glow_color[0],
            b.glow_color[1],
            b.glow_color[2],
            b.glow_color[3],
            b.glow_kind,
            b.glow_progress,
            b.glow_age,
            if b.top { 1.0 } else { -1.0 },
            bounds.x * scale,
            bounds.y * scale,
            0.0,
            0.0,
        ]
    }
}

impl shader::Primitive for Primitive {
    type Pipeline = Pipeline;

    fn prepare(
        &self,
        pipeline: &mut Pipeline,
        _device: &wgpu::Device,
        queue: &wgpu::Queue,
        bounds: &Rectangle,
        viewport: &Viewport,
    ) {
        let floats = self.uniforms(bounds, viewport.scale_factor(), pipeline.srgb);
        let bytes: Vec<u8> = floats.iter().flat_map(|f| f.to_ne_bytes()).collect();
        queue.write_buffer(&pipeline.uniforms, 0, &bytes);
    }

    fn draw(&self, pipeline: &Pipeline, pass: &mut wgpu::RenderPass<'_>) -> bool {
        pass.set_pipeline(&pipeline.pipeline);
        pass.set_bind_group(0, &pipeline.bind_group, &[]);
        pass.draw(0..3, 0..1);
        GPU_DRAWS.store(true, Ordering::Relaxed);
        true
    }
}

pub struct Pipeline {
    pipeline: wgpu::RenderPipeline,
    uniforms: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    srgb: bool,
}

impl shader::Pipeline for Pipeline {
    fn new(device: &wgpu::Device, _queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("purist island"),
            source: wgpu::ShaderSource::Wgsl(include_str!("purist.wgsl").into()),
        });
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("purist island uniforms"),
            size: (UNIFORM_FLOATS * 4) as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("purist island"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("purist island"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniforms.as_entire_binding(),
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("purist island"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("purist island"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview: None,
            cache: None,
        });
        Self {
            pipeline,
            uniforms,
            bind_group,
            srgb: format.is_srgb(),
        }
    }
}
