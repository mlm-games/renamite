//! Conversion of Renamite animated values to/from Lottie property objects.

use glam::DVec2;
use renamite_animation::{Angle, Animated, EasingHandle, Frame, Interpolation, Keyframe, Tween};
use renamite_geometry::{Anchor, TangentMode, VectorPath};
use renamite_model::{Color, GradientStop, GradientStops};
use serde_json::{Map, Value, json};

fn easing_fields<T>(key: &Keyframe<T>, map: &mut Map<String, Value>) {
    if key.interpolation == Interpolation::Hold {
        map.insert("h".into(), json!(1));
        return;
    }
    map.insert(
        "o".into(),
        json!({
            "x": [key.ease_out.x],
            "y": [key.ease_out.y]
        }),
    );
    map.insert(
        "i".into(),
        json!({
            "x": [key.ease_in.x],
            "y": [key.ease_in.y]
        }),
    );
}

pub(crate) fn export_scalar(animated: &Animated<f64>, scale: f64) -> Value {
    if animated.keyframes.is_empty() {
        return json!({
            "a": 0,
            "k": animated.base * scale
        });
    }
    let mut keys = Vec::with_capacity(animated.keyframes.len());
    for (index, key) in animated.keyframes.iter().enumerate() {
        let mut object = Map::new();
        object.insert("t".into(), json!(key.frame.0));
        object.insert("s".into(), json!([key.value * scale]));
        if let Some(next) = animated.keyframes.get(index + 1) {
            object.insert("e".into(), json!([next.value * scale]));
        }
        easing_fields(key, &mut object);
        keys.push(Value::Object(object));
    }
    json!({ "a": 1, "k": keys })
}

pub(crate) fn export_angle(animated: &Animated<Angle>) -> Value {
    if animated.keyframes.is_empty() {
        return json!({
            "a": 0,
            "k": animated.base.0
        });
    }
    let mut keys = Vec::with_capacity(animated.keyframes.len());
    for (index, key) in animated.keyframes.iter().enumerate() {
        let mut object = Map::new();
        object.insert("t".into(), json!(key.frame.0));
        object.insert("s".into(), json!([key.value.0]));
        if let Some(next) = animated.keyframes.get(index + 1) {
            object.insert("e".into(), json!([next.value.0]));
        }
        easing_fields(key, &mut object);
        keys.push(Value::Object(object));
    }
    json!({ "a": 1, "k": keys })
}

pub(crate) fn export_vec2(animated: &Animated<DVec2>) -> Value {
    if animated.keyframes.is_empty() {
        return json!({
            "a": 0,
            "k": [animated.base.x, animated.base.y]
        });
    }
    let mut keys = Vec::with_capacity(animated.keyframes.len());
    for (index, key) in animated.keyframes.iter().enumerate() {
        let mut object = Map::new();
        object.insert("t".into(), json!(key.frame.0));
        object.insert("s".into(), json!([key.value.x, key.value.y]));
        if let Some(next) = animated.keyframes.get(index + 1) {
            object.insert("e".into(), json!([next.value.x, next.value.y]));
        }
        easing_fields(key, &mut object);
        keys.push(Value::Object(object));
    }
    json!({ "a": 1, "k": keys })
}

pub(crate) fn export_color(animated: &Animated<Color>) -> Value {
    if animated.keyframes.is_empty() {
        let c = animated.base;
        return json!({
            "a": 0,
            "k": [c.r, c.g, c.b, c.a]
        });
    }
    let mut keys = Vec::with_capacity(animated.keyframes.len());
    for (index, key) in animated.keyframes.iter().enumerate() {
        let mut object = Map::new();
        let c = key.value;
        object.insert("t".into(), json!(key.frame.0));
        object.insert("s".into(), json!([c.r, c.g, c.b, c.a]));
        if let Some(next) = animated.keyframes.get(index + 1) {
            let n = next.value;
            object.insert("e".into(), json!([n.r, n.g, n.b, n.a]));
        }
        easing_fields(key, &mut object);
        keys.push(Value::Object(object));
    }
    json!({ "a": 1, "k": keys })
}

fn path_json(path: &VectorPath) -> Value {
    json!({
        "c": path.closed,
        "v": path
            .anchors
            .iter()
            .map(|anchor| [anchor.pos.x, anchor.pos.y])
            .collect::<Vec<_>>(),
        "i": path
            .anchors
            .iter()
            .map(|anchor| [anchor.tan_in.x, anchor.tan_in.y])
            .collect::<Vec<_>>(),
        "o": path
            .anchors
            .iter()
            .map(|anchor| [anchor.tan_out.x, anchor.tan_out.y])
            .collect::<Vec<_>>()
    })
}

pub(crate) fn export_path(animated: &Animated<VectorPath>) -> Value {
    if animated.keyframes.is_empty() {
        return json!({
            "a": 0,
            "k": path_json(&animated.base)
        });
    }
    let mut keys = Vec::with_capacity(animated.keyframes.len());
    for (index, key) in animated.keyframes.iter().enumerate() {
        let mut object = Map::new();
        object.insert("t".into(), json!(key.frame.0));
        object.insert("s".into(), json!([path_json(&key.value)]));
        if let Some(next) = animated.keyframes.get(index + 1) {
            object.insert("e".into(), json!([path_json(&next.value)]));
        }
        easing_fields(key, &mut object);
        keys.push(Value::Object(object));
    }
    json!({ "a": 1, "k": keys })
}

fn normalized_stops(stops: &GradientStops, count: usize) -> GradientStops {
    if count <= 1 {
        return GradientStops(vec![GradientStop {
            offset: 0.0,
            color: stops.sample(0.0),
        }]);
    }
    GradientStops(
        (0..count)
            .map(|index| {
                let offset = index as f64 / (count - 1) as f64;
                GradientStop {
                    offset,
                    color: stops.sample(offset),
                }
            })
            .collect(),
    )
}

fn packed_stops(stops: &GradientStops, count: usize) -> Vec<f64> {
    let stops = normalized_stops(stops, count);
    let mut output = Vec::with_capacity(count * 6);
    for stop in &stops.0 {
        output.extend_from_slice(&[stop.offset, stop.color.r, stop.color.g, stop.color.b]);
    }
    for stop in &stops.0 {
        output.extend_from_slice(&[stop.offset, stop.color.a]);
    }
    output
}

/// Returns `(color_stop_count, Lottie animated gradient property)`.
pub(crate) fn export_gradient(animated: &Animated<GradientStops>) -> (usize, Value) {
    let count = animated
        .keyframes
        .iter()
        .map(|key| key.value.0.len())
        .chain(std::iter::once(animated.base.0.len()))
        .max()
        .unwrap_or(2)
        .max(2);
    if animated.keyframes.is_empty() {
        return (
            count,
            json!({
                "a": 0,
                "k": packed_stops(&animated.base, count)
            }),
        );
    }
    let mut keys = Vec::with_capacity(animated.keyframes.len());
    for (index, key) in animated.keyframes.iter().enumerate() {
        let mut object = Map::new();
        object.insert("t".into(), json!(key.frame.0));
        object.insert("s".into(), json!(packed_stops(&key.value, count)));
        if let Some(next) = animated.keyframes.get(index + 1) {
            object.insert("e".into(), json!(packed_stops(&next.value, count)));
        }
        easing_fields(key, &mut object);
        keys.push(Value::Object(object));
    }
    (count, json!({ "a": 1, "k": keys }))
}

fn first_number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::Array(array) => array.first().and_then(first_number),
        _ => None,
    }
}

fn parse_vec2_value(value: &Value) -> Option<DVec2> {
    let array = value.as_array()?;
    if array.first().is_some_and(Value::is_array) {
        return parse_vec2_value(array.first()?);
    }
    Some(DVec2::new(
        array.first()?.as_f64()?,
        array.get(1)?.as_f64()?,
    ))
}

fn parse_color_value(value: &Value) -> Option<Color> {
    let array = value.as_array()?;
    if array.first().is_some_and(Value::is_array) {
        return parse_color_value(array.first()?);
    }
    Some(Color::rgba(
        array.first()?.as_f64()?.clamp(0.0, 1.0),
        array.get(1)?.as_f64()?.clamp(0.0, 1.0),
        array.get(2)?.as_f64()?.clamp(0.0, 1.0),
        array
            .get(3)
            .and_then(Value::as_f64)
            .unwrap_or(1.0)
            .clamp(0.0, 1.0),
    ))
}

fn parse_path_value(value: &Value) -> Option<VectorPath> {
    let object = if let Some(array) = value.as_array() {
        array.first()?
    } else {
        value
    };
    let vertices = object.get("v")?.as_array()?;
    let incoming = object
        .get("i")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let outgoing = object
        .get("o")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut anchors = Vec::with_capacity(vertices.len());
    for (index, vertex) in vertices.iter().enumerate() {
        let pos = parse_vec2_value(vertex)?;
        let tan_in = incoming
            .get(index)
            .and_then(parse_vec2_value)
            .unwrap_or(DVec2::ZERO);
        let tan_out = outgoing
            .get(index)
            .and_then(parse_vec2_value)
            .unwrap_or(DVec2::ZERO);
        let mode = if tan_in.length_squared() < 1e-12 && tan_out.length_squared() < 1e-12 {
            TangentMode::Corner
        } else {
            TangentMode::Smooth
        };
        anchors.push(Anchor {
            pos,
            tan_in,
            tan_out,
            mode,
        });
    }
    Some(VectorPath {
        anchors,
        closed: object.get("c").and_then(Value::as_bool).unwrap_or(false),
    })
}

fn handle_component(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::Array(array) => array.first().and_then(handle_component),
        _ => None,
    }
}

fn parse_interpolation(object: &Value) -> (Interpolation, EasingHandle, EasingHandle) {
    if object.get("h").and_then(Value::as_u64) == Some(1) {
        return (
            Interpolation::Hold,
            EasingHandle::LINEAR_OUT,
            EasingHandle::LINEAR_IN,
        );
    }
    let out_x = object.pointer("/o/x").and_then(handle_component);
    let out_y = object.pointer("/o/y").and_then(handle_component);
    let in_x = object.pointer("/i/x").and_then(handle_component);
    let in_y = object.pointer("/i/y").and_then(handle_component);
    match (out_x, out_y, in_x, in_y) {
        (Some(ox), Some(oy), Some(ix), Some(iy)) => (
            Interpolation::CubicBezier,
            EasingHandle {
                x: ox.clamp(0.0, 1.0),
                y: oy,
            },
            EasingHandle {
                x: ix.clamp(0.0, 1.0),
                y: iy,
            },
        ),
        _ => (
            Interpolation::Linear,
            EasingHandle::LINEAR_OUT,
            EasingHandle::LINEAR_IN,
        ),
    }
}

fn import_property<T: Clone + Tween>(
    property: &Value,
    default: T,
    parse_value: impl Fn(&Value) -> Option<T>,
) -> Animated<T> {
    if property.is_null() {
        return Animated::new(default);
    }
    let raw = property.get("k").unwrap_or(property);
    let explicitly_animated = property.get("a").and_then(Value::as_u64) == Some(1);
    let looks_animated = raw
        .as_array()
        .and_then(|array| array.first())
        .is_some_and(|first| first.get("t").is_some());
    if !explicitly_animated && !looks_animated {
        return Animated::new(parse_value(raw).unwrap_or(default));
    }
    let Some(raw_keys) = raw.as_array() else {
        return Animated::new(default);
    };
    let mut keys = Vec::with_capacity(raw_keys.len());
    let mut previous_value = default.clone();
    let mut previous_end: Option<T> = None;
    for raw_key in raw_keys {
        let value = raw_key
            .get("s")
            .and_then(&parse_value)
            .or_else(|| previous_end.clone())
            .or_else(|| raw_key.get("e").and_then(&parse_value))
            .unwrap_or_else(|| previous_value.clone());
        let (interpolation, ease_out, ease_in) = parse_interpolation(raw_key);
        let frame = Frame(
            raw_key
                .get("t")
                .and_then(Value::as_f64)
                .unwrap_or(0.0)
                .round() as i64,
        );
        keys.push(Keyframe {
            frame,
            value: value.clone(),
            interpolation,
            ease_out,
            ease_in,
        });
        previous_value = value;
        previous_end = raw_key.get("e").and_then(&parse_value);
    }
    keys.sort_by_key(|key| key.frame);
    let mut unique = Vec::with_capacity(keys.len());
    for key in keys {
        if unique
            .last()
            .is_some_and(|last: &Keyframe<T>| last.frame == key.frame)
        {
            *unique.last_mut().unwrap() = key;
        } else {
            unique.push(key);
        }
    }
    let base = unique
        .first()
        .map(|key| key.value.clone())
        .unwrap_or(default);
    Animated {
        base,
        keyframes: unique,
    }
}

pub(crate) fn import_scalar(property: &Value, scale: f64, default: f64) -> Animated<f64> {
    import_property(property, default, |value| {
        first_number(value).map(|number| number * scale)
    })
}

pub(crate) fn import_angle(property: &Value, default: f64) -> Animated<Angle> {
    let scalar = import_scalar(property, 1.0, default);
    Animated {
        base: Angle(scalar.base),
        keyframes: scalar
            .keyframes
            .into_iter()
            .map(|key| Keyframe {
                frame: key.frame,
                value: Angle(key.value),
                interpolation: key.interpolation,
                ease_out: key.ease_out,
                ease_in: key.ease_in,
            })
            .collect(),
    }
}

pub(crate) fn import_vec2(property: &Value, default: DVec2) -> Animated<DVec2> {
    // Split-dimension Lottie position.
    if property.get("s").and_then(Value::as_u64) == Some(1) {
        let x = import_scalar(property.get("x").unwrap_or(&Value::Null), 1.0, default.x);
        let y = import_scalar(property.get("y").unwrap_or(&Value::Null), 1.0, default.y);
        let mut frames = std::collections::BTreeSet::new();
        frames.extend(x.keyframes.iter().map(|key| key.frame));
        frames.extend(y.keyframes.iter().map(|key| key.frame));
        let mut output = Animated::new(DVec2::new(x.base, y.base));
        for frame in frames {
            output.set_key(
                frame,
                DVec2::new(x.value_at(frame.0 as f64), y.value_at(frame.0 as f64)),
            );
        }
        return output;
    }
    import_property(property, default, parse_vec2_value)
}

pub(crate) fn import_color(property: &Value, default: Color) -> Animated<Color> {
    import_property(property, default, parse_color_value)
}

pub(crate) fn import_path(property: &Value) -> Animated<VectorPath> {
    import_property(property, VectorPath::default(), parse_path_value)
}

fn alpha_at(alpha: &[(f64, f64)], offset: f64) -> f64 {
    if alpha.is_empty() {
        return 1.0;
    }
    if offset <= alpha[0].0 {
        return alpha[0].1;
    }
    for pair in alpha.windows(2) {
        let (a_offset, a_alpha) = pair[0];
        let (b_offset, b_alpha) = pair[1];
        if offset <= b_offset {
            let width = (b_offset - a_offset).max(1e-9);
            let t = ((offset - a_offset) / width).clamp(0.0, 1.0);
            return a_alpha + (b_alpha - a_alpha) * t;
        }
    }
    alpha.last().unwrap().1
}

#[allow(clippy::chunks_exact_to_as_chunks)]
fn unpack_stops(value: &Value, count: usize) -> Option<GradientStops> {
    let values = value.as_array()?;
    if values.len() < count * 4 {
        return None;
    }
    let alpha_values = &values[count * 4..];
    let alpha_stops: Vec<(f64, f64)> = alpha_values
        .chunks_exact(2)
        .filter_map(|pair| Some((pair[0].as_f64()?, pair[1].as_f64()?)))
        .collect();
    let mut stops = Vec::with_capacity(count);
    for index in 0..count {
        let base = index * 4;
        let offset = values[base].as_f64()?;
        stops.push(GradientStop {
            offset,
            color: Color::rgba(
                values[base + 1].as_f64()?.clamp(0.0, 1.0),
                values[base + 2].as_f64()?.clamp(0.0, 1.0),
                values[base + 3].as_f64()?.clamp(0.0, 1.0),
                alpha_at(&alpha_stops, offset).clamp(0.0, 1.0),
            ),
        });
    }
    stops.sort_by(|a, b| a.offset.total_cmp(&b.offset));
    Some(GradientStops(stops))
}

pub(crate) fn import_gradient(gradient: &Value) -> Animated<GradientStops> {
    let count = gradient.get("p").and_then(Value::as_u64).unwrap_or(2) as usize;
    import_property(
        gradient.get("k").unwrap_or(&Value::Null),
        GradientStops::default(),
        |value| unpack_stops(value, count),
    )
}
