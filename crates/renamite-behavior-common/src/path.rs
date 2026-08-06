//! Helpers for editing animated paths at the correct frame.

use renamite_animation::Frame;
use renamite_history::EditorCommand;
use renamite_model::{Document, NodeId, PropPath};

/// Determine whether path edits should target the static base path or a
/// keyframe at `playhead`, and whether a seed keyframe must be created first.
///
/// Returns `(edit_frame, seed_command)`:
/// - `edit_frame = None`  -> edit the static base path
/// - `edit_frame = Some(f)` -> edit the keyframe at `f`
/// - `seed_command` is an `AddKeyframe` that must be applied before edits
pub fn path_edit_target(
    doc: &Document,
    id: NodeId,
    playhead: Frame,
    record: bool,
) -> Option<(Option<Frame>, Option<EditorCommand>)> {
    let prop = PropPath::new("shape.path");
    let animated = doc.property_is_animated(id, &prop);

    if !record && !animated {
        return Some((None, None));
    }

    if doc.keyframe_data(id, &prop, playhead).is_some() {
        return Some((Some(playhead), None));
    }

    let value = doc.value_at(id, &prop, playhead.0 as f64).ok()?;
    Some((
        Some(playhead),
        Some(EditorCommand::AddKeyframe {
            id,
            prop,
            frame: playhead,
            value,
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use renamite_animation::Animated;
    use renamite_geometry::VectorPath;
    use renamite_model::{Node, NodeKind, Parent, ShapeKind, Value};

    fn doc_with_path() -> (Document, NodeId) {
        let mut doc = Document::empty();
        let id = doc.create_node(Node::new(
            "path",
            NodeKind::Shape(ShapeKind::Path(Animated::new(VectorPath::default()))),
        ));
        doc.attach(id, Parent::Comp(doc.main), 0).unwrap();
        (doc, id)
    }

    #[test]
    fn static_no_record_targets_base() {
        let (doc, id) = doc_with_path();
        let (frame, seed) = path_edit_target(&doc, id, Frame(10), false).unwrap();
        assert_eq!(frame, None);
        assert!(seed.is_none());
    }

    #[test]
    fn record_seeds_playhead_key() {
        let (doc, id) = doc_with_path();
        let (frame, seed) = path_edit_target(&doc, id, Frame(10), true).unwrap();
        assert_eq!(frame, Some(Frame(10)));
        assert!(matches!(
            seed,
            Some(EditorCommand::AddKeyframe {
                frame: Frame(10),
                ..
            })
        ));
    }

    #[test]
    fn animated_without_key_at_playhead_seeds() {
        let (mut doc, id) = doc_with_path();
        let prop = PropPath::new("shape.path");
        doc.add_keyframe(id, &prop, Frame(0), &Value::Path(VectorPath::default()))
            .unwrap();

        let (frame, seed) = path_edit_target(&doc, id, Frame(5), false).unwrap();
        assert_eq!(frame, Some(Frame(5)));
        assert!(seed.is_some());
    }

    #[test]
    fn animated_with_key_at_playhead_needs_no_seed() {
        let (mut doc, id) = doc_with_path();
        let prop = PropPath::new("shape.path");
        doc.add_keyframe(id, &prop, Frame(10), &Value::Path(VectorPath::default()))
            .unwrap();

        let (frame, seed) = path_edit_target(&doc, id, Frame(10), false).unwrap();
        assert_eq!(frame, Some(Frame(10)));
        assert!(seed.is_none());
    }
}
