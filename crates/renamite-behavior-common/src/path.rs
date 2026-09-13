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
