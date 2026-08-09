//! Paint adaptation: `usvg` fill/stroke paints into Renamite style nodes.

use kurbo::Point;
use renamite_model::{
    Color, FillRule, GradientStop, GradientStops, StrokeCap, StrokeJoin, StylePaint,
};
use usvg::{Fill, Paint, Stroke};

use crate::SvgWarning;
use crate::import::PaintContext;

/// Import a `usvg` fill style.
///
/// Returns the style node with its paint and fill rule. `fill.opacity()`
/// becomes the node's own opacity (Renamite applies style opacity when
/// emitting the paint over its sibling shapes).
pub fn import_fill(
    fill: &Fill,
    context: &PaintContext,
    warnings: &mut Vec<SvgWarning>,
) -> Option<renamite_model::Node> {
    let paint = import_paint(fill.paint(), context, warnings)?;
    let rule = match fill.rule() {
        usvg::FillRule::NonZero => FillRule::NonZero,
        usvg::FillRule::EvenOdd => FillRule::EvenOdd,
    };
    let mut node = renamite_model::Node::new(
        context.name("Fill"),
        renamite_model::NodeKind::Style(renamite_model::StyleKind::Fill { paint, rule }),
    );
    node.opacity = renamite_animation::Animated::new(fill.opacity().get() as f64);
    Some(node)
}

/// Import a `usvg` stroke style.
pub fn import_stroke(
    stroke: &Stroke,
    context: &PaintContext,
    warnings: &mut Vec<SvgWarning>,
) -> Option<renamite_model::Node> {
    let paint = import_paint(stroke.paint(), context, warnings)?;
    let cap = match stroke.linecap() {
        usvg::LineCap::Butt => StrokeCap::Butt,
        usvg::LineCap::Round => StrokeCap::Round,
        usvg::LineCap::Square => StrokeCap::Square,
    };
    let join = match stroke.linejoin() {
        usvg::LineJoin::Miter | usvg::LineJoin::MiterClip => StrokeJoin::Miter,
        usvg::LineJoin::Round => StrokeJoin::Round,
        usvg::LineJoin::Bevel => StrokeJoin::Bevel,
    };
    let dash = stroke
        .dasharray()
        .map(|array| renamite_model::AnimatedDash {
            dashes: array
                .iter()
                .map(|d| renamite_animation::Animated::new(*d as f64))
                .collect(),
            offset: renamite_animation::Animated::new(stroke.dashoffset() as f64),
        });
    let mut node = renamite_model::Node::new(
        context.name("Stroke"),
        renamite_model::NodeKind::Style(renamite_model::StyleKind::Stroke {
            paint,
            width: renamite_animation::Animated::new(stroke.width().get() as f64),
            cap,
            join,
            dash,
        }),
    );
    node.opacity = renamite_animation::Animated::new(stroke.opacity().get() as f64);
    Some(node)
}

/// Convert a `usvg` paint into a Renamite `StylePaint`.
///
/// `context` supplies the owning element's absolute transform: gradient
/// coordinates live in the element's local space (resvg draws the path and
/// its paint under the same transform), so the endpoints are folded into
/// world space together with the shape's baked-in transform.
///
/// Unsupported paints (patterns) and non-Pad spread modes produce warnings
/// and `None`.
pub fn import_paint(
    paint: &Paint,
    context: &PaintContext,
    warnings: &mut Vec<SvgWarning>,
) -> Option<StylePaint> {
    match paint {
        Paint::Color(color) => Some(StylePaint::solid(Color::rgba(
            color.red as f64 / 255.0,
            color.green as f64 / 255.0,
            color.blue as f64 / 255.0,
            1.0,
        ))),

        Paint::LinearGradient(gradient) => {
            if gradient.spread_method() != usvg::SpreadMethod::Pad {
                warnings.push(SvgWarning {
                    path: context.path.to_string(),
                    message: format!(
                        "gradient spread method `{}` is not supported; Renamite clamps outside the gradient",
                        match gradient.spread_method() {
                            usvg::SpreadMethod::Reflect => "reflect",
                            usvg::SpreadMethod::Repeat => "repeat",
                            usvg::SpreadMethod::Pad => "pad",
                        }
                    ),
                });
            }
            let affine = context.paint_affine(gradient.transform());
            let start = affine * Point::new(gradient.x1() as f64, gradient.y1() as f64);
            let end = affine * Point::new(gradient.x2() as f64, gradient.y2() as f64);
            Some(StylePaint::linear(
                glam::DVec2::new(start.x, start.y),
                glam::DVec2::new(end.x, end.y),
                import_stops(gradient.stops()),
            ))
        }

        Paint::RadialGradient(gradient) => {
            if gradient.spread_method() != usvg::SpreadMethod::Pad {
                warnings.push(SvgWarning {
                    path: context.path.to_string(),
                    message: format!(
                        "gradient spread method `{}` is not supported; Renamite clamps outside the gradient",
                        match gradient.spread_method() {
                            usvg::SpreadMethod::Reflect => "reflect",
                            usvg::SpreadMethod::Repeat => "repeat",
                            usvg::SpreadMethod::Pad => "pad",
                        }
                    ),
                });
            }
            let affine = context.paint_affine(gradient.transform());
            let center = affine * Point::new(gradient.cx() as f64, gradient.cy() as f64);
            let edge = affine
                * Point::new(
                    gradient.cx() as f64 + gradient.r().get() as f64,
                    gradient.cy() as f64,
                );
            Some(StylePaint::radial(
                glam::DVec2::new(center.x, center.y),
                glam::DVec2::new(edge.x, edge.y),
                import_stops(gradient.stops()),
            ))
        }

        Paint::Pattern(_) => {
            warnings.push(SvgWarning {
                path: context.path.to_string(),
                message: "SVG patterns are not editable in Renamite and were skipped".into(),
            });
            None
        }
    }
}

/// Convert `usvg` gradient stops into Renamite stops. Each stop's own
/// `stop-opacity` becomes the color alpha.
fn import_stops(stops: &[usvg::Stop]) -> GradientStops {
    GradientStops(
        stops
            .iter()
            .map(|stop| {
                let color = stop.color();
                GradientStop {
                    offset: stop.offset().get() as f64,
                    color: Color::rgba(
                        color.red as f64 / 255.0,
                        color.green as f64 / 255.0,
                        color.blue as f64 / 255.0,
                        stop.opacity().get() as f64,
                    ),
                }
            })
            .collect(),
    )
}
