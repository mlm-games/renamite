//! A self-contained HSV color picker: saturation/value square, hue strip,
//! alpha strip, hex text entry, and a recent-swatches row. Pure math lives in
//! `renamite_behavior_common::color`; this file only wires pointer events and
//! draws.

use renamite_behavior_common::color::{Hsv, hsv_to_rgb, parse_hex, rgb_to_hsv, to_hex};
use renamite_model::Color as ModelColor;
use repose_canvas::{Canvas, DrawScope};
use repose_core::geometry::Rect;
use repose_core::input::PointerEvent;
use repose_core::{AlignItems, Color, Modifier, View, theme};
use repose_material::material3::{Button, ButtonConfig, TextField, TextFieldConfig};
use repose_ui::{Box, Column, Row, Text, TextStyle, ViewExt};
use std::cell::RefCell;
use std::rc::Rc;

const SV_SIZE: f32 = 180.0;
const STRIP_H: f32 = 16.0;
const STRIP_GAP: f32 = 8.0;

/// Local editing state for one open picker instance. Kept outside the session
/// history because it is pure view/editing state for whichever popover is
/// open; the caller stores it in the session's `OpenPicker`.
#[derive(Clone)]
pub struct PickerState {
    pub hsv: Hsv,
    pub alpha: f64,
    pub hex_draft: String,
    pub hex_error: bool,
    /// Which strip is mid-drag (persisted so it survives frame rebuilds).
    dragging_sv: bool,
    dragging_hue: bool,
    dragging_alpha: bool,
}

impl PickerState {
    pub fn from_color(c: ModelColor) -> Self {
        let hsv = rgb_to_hsv(c);
        Self {
            hsv,
            alpha: c.a,
            hex_draft: to_hex(c, false),
            hex_error: false,
            dragging_sv: false,
            dragging_hue: false,
            dragging_alpha: false,
        }
    }

    pub fn color(&self) -> ModelColor {
        hsv_to_rgb(self.hsv, self.alpha)
    }

    fn sync_hex(&mut self) {
        self.hex_draft = to_hex(self.color(), false);
        self.hex_error = false;
    }

    fn set_color(&mut self, c: ModelColor) {
        let alpha = self.alpha;
        self.hsv = rgb_to_hsv(c);
        self.alpha = alpha;
        self.sync_hex();
    }
}

/// Renders the picker and calls `on_change` live while dragging, `on_commit`
/// once per gesture end (drag release / swatch click) so the caller can wrap
/// history transactions correctly.
#[allow(clippy::type_complexity)]
pub fn ColorPicker(
    state: std::rc::Rc<std::cell::RefCell<PickerState>>,
    swatches: Vec<ModelColor>,
    on_change: Rc<dyn Fn(ModelColor)>,
    on_commit: Rc<dyn Fn(ModelColor)>,
    on_add_swatch: Rc<dyn Fn(ModelColor)>,
    on_done: Rc<dyn Fn()>,
) -> View {
    Column(
        Modifier::new()
            .gap(STRIP_GAP)
            .padding(12.0)
            .background(theme().surface_container_high)
            .clip_rounded(12.0),
    )
    .child((
        sv_square(state.clone(), on_change.clone(), on_commit.clone()),
        hue_strip(state.clone(), on_change.clone(), on_commit.clone()),
        alpha_strip(state.clone(), on_change.clone(), on_commit.clone()),
        hex_row(state.clone(), on_change.clone(), on_commit.clone()),
        swatch_row(state, swatches, on_change, on_commit, on_add_swatch),
        Button(
            Modifier::new().fill_max_width(),
            move || on_done(),
            ButtonConfig::default(),
            || Text("Done"),
        ),
    ))
}

fn sv_square(
    state: Rc<RefCell<PickerState>>,
    on_change: Rc<dyn Fn(ModelColor)>,
    on_commit: Rc<dyn Fn(ModelColor)>,
) -> View {
    let update = {
        let state = state.clone();
        let on_change = on_change.clone();
        move |pe: &PointerEvent| {
            let x = (pe.position.x / SV_SIZE).clamp(0.0, 1.0) as f64;
            let y = (1.0 - pe.position.y / SV_SIZE).clamp(0.0, 1.0) as f64;
            let mut s = state.borrow_mut();
            s.hsv.s = x;
            s.hsv.v = y;
            s.sync_hex();
            on_change(s.color());
        }
    };

    Canvas(
        Modifier::new()
            .width(SV_SIZE)
            .height(SV_SIZE)
            .clip_rounded(8.0)
            .on_pointer_down({
                let state = state.clone();
                let update = update.clone();
                move |pe: PointerEvent| {
                    state.borrow_mut().dragging_sv = true;
                    update(&pe);
                }
            })
            .on_pointer_move({
                let state = state.clone();
                let update = update.clone();
                move |pe: PointerEvent| {
                    if state.borrow().dragging_sv {
                        update(&pe);
                    }
                }
            })
            .on_pointer_up({
                let state = state.clone();
                let on_commit = on_commit.clone();
                move |_pe: PointerEvent| {
                    if state.borrow_mut().dragging_sv {
                        state.borrow_mut().dragging_sv = false;
                        on_commit(state.borrow().color());
                    }
                }
            }),
        move |scope| {
            let hue = state.borrow().hsv.h;
            paint_sv_square(scope, hue);
            let s = state.borrow();
            let px = (s.hsv.s as f32) * SV_SIZE;
            let py = (1.0 - s.hsv.v as f32) * SV_SIZE;
            let r = 5.0;
            scope.draw_rect_stroke(
                Rect {
                    x: px - r,
                    y: py - r,
                    w: r * 2.0,
                    h: r * 2.0,
                },
                Color::WHITE,
                r,
                2.0,
            );
            scope.draw_rect_stroke(
                Rect {
                    x: px - r - 1.0,
                    y: py - r - 1.0,
                    w: r * 2.0 + 2.0,
                    h: r * 2.0 + 2.0,
                },
                Color(0, 0, 0, 180),
                r + 1.0,
                1.0,
            );
        },
    )
}

/// Bake the saturation/value gradient as a small grid of solid rects (cheap,
/// no shader needed). 24x24 cells is smooth enough at 180px.
fn paint_sv_square(scope: &mut DrawScope, hue: f64) {
    const CELLS: i32 = 24;
    let cell = SV_SIZE / CELLS as f32;
    for gy in 0..CELLS {
        for gx in 0..CELLS {
            let s = gx as f64 / (CELLS - 1) as f64;
            let v = 1.0 - gy as f64 / (CELLS - 1) as f64;
            let c = hsv_to_rgb(Hsv { h: hue, s, v }, 1.0);
            scope.draw_rect(
                Rect {
                    x: gx as f32 * cell,
                    y: gy as f32 * cell,
                    w: cell + 0.5,
                    h: cell + 0.5,
                },
                Color::from_rgba(
                    (c.r * 255.0) as u8,
                    (c.g * 255.0) as u8,
                    (c.b * 255.0) as u8,
                    255,
                ),
                0.0,
            );
        }
    }
}

fn hue_strip(
    state: Rc<RefCell<PickerState>>,
    on_change: Rc<dyn Fn(ModelColor)>,
    on_commit: Rc<dyn Fn(ModelColor)>,
) -> View {
    let update = {
        let state = state.clone();
        let on_change = on_change.clone();
        move |pe: &PointerEvent| {
            let h = (pe.position.x / SV_SIZE).clamp(0.0, 1.0) as f64 * 359.999;
            let mut s = state.borrow_mut();
            s.hsv.h = h;
            s.sync_hex();
            on_change(s.color());
        }
    };

    Canvas(
        Modifier::new()
            .width(SV_SIZE)
            .height(STRIP_H)
            .clip_rounded(6.0)
            .on_pointer_down({
                let state = state.clone();
                let update = update.clone();
                move |pe: PointerEvent| {
                    state.borrow_mut().dragging_hue = true;
                    update(&pe);
                }
            })
            .on_pointer_move({
                let state = state.clone();
                let update = update.clone();
                move |pe: PointerEvent| {
                    if state.borrow().dragging_hue {
                        update(&pe);
                    }
                }
            })
            .on_pointer_up({
                let state = state.clone();
                let on_commit = on_commit.clone();
                move |_| {
                    if state.borrow_mut().dragging_hue {
                        state.borrow_mut().dragging_hue = false;
                        on_commit(state.borrow().color());
                    }
                }
            }),
        move |scope| {
            const CELLS: i32 = 36;
            let cell_w = SV_SIZE / CELLS as f32;
            for i in 0..CELLS {
                let hue = i as f64 / (CELLS - 1) as f64 * 360.0;
                let c = hsv_to_rgb(
                    Hsv {
                        h: hue,
                        s: 1.0,
                        v: 1.0,
                    },
                    1.0,
                );
                scope.draw_rect(
                    Rect {
                        x: i as f32 * cell_w,
                        y: 0.0,
                        w: cell_w + 0.5,
                        h: STRIP_H,
                    },
                    Color::from_rgba(
                        (c.r * 255.0) as u8,
                        (c.g * 255.0) as u8,
                        (c.b * 255.0) as u8,
                        255,
                    ),
                    0.0,
                );
            }
            let hue = state.borrow().hsv.h;
            let x = (hue / 360.0) as f32 * SV_SIZE;
            draw_strip_cursor(scope, x, STRIP_H);
        },
    )
}

fn alpha_strip(
    state: Rc<RefCell<PickerState>>,
    on_change: Rc<dyn Fn(ModelColor)>,
    on_commit: Rc<dyn Fn(ModelColor)>,
) -> View {
    let update = {
        let state = state.clone();
        let on_change = on_change.clone();
        move |pe: &PointerEvent| {
            let a = (pe.position.x / SV_SIZE).clamp(0.0, 1.0) as f64;
            let mut s = state.borrow_mut();
            s.alpha = a;
            on_change(s.color());
        }
    };

    Canvas(
        Modifier::new()
            .width(SV_SIZE)
            .height(STRIP_H)
            .clip_rounded(6.0)
            .on_pointer_down({
                let state = state.clone();
                let update = update.clone();
                move |pe: PointerEvent| {
                    state.borrow_mut().dragging_alpha = true;
                    update(&pe);
                }
            })
            .on_pointer_move({
                let state = state.clone();
                let update = update.clone();
                move |pe: PointerEvent| {
                    if state.borrow().dragging_alpha {
                        update(&pe);
                    }
                }
            })
            .on_pointer_up({
                let state = state.clone();
                let on_commit = on_commit.clone();
                move |_| {
                    if state.borrow_mut().dragging_alpha {
                        state.borrow_mut().dragging_alpha = false;
                        on_commit(state.borrow().color());
                    }
                }
            }),
        move |scope| {
            // Checkerboard backdrop so alpha=0 is visually distinguishable.
            let th = theme();
            const CELL: f32 = 8.0;
            let cols = (SV_SIZE / CELL).ceil() as i32;
            for i in 0..cols {
                let bg = if i % 2 == 0 {
                    th.surface
                } else {
                    th.surface_container_high
                };
                scope.draw_rect(
                    Rect {
                        x: i as f32 * CELL,
                        y: 0.0,
                        w: CELL,
                        h: STRIP_H,
                    },
                    bg,
                    0.0,
                );
            }
            // Solid color, alpha ramp left(0)->right(1).
            let base = state.borrow().color();
            const CELLS: i32 = 36;
            let cell_w = SV_SIZE / CELLS as f32;
            for i in 0..CELLS {
                let a = i as f64 / (CELLS - 1) as f64;
                let c = Color::from_rgba(
                    (base.r * 255.0) as u8,
                    (base.g * 255.0) as u8,
                    (base.b * 255.0) as u8,
                    (a * 255.0) as u8,
                );
                scope.draw_rect(
                    Rect {
                        x: i as f32 * cell_w,
                        y: 0.0,
                        w: cell_w + 0.5,
                        h: STRIP_H,
                    },
                    c,
                    0.0,
                );
            }
            let x = state.borrow().alpha as f32 * SV_SIZE;
            draw_strip_cursor(scope, x, STRIP_H);
        },
    )
}

fn hex_row(
    state: Rc<RefCell<PickerState>>,
    on_change: Rc<dyn Fn(ModelColor)>,
    on_commit: Rc<dyn Fn(ModelColor)>,
) -> View {
    let draft = state.borrow().hex_draft.clone();
    let error = state.borrow().hex_error;

    Row(Modifier::new().gap(8.0).align_items(AlignItems::CENTER)).child((
        TextField(
            Modifier::new().width(120.0),
            draft,
            {
                let state = state.clone();
                let on_change = on_change.clone();
                move |text: String| {
                    let mut s = state.borrow_mut();
                    s.hex_draft = text.clone();
                    match parse_hex(&text) {
                        Some(c) => {
                            s.hex_error = false;
                            let alpha = s.alpha;
                            s.hsv = rgb_to_hsv(c);
                            s.alpha = if text.trim_start_matches('#').len() == 8 {
                                c.a
                            } else {
                                alpha
                            };
                            on_change(s.color());
                        }
                        None => s.hex_error = true,
                    }
                }
            },
            TextFieldConfig {
                is_error: error,
                single_line: true,
                ..Default::default()
            },
        ),
        // Live preview; click commits the (currently valid) hex.
        Box(Modifier::new()
            .width(28.0)
            .height(28.0)
            .clip_rounded(6.0)
            .background({
                let c = state.borrow().color();
                Color::from_rgba(
                    (c.r * 255.0) as u8,
                    (c.g * 255.0) as u8,
                    (c.b * 255.0) as u8,
                    (c.a * 255.0) as u8,
                )
            })
            .on_pointer_down({
                let state = state.clone();
                let on_commit = on_commit.clone();
                move |_| {
                    if !state.borrow().hex_error {
                        on_commit(state.borrow().color());
                    }
                }
            })),
    ))
}

fn swatch_row(
    state: Rc<RefCell<PickerState>>,
    swatches: Vec<ModelColor>,
    on_change: Rc<dyn Fn(ModelColor)>,
    on_commit: Rc<dyn Fn(ModelColor)>,
    on_add_swatch: Rc<dyn Fn(ModelColor)>,
) -> View {
    let th = theme();
    let mut cells: Vec<View> = swatches
        .into_iter()
        .map(|c| {
            swatch_cell(c, {
                let state = state.clone();
                let on_change = on_change.clone();
                let on_commit = on_commit.clone();
                move || {
                    state.borrow_mut().set_color(c);
                    on_change(c);
                    on_commit(c);
                }
            })
        })
        .collect();

    // "+" cell: save the current color as a new swatch.
    cells.push(
        Box(Modifier::new()
            .width(20.0)
            .height(20.0)
            .clip_rounded(4.0)
            .background(th.surface_container_low)
            .on_pointer_down({
                let state = state.clone();
                let on_add_swatch = on_add_swatch.clone();
                move |_| on_add_swatch(state.borrow().color())
            }))
        .child(
            Text("+")
                .size(th.typography.label_medium)
                .color(th.on_surface_variant),
        ),
    );

    Row(Modifier::new().gap(4.0)).child(cells)
}

fn swatch_cell(c: ModelColor, on_click: impl Fn() + 'static) -> View {
    Box(Modifier::new()
        .width(20.0)
        .height(20.0)
        .clip_rounded(4.0)
        .border(1.0, theme().outline_variant, 4.0)
        .background(Color::from_rgba(
            (c.r * 255.0) as u8,
            (c.g * 255.0) as u8,
            (c.b * 255.0) as u8,
            (c.a * 255.0) as u8,
        ))
        .on_pointer_down(move |_| on_click()))
}

fn draw_strip_cursor(scope: &mut DrawScope, x: f32, h: f32) {
    let w = 3.0;
    scope.draw_rect(
        Rect {
            x: x - 1.5,
            y: -2.0,
            w,
            h: h + 4.0,
        },
        Color::WHITE,
        1.0,
    );
    scope.draw_rect_stroke(
        Rect {
            x: x - 1.5,
            y: -2.0,
            w,
            h: h + 4.0,
        },
        Color(0, 0, 0, 180),
        1.0,
        1.0,
    );
}
