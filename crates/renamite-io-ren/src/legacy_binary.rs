use std::collections::HashMap;

use renamite_animation::{Animated, AnimatedTransform};
use renamite_machine::{ClipId, ClipMap, MachineId, MachineMap};
use renamite_model::{
    Asset, AssetId, AssetMap, CompId, CompMap, Composition, Document, FillRule, ImageAsset,
    ImageNode, LayerProps, MaskProps, ModifierKind, Node, NodeId, NodeKind, NodeMap, ShapeKind,
    StarKind, StrokeCap, StrokeJoin, StyleKind, StylePaint, TextNode, TimeMap, TrimMode,
};
use serde::Deserialize;
use slotmap::SlotMap;

use super::{Meta, RenFile};

fn default_miter_limit() -> Animated<f64> {
    Animated::new(4.0)
}

#[derive(Deserialize)]
struct LegacyRenFile {
    format_version: u32,
    meta: Meta,
    document: LegacyDocument,
    #[serde(default)]
    clips: ClipMap,
    #[serde(default)]
    machines: MachineMap,
    #[serde(default)]
    start_machine: Option<MachineId>,
    #[serde(default)]
    clip_order: Vec<ClipId>,
    #[serde(default)]
    machine_order: Vec<MachineId>,
}

#[derive(Deserialize)]
struct LegacyDocument {
    format_version: u32,
    compositions: SlotMap<CompId, Composition>,
    nodes: SlotMap<NodeId, LegacyNode>,
    assets: SlotMap<AssetId, LegacyAsset>,
    main: CompId,
}

#[derive(Deserialize)]
struct LegacyNode {
    name: String,
    parent: Option<NodeId>,
    children: Vec<NodeId>,
    visible: bool,
    locked: bool,
    transform: AnimatedTransform,
    opacity: Animated<f64>,
    kind: LegacyNodeKind,
}

#[derive(Deserialize)]
enum LegacyNodeKind {
    Group,
    Layer(LayerProps),
    Shape(LegacyShapeKind),
    Style(LegacyStyleKind),
    Modifier(LegacyModifierKind),
    Text(LegacyTextNode),
    Image(AssetId),
    Precomp { comp: CompId, time_map: TimeMap },
    Mask(LegacyMaskProps),
}

#[derive(Deserialize)]
enum LegacyShapeKind {
    Path(Animated<renamite_geometry::VectorPath>),
    Rect {
        pos: Animated<glam::DVec2>,
        size: Animated<glam::DVec2>,
        rounded: Animated<f64>,
    },
    Ellipse {
        pos: Animated<glam::DVec2>,
        size: Animated<glam::DVec2>,
    },
    Star {
        pos: Animated<glam::DVec2>,
        points: Animated<f64>,
        inner_r: Animated<f64>,
        outer_r: Animated<f64>,
        roundness: Animated<f64>,
        kind: StarKind,
    },
    Polygon {
        pos: Animated<glam::DVec2>,
        points: Animated<f64>,
        outer_r: Animated<f64>,
        roundness: Animated<f64>,
    },
}

#[derive(Deserialize)]
enum LegacyStyleKind {
    Fill {
        color: Animated<renamite_model::Color>,
        rule: FillRule,
    },
    Stroke {
        color: Animated<renamite_model::Color>,
        width: Animated<f64>,
        cap: StrokeCap,
        join: StrokeJoin,
        dash: Option<renamite_model::AnimatedDash>,
        #[serde(default = "default_miter_limit")]
        miter_limit: Animated<f64>,
    },
}

#[derive(Deserialize)]
enum LegacyModifierKind {
    TrimPath {
        start: Animated<f64>,
        end: Animated<f64>,
        offset: Animated<f64>,
        #[serde(default)]
        mode: TrimMode,
    },
    Repeater {
        copies: Animated<f64>,
        offset: Animated<f64>,
        transform: AnimatedTransform,
    },
    RoundCorners {
        radius: Animated<f64>,
    },
    OffsetPath {
        amount: Animated<f64>,
    },
    ZigZag {
        amplitude: Animated<f64>,
        frequency: Animated<f64>,
        points: Animated<f64>,
    },
    InflateDeflate {
        amount: Animated<f64>,
    },
}

#[derive(Deserialize)]
struct LegacyTextNode {
    text: String,
}

#[derive(Deserialize)]
struct LegacyMaskProps {
    inverted: bool,
}

#[derive(Deserialize)]
enum LegacyAsset {
    Image,
}

impl LegacyRenFile {
    fn into_current(self) -> RenFile {
        let document = self.document.into_current();
        RenFile {
            format_version: self.format_version,
            meta: self.meta,
            document,
            clips: self.clips,
            machines: self.machines,
            start_machine: self.start_machine,
            clip_order: self.clip_order,
            machine_order: self.machine_order,
        }
    }
}

impl LegacyDocument {
    fn into_current(self) -> Document {
        let mut compositions = CompMap::default();
        let mut composition_ids = HashMap::new();
        for (old_id, composition) in self.compositions {
            let new_id = compositions.insert(composition);
            composition_ids.insert(old_id, new_id);
        }

        let mut assets = AssetMap::default();
        let mut asset_ids = HashMap::new();
        for (old_id, _) in self.assets {
            let new_id = assets.insert(Asset::Image(ImageAsset {
                name: String::new(),
                mime: String::new(),
                bytes: Vec::new(),
                width: 1,
                height: 1,
                srgb: true,
            }));
            asset_ids.insert(old_id, new_id);
        }

        let topology = self
            .nodes
            .iter()
            .map(|(id, node)| (id, node.parent, node.children.clone()))
            .collect::<Vec<_>>();
        let mut nodes = NodeMap::default();
        let mut node_ids = HashMap::new();
        for (old_id, node) in self.nodes {
            let new_id = nodes.insert(Node {
                name: node.name,
                parent: None,
                children: Vec::new(),
                visible: node.visible,
                locked: node.locked,
                transform: node.transform,
                opacity: node.opacity,
                kind: match node.kind {
                    LegacyNodeKind::Group => NodeKind::Group,
                    LegacyNodeKind::Layer(layer) => NodeKind::Layer(layer),
                    LegacyNodeKind::Shape(shape) => NodeKind::Shape(shape.into_current()),
                    LegacyNodeKind::Style(style) => NodeKind::Style(style.into_current()),
                    LegacyNodeKind::Modifier(modifier) => {
                        NodeKind::Modifier(modifier.into_current())
                    }
                    LegacyNodeKind::Text(text) => NodeKind::Text(TextNode {
                        text: text.text,
                        size: Animated::new(48.0),
                        align: Default::default(),
                        font: None,
                        tracking: Animated::new(0.0),
                        leading: Animated::new(0.0),
                    }),
                    LegacyNodeKind::Image(asset) => NodeKind::Image(ImageNode::new(
                        asset_ids.get(&asset).copied().unwrap_or_default(),
                    )),
                    LegacyNodeKind::Precomp { comp, time_map } => NodeKind::Precomp {
                        comp: composition_ids.get(&comp).copied().unwrap_or_default(),
                        time_map,
                    },
                    LegacyNodeKind::Mask(mask) => NodeKind::Mask(MaskProps {
                        inverted: mask.inverted,
                        shape: ShapeKind::Path(Animated::new(Default::default())),
                    }),
                },
            });
            node_ids.insert(old_id, new_id);
        }

        for (old_id, parent, children) in topology {
            let Some(new_id) = node_ids.get(&old_id).copied() else {
                continue;
            };
            let node = nodes.get_mut(new_id).expect("inserted node missing");
            node.parent = parent.and_then(|id| node_ids.get(&id).copied());
            node.children = children
                .iter()
                .filter_map(|id| node_ids.get(id).copied())
                .collect();
        }

        for composition in compositions.values_mut() {
            composition.children = composition
                .children
                .iter()
                .filter_map(|id| node_ids.get(id).copied())
                .collect();
        }

        let mut document = Document {
            format_version: self.format_version,
            compositions,
            nodes,
            assets,
            asset_order: Vec::new(),
            main: composition_ids.get(&self.main).copied().unwrap_or_default(),
        };
        document.ensure_main_composition();
        document
    }
}

impl LegacyShapeKind {
    fn into_current(self) -> ShapeKind {
        match self {
            Self::Path(path) => ShapeKind::Path(path),
            Self::Rect { pos, size, rounded } => ShapeKind::Rect { pos, size, rounded },
            Self::Ellipse { pos, size } => ShapeKind::Ellipse { pos, size },
            Self::Star {
                pos,
                points,
                inner_r,
                outer_r,
                roundness,
                kind,
            } => ShapeKind::Star {
                pos,
                points,
                inner_r,
                outer_r,
                roundness,
                kind,
            },
            Self::Polygon {
                pos,
                points,
                outer_r,
                roundness,
            } => ShapeKind::Polygon {
                pos,
                points,
                outer_r,
                roundness,
            },
        }
    }
}

impl LegacyStyleKind {
    fn into_current(self) -> StyleKind {
        match self {
            Self::Fill { color, rule } => StyleKind::Fill {
                paint: StylePaint::Solid { color },
                rule,
            },
            Self::Stroke {
                color,
                width,
                cap,
                join,
                dash,
                miter_limit,
            } => StyleKind::Stroke {
                paint: StylePaint::Solid { color },
                width,
                cap,
                join,
                dash,
                miter_limit,
            },
        }
    }
}

impl LegacyModifierKind {
    fn into_current(self) -> ModifierKind {
        match self {
            Self::TrimPath {
                start,
                end,
                offset,
                mode,
            } => ModifierKind::TrimPath {
                start,
                end,
                offset,
                mode,
            },
            Self::Repeater {
                copies,
                offset,
                transform,
            } => ModifierKind::Repeater {
                copies,
                offset,
                transform: Box::new(transform),
                start_opacity: Animated::new(1.0),
                end_opacity: Animated::new(1.0),
            },
            Self::RoundCorners { radius } => ModifierKind::RoundCorners { radius },
            Self::OffsetPath { amount } => ModifierKind::OffsetPath { amount },
            Self::ZigZag {
                amplitude,
                frequency,
                points,
            } => ModifierKind::ZigZag {
                amplitude,
                frequency,
                smooth: points.value_at(0.0) > 0.0,
            },
            Self::InflateDeflate { amount } => ModifierKind::PuckerBloat { amount },
        }
    }
}

pub(crate) fn decode(bytes: &[u8]) -> Result<RenFile, postcard::Error> {
    postcard::from_bytes::<LegacyRenFile>(bytes).map(LegacyRenFile::into_current)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;

    #[derive(Serialize)]
    struct FixtureFile {
        format_version: u32,
        meta: Meta,
        document: FixtureDocument,
        clips: ClipMap,
        machines: MachineMap,
        start_machine: Option<MachineId>,
        clip_order: Vec<ClipId>,
        machine_order: Vec<MachineId>,
    }

    #[derive(Serialize)]
    struct FixtureDocument {
        format_version: u32,
        compositions: SlotMap<CompId, Composition>,
        nodes: SlotMap<NodeId, FixtureNode>,
        assets: SlotMap<AssetId, FixtureAsset>,
        main: CompId,
    }

    #[derive(Serialize)]
    struct FixtureNode {
        name: String,
        parent: Option<NodeId>,
        children: Vec<NodeId>,
        visible: bool,
        locked: bool,
        transform: AnimatedTransform,
        opacity: Animated<f64>,
        kind: FixtureNodeKind,
    }

    #[allow(dead_code)]
    #[derive(Serialize)]
    enum FixtureNodeKind {
        Group,
        Layer(()),
        Shape(()),
        Style(FixtureStyleKind),
        Modifier(()),
        Text(()),
        Image(()),
        Precomp { comp: CompId, time_map: TimeMap },
        Mask(()),
    }

    #[allow(dead_code)]
    #[derive(Serialize)]
    enum FixtureStyleKind {
        Fill {
            color: Animated<renamite_model::Color>,
            rule: FillRule,
        },
        Stroke {
            color: Animated<renamite_model::Color>,
            width: Animated<f64>,
            cap: StrokeCap,
            join: StrokeJoin,
            dash: Option<renamite_model::AnimatedDash>,
            miter_limit: Animated<f64>,
        },
    }

    #[allow(dead_code)]
    #[derive(Serialize)]
    enum FixtureAsset {
        Image,
    }

    #[test]
    fn decodes_legacy_style_payload() {
        let mut compositions = SlotMap::<CompId, Composition>::with_key();
        let main = compositions.insert(Composition::default());
        let mut nodes = SlotMap::<NodeId, FixtureNode>::with_key();
        let node = nodes.insert(FixtureNode {
            name: "legacy fill".into(),
            parent: None,
            children: Vec::new(),
            visible: true,
            locked: false,
            transform: AnimatedTransform::identity(),
            opacity: Animated::new(1.0),
            kind: FixtureNodeKind::Style(FixtureStyleKind::Fill {
                color: Animated::new(renamite_model::Color::BLACK),
                rule: FillRule::NonZero,
            }),
        });
        compositions.get_mut(main).unwrap().children.push(node);
        let payload = postcard::to_stdvec(&FixtureFile {
            format_version: 0,
            meta: Meta {
                name: "legacy".into(),
                ..Meta::default()
            },
            document: FixtureDocument {
                format_version: 0,
                compositions,
                nodes,
                assets: SlotMap::<AssetId, FixtureAsset>::with_key(),
                main,
            },
            clips: ClipMap::default(),
            machines: MachineMap::default(),
            start_machine: None,
            clip_order: Vec::new(),
            machine_order: Vec::new(),
        })
        .unwrap();
        let mut bytes = b"RENB".to_vec();
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend(payload);

        let decoded = super::super::open_binary_unormalized(&bytes).unwrap();
        assert_eq!(decoded.meta.name, "legacy");
        let decoded_node = decoded.document.nodes.values().next().unwrap();
        let NodeKind::Style(StyleKind::Fill { paint, .. }) = &decoded_node.kind else {
            panic!("expected legacy fill");
        };
        let StylePaint::Solid { color } = paint else {
            panic!("expected solid paint");
        };
        assert_eq!(color.base, renamite_model::Color::BLACK);
    }
}
