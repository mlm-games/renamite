//! Template render smoke: every built-in template must produce a valid PNG at
//! frame 0 and at a mid-animation frame through the canonical offscreen
//! renderer (the same pipeline `renamite render` and the editor use).
//! Skips (with a warning) when no WGPU adapter is available.

use renamite_examples::{TemplateId, build_template};
use renamite_render_offscreen::{OffscreenRenderer, fit_view};

const SIZE: u32 = 256;

fn gpu() -> Option<OffscreenRenderer> {
    match OffscreenRenderer::new_blocking(SIZE, SIZE, 4) {
        Ok(g) => Some(g),
        Err(e) => {
            eprintln!("skipping template smoke test: {e}");
            None
        }
    }
}

fn render_template(gpu: &mut OffscreenRenderer, id: TemplateId, frame: f64) -> Vec<u8> {
    let file = build_template(id);
    let doc = &file.document;
    let comp = doc.main;
    let comp_size = doc.compositions[comp].size;
    let scene = renamite_model::evaluate(doc, comp, frame);
    let view: renamite_behavior_common::ViewTransform = fit_view(comp_size, SIZE, SIZE);
    let mut bridge = renamite_render_bridge::SceneRenderer::new();
    let prepared = bridge.prepare(&scene, &view);
    let mut repose = repose_core::Scene::default();
    bridge.append_repose_scene(&prepared, &mut repose);
    gpu.sync_document_images(doc)
        .expect("upload template image assets");
    gpu.render_png(&repose, Some([1.0, 1.0, 1.0, 1.0]))
        .expect("render")
}

#[test]
fn templates_render_valid_pngs() {
    let Some(mut gpu) = gpu() else { return };
    for id in TemplateId::all() {
        for frame in [0.0f64, 90.0] {
            let png = render_template(&mut gpu, *id, frame);
            let img = image::load_from_memory(&png)
                .unwrap_or_else(|e| panic!("{} @ frame {frame}: invalid PNG: {e}", id.slug()))
                .to_rgba8();
            assert_eq!(
                img.dimensions(),
                (SIZE, SIZE),
                "{} @ frame {frame}: wrong dimensions",
                id.slug()
            );

            // Blank is intentionally empty; every other template must paint
            // at least one non-background pixel.
            if *id != TemplateId::Blank {
                let has_content = img.pixels().any(|p| p.0 != [255, 255, 255, 255]);
                assert!(
                    has_content,
                    "{} @ frame {frame}: rendered entirely blank",
                    id.slug()
                );
            }
        }
    }
}
