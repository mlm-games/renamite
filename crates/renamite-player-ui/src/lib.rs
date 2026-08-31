//! Repose embed widget for `renamite_player::Player`.
//!
//! A thin spine-style runtime shell: [`PlayerHost`] owns the player plus the
//! per-surface view/renderer state, and [`RenamitePlayer`] is a drop-in
//! `repose_canvas::Canvas` that fits the artboard, paints the evaluated
//! `Scene`, forwards pointer input, and drives playback while playing.
//!
//! ```no_run
//! use renamite_player_ui::{PlayerHost, RenamitePlayer};
//! use repose_core::RenderContext;
//! use std::cell::RefCell;
//! use std::rc::Rc;
//!
//! let host = Rc::new(RefCell::new(
//!     PlayerHost::from_ren_str("(document ...)").unwrap(),
//! ));
//! // host.play(); // or host.toggle(); buttons keep ticking while playing.
//! // In the view tree, with a live `RenderContext`:
//! let render_context = RenderContext::new();
//! RenamitePlayer(host, render_context);
//! ```
//!
//! Only the caveat that pointer input is routed in *world* coordinates: the
//! widget converts surface px through the current [`ViewTransform`], so hit
//! tests (machine listeners) match what the user sees.

use std::cell::RefCell;
use std::rc::Rc;

use glam::DVec2;
use renamite_behavior_common::ViewTransform;
use renamite_player::{Player, PlayerError};
use renamite_render_bridge::SceneRenderer;
use repose_canvas::{Canvas, DrawScope};
use repose_core::geometry::Rect;
use repose_core::input::PointerEvent;
use repose_core::{Color, Modifier, RenderContext, Vec2, View, request_frame, theme};
use web_time::Instant;

/// Shared host handle: the embed owner builds it and passes it to the widget.
pub type PlayerHostRef = Rc<RefCell<PlayerHost>>;

/// Per-surface state for one embedded player: the engine, the tessellator
/// (`SceneRenderer`), the viewport fit/zoom mapping, and playback control.
pub struct PlayerHost {
    pub player: Player,
    pub renderer: SceneRenderer,
    /// Screen - world mapping for the embed surface.
    pub view: ViewTransform,
    /// True while the widget keeps ticking playback each frame.
    pub playing: bool,
    pub last_tick: Instant,
    /// Set when image assets changed. Uploaded to `RenderContext` on the next
    /// draw. Constructors mark it dirty so a fresh renderer uploads once.
    pub dirty_images: bool,
    /// Most recent pointer position in surface px (wheel-zoom anchor).
    pub last_pointer: DVec2,
    surface_size: DVec2,
    fit_pending: bool,
}

impl PlayerHost {
    pub fn new(player: Player) -> Self {
        Self {
            player,
            renderer: SceneRenderer::new(),
            view: ViewTransform::identity(),
            playing: true,
            last_tick: Instant::now(),
            dirty_images: true,
            last_pointer: DVec2::ZERO,
            surface_size: DVec2::ZERO,
            fit_pending: true,
        }
    }

    pub fn from_ren_str(text: &str) -> Result<Self, PlayerError> {
        Ok(Self::new(Player::from_ren_str(text)?))
    }

    #[cfg(feature = "binary")]
    pub fn from_ren_bytes(bytes: &[u8]) -> Result<Self, PlayerError> {
        Ok(Self::new(Player::from_ren_bytes(bytes)?))
    }

    /// Artboard size of the active composition.
    pub fn artboard(&self) -> DVec2 {
        let comp = self.player.composition();
        match self.player.project.document.compositions.get(comp) {
            Some(c) => DVec2::new(c.size.0 as f64, c.size.1 as f64),
            None => DVec2::ZERO,
        }
    }

    /// Mark image assets dirty so the next draw re-uploads them to
    /// `RenderContext`. Call after replacing the player's project.
    pub fn mark_images_dirty(&mut self) {
        self.dirty_images = true;
    }

    pub fn play(&mut self) {
        self.playing = true;
        self.last_tick = Instant::now();
        request_frame();
    }

    pub fn pause(&mut self) {
        self.playing = false;
    }

    pub fn toggle(&mut self) {
        if self.playing {
            self.pause();
        } else {
            self.play();
        }
    }

    /// Advance playback by the wall-clock delta since the last call. Returns
    /// true while playing so callers (or hosts with their own loop) can keep
    /// requesting frames. Zero-cost whenever paused.
    pub fn tick_playback(&mut self) -> bool {
        if !self.playing {
            return false;
        }
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick).as_secs_f64().min(0.05);
        self.last_tick = now;
        let _ = self.player.tick(dt);
        true
    }

    fn ensure_fit(&mut self, surface: DVec2) {
        let resized = (surface - self.surface_size).abs().max_element() > 0.5;
        self.surface_size = surface;
        if self.fit_pending || resized {
            self.fit(surface);
        }
    }

    /// Fit the artboard inside `surface` with a margin, centering it.
    pub fn fit(&mut self, surface: DVec2) {
        let artboard = self.artboard();
        if surface.x <= 1.0 || surface.y <= 1.0 || artboard.x <= 0.0 || artboard.y <= 0.0 {
            return;
        }
        let margin = 56.0;
        let available = (surface - DVec2::splat(margin * 2.0)).max(DVec2::splat(1.0));
        let scale = (available.x / artboard.x)
            .min(available.y / artboard.y)
            .clamp(0.05, 32.0);
        self.view.scale = scale;
        self.view.offset = (surface - artboard * scale) * 0.5;
        self.fit_pending = false;
    }

    pub fn zoom_centered(&mut self, factor: f64) {
        if self.surface_size == DVec2::ZERO {
            return;
        }
        self.zoom_at(self.surface_size * 0.5, factor);
    }

    /// Zoom by `factor` keeping the world point under `screen_pos` anchored.
    pub fn zoom_at(&mut self, screen_pos: DVec2, factor: f64) {
        if self.surface_size == DVec2::ZERO {
            return;
        }
        let world = self.view.screen_to_world(screen_pos);
        self.view.scale = (self.view.scale * factor).clamp(0.05, 64.0);
        self.view.offset = screen_pos - world * self.view.scale;
    }
}

fn pe_position(pe: &PointerEvent) -> DVec2 {
    DVec2::new(pe.position.x as f64, pe.position.y as f64)
}

#[allow(non_snake_case)]
/// Drop-in embed: a full-surface `Canvas` that fits and paints the active
/// composition of `host`'s player, forwards pointer/scroll input, and ticks
/// playback each frame while `host.playing`.
pub fn RenamitePlayer(host: PlayerHostRef, render_context: RenderContext) -> View {
    let draw = host.clone();

    Canvas(
        Modifier::new()
            .fill_max_size()
            .background(theme().surface_container_lowest)
            .on_scroll({
                let host = host.clone();
                move |delta: Vec2| {
                    let mut h = host.borrow_mut();
                    let factor = (1.0 + (-delta.y as f64) * 0.002).clamp(0.5, 2.0);
                    let anchor = h.last_pointer;
                    h.zoom_at(anchor, factor);
                    request_frame();
                    Vec2::ZERO
                }
            })
            .on_pointer_down({
                let host = host.clone();
                move |pe: PointerEvent| {
                    let mut h = host.borrow_mut();
                    h.last_pointer = pe_position(&pe);
                    let world = h.view.screen_to_world(h.last_pointer);
                    h.player.pointer_down(world);
                    request_frame();
                }
            })
            .on_pointer_up({
                let host = host.clone();
                move |pe: PointerEvent| {
                    let mut h = host.borrow_mut();
                    let world = h.view.screen_to_world(pe_position(&pe));
                    h.player.pointer_up(world);
                    request_frame();
                }
            })
            .on_pointer_move({
                let host = host.clone();
                move |pe: PointerEvent| {
                    let mut h = host.borrow_mut();
                    h.last_pointer = pe_position(&pe);
                    let world = h.view.screen_to_world(h.last_pointer);
                    h.player.pointer_move(world);
                    request_frame();
                }
            })
            .on_pointer_leave({
                let host = host.clone();
                move |_pe: PointerEvent| {
                    host.borrow_mut().player.pointer_leave();
                    request_frame();
                }
            }),
        move |scope| {
            let mut h = draw.borrow_mut();

            if h.tick_playback() {
                request_frame();
            }

            let surface = DVec2::new(scope.size.width as f64, scope.size.height as f64);
            h.ensure_fit(surface);

            if h.dirty_images {
                let PlayerHost {
                    renderer, player, ..
                } = &mut *h;
                renderer.sync_document_images(&player.project.document, &render_context);
                h.dirty_images = false;
            }

            paint_artboard(scope, h.artboard(), &h.view);

            let scene = h.player.scene().clone();
            let view = h.view;
            let prepared = h.renderer.prepare(&scene, &view);
            h.renderer.paint_prepared(&prepared, scope);
        },
    )
}

/// Artboard backplate: soft shadow, checkerboard, and a one-pixel border.
fn paint_artboard(scope: &mut DrawScope, artboard: DVec2, view: &ViewTransform) {
    let th = theme();
    let origin = view.world_to_screen(DVec2::ZERO);
    let width = artboard.x * view.scale;
    let height = artboard.y * view.scale;

    // Shadow/backplate.
    scope.draw_rect(
        Rect {
            x: origin.x as f32 - 4.0,
            y: origin.y as f32 - 4.0,
            w: width as f32 + 8.0,
            h: height as f32 + 8.0,
        },
        Color(0, 0, 0, 48),
        3.0,
    );

    // Checkerboard (transparent pixels read as a neutral grid).
    let tile_world = 32.0;
    let cols = (artboard.x / tile_world).ceil() as usize;
    let rows = (artboard.y / tile_world).ceil() as usize;
    for y in 0..rows {
        for x in 0..cols {
            let p = view.world_to_screen(DVec2::new(x as f64 * tile_world, y as f64 * tile_world));
            let color = if (x + y) % 2 == 0 {
                th.surface
            } else {
                th.surface_container_high
            };
            scope.draw_rect(
                Rect {
                    x: p.x as f32,
                    y: p.y as f32,
                    w: (tile_world * view.scale).ceil() as f32,
                    h: (tile_world * view.scale).ceil() as f32,
                },
                color,
                0.0,
            );
        }
    }

    // One-pixel border.
    let border = th.outline_variant;
    let (x, y, w, h) = (
        origin.x as f32,
        origin.y as f32,
        width as f32,
        height as f32,
    );
    scope.draw_rect(Rect { x, y, w, h: 1.0 }, border, 0.0);
    scope.draw_rect(
        Rect {
            x,
            y: y + h - 1.0,
            w,
            h: 1.0,
        },
        border,
        0.0,
    );
    scope.draw_rect(Rect { x, y, w: 1.0, h }, border, 0.0);
    scope.draw_rect(
        Rect {
            x: x + w - 1.0,
            y,
            w: 1.0,
            h,
        },
        border,
        0.0,
    );
}
