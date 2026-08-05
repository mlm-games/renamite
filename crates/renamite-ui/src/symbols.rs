#![allow(non_upper_case_globals)]

use repose_core::{View, theme};
use repose_material::{Icon, Symbol};
use repose_ui::TextStyle;

repose_material::material_symbols! {
    menu:                '\u{e5d2}',
    undo:                '\u{e166}',
    redo:                '\u{e15a}',
    play_arrow:          '\u{e037}',
    pause:               '\u{e034}',
    save:                '\u{e161}',
    folder_open:         '\u{e2c8}',
    more_vert:           '\u{e5d4}',

    arrow_selector_tool: '\u{f82f}',
    edit:                '\u{f097}',
    rectangle:           '\u{eb54}',
    circle:              '\u{ef4a}',
    star:                '\u{f09a}',
    gradient:            '\u{e3e9}',
    format_color_fill:   '\u{e23a}',

    fit_screen:          '\u{ea10}',
    zoom_in:             '\u{e8ff}',
    zoom_out:            '\u{e900}',

    layers:              '\u{e53b}',
    settings:            '\u{e8b8}',
}

pub fn AppIcon(symbol: Symbol, size: f32) -> View {
    Icon(symbol).size(size).single_line()
}

pub fn MutedIcon(symbol: Symbol, size: f32) -> View {
    AppIcon(symbol, size).color(theme().on_surface_variant)
}
