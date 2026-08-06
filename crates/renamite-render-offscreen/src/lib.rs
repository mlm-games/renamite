//! Canonical offscreen export through Repose's production WGPU renderer.
//!
//! [`OffscreenRenderer`] owns a headless WGPU adapter/device, a
//! [`repose_render_wgpu::WgpuSceneRenderer`] built via
//! `from_device`, and an offscreen RGBA8-sRGB target. Rendering a
//! `repose_core::Scene` produces a tightly packed RGBA8 buffer
//! ([`OffscreenRenderer::render_rgba`]) or a PNG
//! ([`OffscreenRenderer::render_png`]).

use anyhow::Context;
use renamite_behavior_common::ViewTransform;
use repose_core::Scene;
use repose_render_wgpu::WgpuSceneRenderer;

/// Build a `ViewTransform` that letterboxes `artboard` into a `w×h` frame.
pub fn fit_view(artboard: (u32, u32), w: u32, h: u32) -> ViewTransform {
    let scale = (w as f64 / artboard.0.max(1) as f64)
        .min(h as f64 / artboard.1.max(1) as f64)
        .max(1e-6);
    ViewTransform {
        scale,
        offset: glam::DVec2::new(
            (w as f64 - artboard.0 as f64 * scale) * 0.5,
            (h as f64 - artboard.1 as f64 * scale) * 0.5,
        ),
    }
}

pub struct OffscreenRenderer {
    renderer: WgpuSceneRenderer,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    readback: wgpu::Buffer,

    width: u32,
    height: u32,
    padded_bytes_per_row: u32,
    #[allow(dead_code)]
    format: wgpu::TextureFormat,
}

impl OffscreenRenderer {
    /// Blocking convenience wrapper around [`OffscreenRenderer::new`].
    pub fn new_blocking(width: u32, height: u32, msaa: u32) -> anyhow::Result<Self> {
        pollster::block_on(Self::new(width, height, msaa))
    }

    /// Create a headless renderer-sized target. Adapter lookup happens
    /// without a compatible surface; failure is returned, not a panic.
    pub async fn new(width: u32, height: u32, msaa: u32) -> anyhow::Result<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
                apply_limit_buckets: false,
            })
            .await
            .context("no WGPU adapter available for offscreen export")?;

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("renamite offscreen device"),
                required_features: wgpu::Features::empty(),
                required_limits: adapter.limits(),
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::Off,
            })
            .await?;

        let format = wgpu::TextureFormat::Rgba8UnormSrgb;

        let renderer = WgpuSceneRenderer::from_device(device, queue, format, msaa);

        let texture = renderer.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("renamite offscreen target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        let row_bytes = (width as u64) * 4;
        let padded_bytes_per_row = (256 * row_bytes.div_ceil(256)) as u32;

        let readback = renderer.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("renamite offscreen readback"),
            size: (padded_bytes_per_row as u64) * (height as u64),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        Ok(Self {
            renderer,
            texture,
            view,
            readback,
            width,
            height,
            padded_bytes_per_row,
            format,
        })
    }

    /// Render `scene` and return a tightly packed RGBA8 buffer.
    pub fn render_rgba(
        &mut self,
        scene: &Scene,
        clear: Option<[f64; 4]>,
    ) -> anyhow::Result<Vec<u8>> {
        let mut encoder =
            self.renderer
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("renamite offscreen encoder"),
                });

        self.renderer.render_to_view(
            scene,
            &mut encoder,
            &self.view,
            self.width,
            self.height,
            clear,
        );

        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &self.readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(self.padded_bytes_per_row),
                    rows_per_image: Some(self.height),
                },
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );

        self.renderer
            .queue
            .submit(std::iter::once(encoder.finish()));

        let slice = self.readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        self.renderer
            .device
            .poll(wgpu::PollType::wait_indefinitely())?;
        rx.recv()??;

        let mapped = slice.get_mapped_range()?;
        let packed = strip_padding(&mapped, self.width, self.height, self.padded_bytes_per_row);
        drop(mapped);
        self.readback.unmap();

        Ok(packed)
    }

    /// Render `scene` and encode the result as PNG bytes.
    pub fn render_png(
        &mut self,
        scene: &Scene,
        clear: Option<[f64; 4]>,
    ) -> anyhow::Result<Vec<u8>> {
        let rgba = self.render_rgba(scene, clear)?;

        let image = image::RgbaImage::from_raw(self.width, self.height, rgba)
            .ok_or_else(|| anyhow::anyhow!("invalid RGBA buffer"))?;

        let mut bytes = std::io::Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png)?;

        Ok(bytes.into_inner())
    }
}

/// Remove WGPU's 256-byte-per-row alignment padding, yielding one packed row.
fn strip_padding(data: &[u8], width: u32, height: u32, padded_bytes_per_row: u32) -> Vec<u8> {
    let row_bytes = (width as usize) * 4;
    let padded = padded_bytes_per_row as usize;
    let mut out = Vec::with_capacity(row_bytes * (height as usize));
    for row in 0..(height as usize) {
        let start = row * padded;
        out.extend_from_slice(&data[start..start + row_bytes]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use kurbo::Shape;
    use renamite_behavior_common::ViewTransform;
    use renamite_model::{
        ClipPath, Color, NodeId, PaintKind, Scene as ModelScene, SceneItem, ScenePaint,
    };
    use renamite_render_bridge::SceneRenderer;

    #[test]
    #[ignore]
    fn offscreen_png_export() -> anyhow::Result<()> {
        let mut bridge = SceneRenderer::new();
        let model = ModelScene {
            clips: vec![ClipPath {
                path: kurbo::Rect::new(-25.0, -25.0, 25.0, 25.0).to_path(0.1),
            }],
            items: vec![SceneItem {
                path: kurbo::Circle::new((0.0, 0.0), 20.0).to_path(0.1),
                node: NodeId::default(),
                style: NodeId::default(),
                paint: ScenePaint::Solid(Color::rgba(1.0, 0.0, 0.0, 1.0)),
                kind: PaintKind::Fill(renamite_model::FillRule::NonZero),
                opacity: 1.0,
                clip: Some(0),
                blend: renamite_model::BlendMode::Normal,
            }],
        };

        let view = ViewTransform {
            scale: 1.0,
            offset: glam::DVec2::new(32.0, 32.0),
        };
        let prepared = bridge.prepare(&model, &view);

        let mut repose = Scene::default();
        bridge.append_repose_scene(&prepared, &mut repose);

        let mut gpu = pollster::block_on(OffscreenRenderer::new(64, 64, 4))?;
        let rgba = gpu.render_rgba(&repose, Some([1.0, 1.0, 1.0, 1.0]))?;
        assert_eq!(rgba.len(), 64 * 64 * 4);

        let png = gpu.render_png(&repose, Some([1.0, 1.0, 1.0, 1.0]))?;
        assert!(png.starts_with(b"\x89PNG"));
        Ok(())
    }
}
