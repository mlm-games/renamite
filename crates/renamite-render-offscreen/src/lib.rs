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

/// Re-export shared device.
pub fn shared_device() -> Option<(wgpu::Device, wgpu::Queue)> {
    repose_render_wgpu::offscreen::shared_device()
}
pub fn set_shared_device(device: wgpu::Device, queue: wgpu::Queue) {
    repose_render_wgpu::offscreen::set_shared_device(device, queue)
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct ImageFingerprint {
    hash: u64,
    len: usize,
    srgb: bool,
}

fn image_fingerprint(bytes: &[u8], srgb: bool) -> ImageFingerprint {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    srgb.hash(&mut hasher);
    ImageFingerprint {
        hash: hasher.finish(),
        len: bytes.len(),
        srgb,
    }
}

/// Build a `ViewTransform` that letterboxes `artboard` into a `wxh` frame.
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

fn pick_device_msaa(_device: &wgpu::Device, _format: wgpu::TextureFormat, _requested: u32) -> u32 {
    1
}

pub struct OffscreenRenderer {
    renderer: WgpuSceneRenderer,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    readback: wgpu::Buffer,

    width: u32,
    height: u32,
    padded_bytes_per_row: u32,
    uploaded_image_hashes: std::collections::HashMap<u64, ImageFingerprint>,
}

impl OffscreenRenderer {
    pub fn new_blocking(width: u32, height: u32, msaa: u32) -> anyhow::Result<Self> {
        pollster::block_on(Self::new(width, height, msaa))
    }

    /// Shared-device: reuse Device/Queue, no Adapter.
    pub fn from_device(
        device: wgpu::Device,
        queue: wgpu::Queue,
        width: u32,
        height: u32,
        msaa: u32,
    ) -> anyhow::Result<Self> {
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let msaa = pick_device_msaa(&device, format, msaa);
        let renderer = WgpuSceneRenderer::from_device(device, queue, format, msaa);
        Self::from_renderer(renderer, width, height)
    }

    pub fn from_device_with_adapter(
        device: wgpu::Device,
        queue: wgpu::Queue,
        adapter: &wgpu::Adapter,
        width: u32,
        height: u32,
        msaa: u32,
    ) -> anyhow::Result<Self> {
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let msaa = repose_render_wgpu::pick_surface_msaa(adapter, format, msaa);
        let renderer = WgpuSceneRenderer::from_device(device, queue, format, msaa);
        Self::from_renderer(renderer, width, height)
    }

    pub fn from_renderer(
        mut renderer: WgpuSceneRenderer,
        width: u32,
        height: u32,
    ) -> anyhow::Result<Self> {
        let (width, height) = Self::validate_dimensions(&renderer.device, width, height)?;
        let (texture, view, readback, padded_bytes_per_row) =
            Self::create_target(&renderer.device, width, height)?;
        renderer.resize(width, height);
        Ok(Self {
            renderer,
            texture,
            view,
            readback,
            width,
            height,
            padded_bytes_per_row,
            uploaded_image_hashes: std::collections::HashMap::new(),
        })
    }

    fn validate_dimensions(
        device: &wgpu::Device,
        width: u32,
        height: u32,
    ) -> anyhow::Result<(u32, u32)> {
        if width == 0 || height == 0 {
            anyhow::bail!("offscreen dimensions must be greater than zero");
        }
        if width > device.limits().max_texture_dimension_2d
            || height > device.limits().max_texture_dimension_2d
        {
            anyhow::bail!("offscreen target exceeds device texture limits");
        }
        Ok((width, height))
    }

    fn create_target(
        device: &wgpu::Device,
        width: u32,
        height: u32,
    ) -> anyhow::Result<(wgpu::Texture, wgpu::TextureView, wgpu::Buffer, u32)> {
        if width == 0 || height == 0 {
            anyhow::bail!("offscreen dimensions must be greater than zero");
        }
        let row_bytes = u64::from(width)
            .checked_mul(4)
            .ok_or_else(|| anyhow::anyhow!("offscreen row size overflow"))?;
        let padded_bytes_per_row = row_bytes
            .div_ceil(256)
            .checked_mul(256)
            .ok_or_else(|| anyhow::anyhow!("offscreen row alignment overflow"))?;
        let padded_bytes_per_row_u32 = u32::try_from(padded_bytes_per_row)
            .map_err(|_| anyhow::anyhow!("offscreen row size exceeds WGPU limits"))?;
        let buffer_size = padded_bytes_per_row
            .checked_mul(u64::from(height))
            .ok_or_else(|| anyhow::anyhow!("offscreen readback size overflow"))?;
        if buffer_size > device.limits().max_buffer_size
            || width > device.limits().max_texture_dimension_2d
            || height > device.limits().max_texture_dimension_2d
        {
            return Err(anyhow::anyhow!("offscreen target exceeds device limits"));
        }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("renamite offscreen target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("renamite offscreen readback"),
            size: buffer_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        Ok((texture, view, readback, padded_bytes_per_row_u32))
    }

    pub async fn new(width: u32, height: u32, msaa: u32) -> anyhow::Result<Self> {
        let instance = if cfg!(target_arch = "wasm32") {
            let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
            desc.backends = wgpu::Backends::BROWSER_WEBGPU | wgpu::Backends::GL;
            wgpu::util::new_instance_with_webgpu_detection(desc).await
        } else {
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle())
        };

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
                apply_limit_buckets: false,
            })
            .await
            .context("no WGPU adapter available for offscreen export")?;

        if width == 0 || height == 0 {
            anyhow::bail!("offscreen dimensions must be greater than zero");
        }
        if width > adapter.limits().max_texture_dimension_2d
            || height > adapter.limits().max_texture_dimension_2d
        {
            anyhow::bail!("offscreen target exceeds device texture limits");
        }

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
        let msaa = repose_render_wgpu::pick_surface_msaa(&adapter, format, msaa);

        let renderer = WgpuSceneRenderer::from_device(device, queue, format, msaa);

        let (width, height) = Self::validate_dimensions(&renderer.device, width, height)?;
        let (texture, view, readback, padded_bytes_per_row) =
            Self::create_target(&renderer.device, width, height)?;

        Ok(Self {
            renderer,
            texture,
            view,
            readback,
            width,
            height,
            padded_bytes_per_row,
            uploaded_image_hashes: std::collections::HashMap::new(),
        })
    }

    pub fn render_rgba(
        &mut self,
        scene: &Scene,
        clear: Option<[f64; 4]>,
    ) -> anyhow::Result<Vec<u8>> {
        #[cfg(all(target_family = "wasm", target_os = "unknown"))]
        debug_assert!(
            web_workers::web::has_block_support(),
            "render_rgba (blocking) called on wasm main thread; use render_rgba_async"
        );
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
        repose_render_wgpu::offscreen::map_buffer_blocking(&slice, &self.renderer.device)?;

        let mapped = slice.get_mapped_range()?;
        let packed = strip_padding(&mapped, self.width, self.height, self.padded_bytes_per_row);
        drop(mapped);
        self.readback.unmap();

        packed
    }

    pub async fn render_rgba_async(
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
        repose_render_wgpu::offscreen::map_buffer_unified(&slice, &self.renderer.device).await?;

        let mapped = slice.get_mapped_range().context("mapped range")?;
        let packed = strip_padding(&mapped, self.width, self.height, self.padded_bytes_per_row);
        drop(mapped);
        self.readback.unmap();

        packed
    }

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

    pub async fn render_png_async(
        &mut self,
        scene: &Scene,
        clear: Option<[f64; 4]>,
    ) -> anyhow::Result<Vec<u8>> {
        let rgba = self.render_rgba_async(scene, clear).await?;
        let image = image::RgbaImage::from_raw(self.width, self.height, rgba)
            .ok_or_else(|| anyhow::anyhow!("invalid RGBA buffer"))?;
        let mut bytes = std::io::Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png)?;
        Ok(bytes.into_inner())
    }

    pub fn set_image_encoded(
        &mut self,
        handle: repose_core::ImageHandle,
        bytes: &[u8],
        srgb: bool,
    ) -> anyhow::Result<()> {
        let fingerprint = image_fingerprint(bytes, srgb);
        if self.uploaded_image_hashes.get(&handle) == Some(&fingerprint) {
            return Ok(());
        }
        self.renderer.set_image_from_bytes(handle, bytes, srgb)?;
        self.uploaded_image_hashes.insert(handle, fingerprint);
        Ok(())
    }

    pub fn sync_document_images(
        &mut self,
        document: &renamite_model::Document,
    ) -> anyhow::Result<()> {
        let mut live = std::collections::HashSet::new();
        for &id in &document.asset_order {
            let Some(image) = document.image_asset(id) else {
                continue;
            };
            let handle = renamite_render_bridge::image_handle(id);
            live.insert(handle);
            self.set_image_encoded(handle, &image.bytes, image.srgb)?;
        }

        let stale: Vec<_> = self
            .uploaded_image_hashes
            .keys()
            .copied()
            .filter(|handle| !live.contains(handle))
            .collect();
        for handle in stale {
            self.renderer.remove_image(handle);
            self.uploaded_image_hashes.remove(&handle);
        }

        Ok(())
    }
}

fn strip_padding(
    data: &[u8],
    width: u32,
    height: u32,
    padded_bytes_per_row: u32,
) -> anyhow::Result<Vec<u8>> {
    let row_bytes = (width as usize)
        .checked_mul(4)
        .ok_or_else(|| anyhow::anyhow!("offscreen row size overflow"))?;
    let padded = padded_bytes_per_row as usize;
    let total = padded
        .checked_mul(height as usize)
        .ok_or_else(|| anyhow::anyhow!("offscreen readback size overflow"))?;
    if padded < row_bytes || data.len() < total {
        anyhow::bail!("offscreen readback buffer is shorter than its target");
    }
    let mut out = Vec::with_capacity(
        row_bytes
            .checked_mul(height as usize)
            .ok_or_else(|| anyhow::anyhow!("offscreen output size overflow"))?,
    );
    for row in 0..(height as usize) {
        let start = row * padded;
        out.extend_from_slice(&data[start..start + row_bytes]);
    }
    Ok(out)
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
    fn offscreen_png_export() -> anyhow::Result<()> {
        let mut bridge = SceneRenderer::new();
        let model = ModelScene {
            clips: vec![ClipPath {
                path: kurbo::Rect::new(-25.0, -25.0, 25.0, 25.0).to_path(0.1),
                rule: renamite_model::FillRule::NonZero,
            }],
            items: vec![SceneItem {
                path: kurbo::Circle::new((0.0, 0.0), 20.0).to_path(0.1),
                node: NodeId::default(),
                style: NodeId::default(),
                paint: ScenePaint::Solid(Color::rgba(1.0, 0.0, 0.0, 1.0)),
                kind: PaintKind::Fill(renamite_model::FillRule::NonZero),
                opacity: 1.0,
                clips: vec![0],
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

        let mut gpu = match pollster::block_on(OffscreenRenderer::new(64, 64, 4)) {
            Ok(g) => g,
            Err(e) => {
                let s = format!("{e:?}").to_lowercase();
                if s.contains("adapter") || s.contains("wgpu") || s.contains("gpu") {
                    eprintln!("SKIP offscreen_png_export: no GPU adapter ({e:#})");
                    return Ok(());
                }
                return Err(e);
            }
        };
        let rgba = gpu.render_rgba(&repose, Some([1.0, 1.0, 1.0, 1.0]))?;
        assert_eq!(rgba.len(), 64 * 64 * 4);

        let png = gpu.render_png(&repose, Some([1.0, 1.0, 1.0, 1.0]))?;
        assert!(png.starts_with(b"\x89PNG"));
        Ok(())
    }
}
