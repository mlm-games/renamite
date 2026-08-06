use renamite_behavior_timeline::{TimelineEvent, TimelineLayout};
use repose_canvas::Canvas;
use repose_core::input::PointerEvent;
use repose_core::{Modifier, View, theme};
use repose_ui::{Column, ViewExt};

use crate::components::PanelHeader;
use crate::session::{SessionRef, dispatch_timeline, map_modifiers, pe_pos};
use crate::symbols::Symbols;

pub fn TimelinePanel(session: SessionRef) -> View {
    let sess_draw = session.clone();
    let header = PanelHeader(Symbols::play_arrow, "Timeline", vec![]);

    let content = Canvas(
        Modifier::new()
            .fill_max_size()
            .on_pointer_down({
                let session = session.clone();
                move |pe: PointerEvent| {
                    let mut s = session.borrow_mut();
                    dispatch_timeline(
                        &mut s,
                        TimelineEvent::Press {
                            pos: pe_pos(&pe),
                            modifiers: map_modifiers(&pe),
                        },
                    );
                }
            })
            .on_pointer_up({
                let session = session.clone();
                move |pe: PointerEvent| {
                    let mut s = session.borrow_mut();
                    dispatch_timeline(
                        &mut s,
                        TimelineEvent::Release {
                            pos: pe_pos(&pe),
                            modifiers: map_modifiers(&pe),
                        },
                    );
                }
            }),
        move |scope| {
            let s = sess_draw.borrow();
            let th = theme();
            let layout = TimelineLayout {
                origin_x: 80.0,
                px_per_frame: 6.0,
                row_top: 28.0,
                row_height: 22.0,
                key_tolerance_px: 6.0,
            };
            let x = layout.frame_to_x(s.playback.head) as f32;
            scope.draw_rect(
                repose_core::geometry::Rect {
                    x: x - 1.0,
                    y: 0.0,
                    w: 2.0,
                    h: scope.size.height,
                },
                th.primary,
                0.0,
            );
            let _ = s.revision;
        },
    );

    Column(Modifier::new().fill_max_size()).child((header, content))
}
