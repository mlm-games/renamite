#![allow(non_upper_case_globals)]

use repose_core::{UnitExt, View, theme};
use repose_material::{Icon, Symbol};
use repose_ui::TextStyle;

repose_material::material_symbols! {
    menu:                '\u{e5d2}',
    undo:                '\u{e166}',
    redo:                '\u{e15a}',
    play_arrow:          '\u{e037}',
    pause:               '\u{e034}',
    skip_previous:       '\u{e045}',
    save:                '\u{e161}',
    save_as:             '\u{eb60}',
    folder_open:         '\u{e2c8}',
    more_vert:           '\u{e5d4}',
    add:                 '\u{e145}',
    file_upload:         '\u{f09b}',
    file_download:       '\u{f090}',
    image:               '\u{e3f4}',
    font_download:       '\u{e167}',
    check:               '\u{e5ca}',

    arrow_selector_tool: '\u{f82f}',
    gps_fixed:            '\u{e55c}',
    grid_on:             '\u{e3ec}',
    grid_guides:         '\u{f76f}',
    edit:                '\u{f097}',
    transform:           '\u{e428}',
    draw:                '\u{e746}',
    rectangle:           '\u{eb54}',
    circle:              '\u{ef4a}',
    star:                '\u{f09a}',
    text_fields:         '\u{e262}',
    gradient:            '\u{e3e9}',
    format_color_fill:   '\u{e23a}',
    colorize:            '\u{e3b8}',
    palette:             '\u{e40a}',
    content_cut:         '\u{e14e}',
    content_copy:        '\u{e14d}',

    fit_screen:          '\u{ea10}',
    zoom_in:             '\u{e8ff}',
    zoom_out:            '\u{e900}',

    layers:              '\u{e53b}',
    settings:            '\u{e8b8}',
    tune:                '\u{e429}',
    view_timeline:       '\u{eb85}',
    touch_app:           '\u{e913}',
    animation:           '\u{e71c}',

    visibility:          '\u{e8f4}',
    visibility_off:      '\u{e8f5}',
    lock:                '\u{e899}',
    lock_open:           '\u{e898}',
    expand_more:         '\u{e5cf}',
    expand_less:         '\u{e5ce}',
    unfold_more:         '\u{e5d7}',
    unfold_less:         '\u{e5d6}',
    chevron_right:       '\u{e5cc}',
    drag_indicator:      '\u{e945}',

    radio_button_unchecked: '\u{e836}',
    stop_circle:            '\u{ef71}',
    fiber_manual_record:    '\u{e061}',

    fast_rewind:            '\u{e020}',
    fast_forward:           '\u{e01f}',
    skip_next:              '\u{e044}',
    sync:                   '\u{e627}',
    swap_horiz:             '\u{e8d4}',
    arrow_right_alt:        '\u{e941}',

    remove:                '\u{e15b}',
    delete:                '\u{e92e}',
    account_tree:          '\u{e97a}',
}

pub fn AppIcon(symbol: Symbol, size: f32) -> View {
    Icon(symbol).size(size.sp()).single_line()
}

pub fn MutedIcon(symbol: Symbol, size: f32) -> View {
    AppIcon(symbol, size).color(theme().on_surface_variant)
}
