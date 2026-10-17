use crate::{Gpu, Image};
use wgpu::util::DeviceExt;

/// Source-channel interpretation for a raw, unquantized grayscale mask.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MaskSourceKind {
    Luminosity { low: f32, high: f32 },
    Transparency,
    Outlines,
}

impl Gpu {
    /// Build an RGBA32F mask at source resolution, before resampling or combining.
    /// RGB contains the clamped mask value and alpha is always one.
    pub fn mask_from_source(&self, src: &Image, kind: MaskSourceKind) -> Image {
        let pipeline = self.mask_source.get_or_init(|| {
            let module = self.device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("Moya source mask"),
                source: wgpu::ShaderSource::Wgsl(include_str!("masks.wgsl").into()),
            });
            let bindings = self.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: None,
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: false },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::StorageTexture {
                            access: wgpu::StorageTextureAccess::WriteOnly,
                            format: wgpu::TextureFormat::Rgba32Float,
                            view_dimension: wgpu::TextureViewDimension::D2,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });
            let layout = self.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: &[Some(&bindings)],
                immediate_size: 0,
            });
            self.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("Moya source mask"),
                layout: Some(&layout),
                module: &module,
                entry_point: Some("main"),
                compilation_options: Default::default(),
                cache: None,
            })
        });
        let params = match kind {
            MaskSourceKind::Luminosity { low, high } => [0, low.to_bits(), high.to_bits(), 0],
            MaskSourceKind::Transparency => [1, 0, 0, 0],
            MaskSourceKind::Outlines => [2, 0, 0, 0],
        };
        let uniform = self.track_buffer(self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Moya source mask parameters"),
            contents: bytemuck::cast_slice(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        }));
        let image = self.image(src.width(), src.height());
        let source_view = src.texture.create_view(&Default::default());
        let target_view = image.texture.create_view(&Default::default());
        let group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&source_view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&target_view) },
                wgpu::BindGroupEntry { binding: 2, resource: uniform.as_entire_binding() },
            ],
        });
        self.record_buffers(None, &[uniform], |encoder| {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(src.width().div_ceil(16), src.height().div_ceil(16), 1);
        });
        image
    }
}
