//! Static SVG export: evaluated `Scene` -> SVG XML.
//!
//! This is a frame snapshot exporter: `Document + frame -> evaluate() ->
//! Scene -> SVG`. Repeaters, modifiers, masks, text, and precomps are already
//! resolved by the evaluator, so the output matches what the editor renders.
//! No animation (SMIL/scripting) is written.

use std::collections::HashMap;

use renamite_model::{CompId, Document, PaintKind, SceneItem, ScenePaint, StrokeCap, StrokeJoin};

use crate::path::{fmt, matrix_attr, path_data};
use crate::{SvgError, SvgReport, SvgWarning};

pub fn export_with_report(
    document: &Document,
    composition: CompId,
    frame: f64,
) -> Result<SvgReport<String>, SvgError> {
    let comp = document
        .compositions
        .get(composition)
        .ok_or(SvgError::MissingComposition)?;

    let scene = renamite_model::evaluate(document, composition, frame);

    let mut exporter = Exporter {
        document,
        scene: &scene,
        defs: String::new(),
        gradient_ids: HashMap::new(),
        warnings: Vec::new(),
    };

    let body = exporter.export_body();

    let width = comp.size.0;
    let height = comp.size.1;
    let mut out = String::new();
    out.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" width=\"{width}\" height=\"{height}\" viewBox=\"0 0 {width} {height}\">\n"
    ));
    if !exporter.defs.is_empty() {
        out.push_str("<defs>\n");
        out.push_str(&exporter.defs);
        out.push_str("</defs>\n");
    }
    out.push_str(&body);
    out.push_str("</svg>\n");

    Ok(SvgReport {
        value: out,
        warnings: exporter.warnings,
    })
}

struct Exporter<'a> {
    document: &'a Document,
    scene: &'a renamite_model::Scene,
    defs: String,
    gradient_ids: HashMap<String, String>,
    warnings: Vec<SvgWarning>,
}

impl Exporter<'_> {
    fn export_body(&mut self) -> String {
        // Clip definitions (scene items reference them by index).
        for (index, clip) in self.scene.clips.iter().enumerate() {
            let rule = match clip.rule {
                renamite_model::FillRule::EvenOdd => " clip-rule=\"evenodd\"",
                renamite_model::FillRule::NonZero => "",
            };
            let d = path_data(&clip.path);
            if d.is_empty() {
                continue;
            }
            self.defs.push_str(&format!(
                "<clipPath id=\"clip{index}\"><path d=\"{d}\"{rule}/></clipPath>\n"
            ));
        }

        let mut body = String::new();
        for item in &self.scene.items {
            self.export_item(item, &mut body);
        }
        body
    }

    fn export_item(&mut self, item: &SceneItem, out: &mut String) {
        // Scene items are emitted bottom-first; each item is wrapped in one
        // `<g>` per clip (outermost first), applied innermost-last.
        let open: Vec<String> = item
            .clips
            .iter()
            .map(|&clip| format!("<g clip-path=\"url(#clip{clip})\">\n"))
            .collect();
        let blend_open = if item.blend != renamite_model::BlendMode::Normal {
            Some(format!(
                "<g style=\"mix-blend-mode:{};isolation:isolate\">\n",
                blend_to_css(item.blend)
            ))
        } else {
            None
        };

        let mut content = String::new();
        match &item.paint {
            ScenePaint::Image {
                asset,
                width,
                height,
                affine,
                tint,
            } => self.export_image(item, *asset, *width, *height, *affine, *tint, &mut content),
            paint => {
                let opacity = item.opacity;
                match &item.kind {
                    PaintKind::Fill(rule) => {
                        self.export_fill(item, paint, *rule, opacity, &mut content)
                    }
                    PaintKind::Stroke(stroke) => {
                        self.export_stroke(item, paint, stroke, opacity, &mut content)
                    }
                }
            }
        }
        if content.is_empty() {
            return;
        }

        if let Some(ref b) = blend_open {
            out.push_str(b);
        }
        out.push_str(&open.iter().map(|s| s.as_str()).collect::<Vec<_>>().concat());
        out.push_str(&content);
        for _ in &open {
            out.push_str("</g>\n");
        }
        if blend_open.is_some() {
            out.push_str("</g>\n");
        }
    }

    fn export_fill(
        &mut self,
        item: &SceneItem,
        paint: &ScenePaint,
        rule: renamite_model::FillRule,
        opacity: f64,
        out: &mut String,
    ) {
        let d = path_data(&item.path);
        if d.is_empty() {
            return;
        }
        let fill = self.paint_attr(paint, "fill");
        let rule_attr = match rule {
            renamite_model::FillRule::EvenOdd => " fill-rule=\"evenodd\"",
            renamite_model::FillRule::NonZero => "",
        };
        let opacity_attr = if opacity < 1.0 {
            format!(" opacity=\"{}\"", fmt(opacity))
        } else {
            String::new()
        };
        out.push_str(&format!(
            "<path d=\"{d}\"{fill}{rule_attr}{opacity_attr}/>\n"
        ));
    }

    fn export_stroke(
        &mut self,
        item: &SceneItem,
        paint: &ScenePaint,
        stroke: &renamite_model::StrokeSample,
        opacity: f64,
        out: &mut String,
    ) {
        let d = path_data(&item.path);
        if d.is_empty() {
            return;
        }
        let mut attrs = String::from(" fill=\"none\"");
        attrs.push_str(&self.paint_attr(paint, "stroke"));
        attrs.push_str(&format!(" stroke-width=\"{}\"", fmt(stroke.width)));
        let cap = match stroke.cap {
            StrokeCap::Butt => "",
            StrokeCap::Round => " round",
            StrokeCap::Square => " square",
        };
        if !cap.is_empty() {
            attrs.push_str(" stroke-linecap=\"");
            attrs.push_str(cap.trim());
            attrs.push('"');
        }
        let join = match stroke.join {
            StrokeJoin::Miter => "",
            StrokeJoin::Round => " round",
            StrokeJoin::Bevel => " bevel",
        };
        if !join.is_empty() {
            attrs.push_str(" stroke-linejoin=\"");
            attrs.push_str(join.trim());
            attrs.push('"');
        }
        if let Some(dash) = &stroke.dash {
            let dashes = dash
                .dashes
                .iter()
                .map(|d| fmt(*d))
                .collect::<Vec<_>>()
                .join(" ");
            attrs.push_str(&format!(" stroke-dasharray=\"{dashes}\""));
            if dash.offset != 0.0 {
                attrs.push_str(&format!(" stroke-dashoffset=\"{}\"", fmt(dash.offset)));
            }
        }
        if opacity < 1.0 {
            attrs.push_str(&format!(" opacity=\"{}\"", fmt(opacity)));
        }
        out.push_str(&format!("<path d=\"{d}\"{attrs}/>\n"));
    }

    #[allow(clippy::too_many_arguments)]
    fn export_image(
        &mut self,
        item: &SceneItem,
        asset: renamite_model::AssetId,
        width: u32,
        height: u32,
        affine: [f64; 6],
        tint: renamite_model::Color,
        out: &mut String,
    ) {
        let Some(asset) = self.document.image_asset(asset) else {
            return;
        };
        if tint != renamite_model::Color::WHITE {
            self.warnings.push(SvgWarning {
                path: "image".into(),
                message: "image tint is not representable in SVG and was dropped".into(),
            });
        }
        let href = image_data_uri(asset.mime.as_str(), &asset.bytes);
        let transform = matrix_attr(kurbo::Affine::new(affine));
        let opacity_attr = if item.opacity < 1.0 {
            format!(" opacity=\"{}\"", fmt(item.opacity))
        } else {
            String::new()
        };
        out.push_str(&format!(
            "<image x=\"0\" y=\"0\" width=\"{width}\" height=\"{height}\" preserveAspectRatio=\"none\" href=\"{href}\" transform=\"{transform}\"{opacity_attr}/>\n"
        ));
    }

    /// `<attr>="#..."` or `<attr>="url(#gradN)"`, registering gradient defs.
    fn paint_attr(&mut self, paint: &ScenePaint, attr: &str) -> String {
        match paint {
            ScenePaint::Solid(color) => format!(" {attr}=\"{}\"", color_hex(*color)),
            ScenePaint::LinearGradient { .. } | ScenePaint::RadialGradient { .. } => {
                let id = self.gradient_id(paint);
                format!(" {attr}=\"url(#{id})\"")
            }
            ScenePaint::Image { .. } => String::new(),
        }
    }

    fn gradient_id(&mut self, paint: &ScenePaint) -> String {
        let key = gradient_key(paint);
        if let Some(id) = self.gradient_ids.get(&key) {
            return id.clone();
        }
        let id = format!("grad{}", self.gradient_ids.len());
        self.defs.push_str(&gradient_def(&id, paint));
        self.gradient_ids.insert(key, id.clone());
        id
    }
}

fn gradient_key(paint: &ScenePaint) -> String {
    match paint {
        ScenePaint::LinearGradient { start, end, stops } => format!(
            "L {} {} {} {} {}",
            fmt(start.x),
            fmt(start.y),
            fmt(end.x),
            fmt(end.y),
            stops_key(stops),
        ),
        ScenePaint::RadialGradient { center, end, stops } => format!(
            "R {} {} {} {} {}",
            fmt(center.x),
            fmt(center.y),
            fmt(end.x),
            fmt(end.y),
            stops_key(stops),
        ),
        _ => String::new(),
    }
}

fn stops_key(stops: &renamite_model::GradientStops) -> String {
    stops
        .0
        .iter()
        .map(|stop| {
            format!(
                "{}#{:02X}{:02X}{:02X}{:02X}",
                fmt(stop.offset),
                (stop.color.r * 255.0).round() as u8,
                (stop.color.g * 255.0).round() as u8,
                (stop.color.b * 255.0).round() as u8,
                (stop.color.a * 255.0).round() as u8,
            )
        })
        .collect::<Vec<_>>()
        .join(";")
}

fn gradient_def(id: &str, paint: &ScenePaint) -> String {
    let stops = match paint {
        ScenePaint::LinearGradient { stops, .. } | ScenePaint::RadialGradient { stops, .. } => {
            stops
        }
        _ => return String::new(),
    };
    let mut stop_xml = String::new();
    for stop in &stops.0 {
        stop_xml.push_str(&format!(
            "<stop offset=\"{}\" stop-color=\"{}\" stop-opacity=\"{}\"/>\n",
            fmt(stop.offset),
            color_hex_no_alpha(stop.color),
            fmt(stop.color.a),
        ));
    }
    match paint {
        ScenePaint::LinearGradient { start, end, .. } => format!(
            "<linearGradient id=\"{id}\" gradientUnits=\"userSpaceOnUse\" x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\">\n{stop_xml}</linearGradient>\n",
            fmt(start.x),
            fmt(start.y),
            fmt(end.x),
            fmt(end.y),
        ),
        ScenePaint::RadialGradient { center, end, .. } => {
            let r = (*end - *center).length();
            format!(
                "<radialGradient id=\"{id}\" gradientUnits=\"userSpaceOnUse\" cx=\"{}\" cy=\"{}\" r=\"{}\">\n{stop_xml}</radialGradient>\n",
                fmt(center.x),
                fmt(center.y),
                fmt(r),
            )
        }
        _ => String::new(),
    }
}

/// `#RRGGBBAA` (or `#RRGGBB` when opaque).
fn color_hex(color: renamite_model::Color) -> String {
    let alpha = (color.a * 255.0).round() as u8;
    if alpha == 255 {
        color_hex_no_alpha(color)
    } else {
        format!(
            "#{:02X}{:02X}{:02X}{:02X}",
            (color.r * 255.0).round() as u8,
            (color.g * 255.0).round() as u8,
            (color.b * 255.0).round() as u8,
            alpha,
        )
    }
}

fn color_hex_no_alpha(color: renamite_model::Color) -> String {
    format!(
        "#{:02X}{:02X}{:02X}",
        (color.r * 255.0).round() as u8,
        (color.g * 255.0).round() as u8,
        (color.b * 255.0).round() as u8,
    )
}

fn image_data_uri(mime: &str, bytes: &[u8]) -> String {
    use base64::Engine as _;
    format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

fn blend_to_css(blend: renamite_model::BlendMode) -> &'static str {
    match blend {
        renamite_model::BlendMode::Normal => "normal",
        renamite_model::BlendMode::Multiply => "multiply",
        renamite_model::BlendMode::Screen => "screen",
        renamite_model::BlendMode::Overlay => "overlay",
        renamite_model::BlendMode::Darken => "darken",
        renamite_model::BlendMode::Lighten => "lighten",
        renamite_model::BlendMode::ColorDodge => "color-dodge",
        renamite_model::BlendMode::ColorBurn => "color-burn",
        renamite_model::BlendMode::HardLight => "hard-light",
        renamite_model::BlendMode::SoftLight => "soft-light",
        renamite_model::BlendMode::Difference => "difference",
        renamite_model::BlendMode::Exclusion => "exclusion",
        renamite_model::BlendMode::Hue => "hue",
        renamite_model::BlendMode::Saturation => "saturation",
        renamite_model::BlendMode::Color => "color",
        renamite_model::BlendMode::Luminosity => "luminosity",
    }
}
