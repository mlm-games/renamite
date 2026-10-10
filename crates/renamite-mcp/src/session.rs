//! The headless document session: one `RenFile` in memory, a player for
//! preview, and the tool operations an agent drives them with. The document
//! operations live in `ops` so the editor's control channel runs the same code.

use std::path::{Path, PathBuf};

use renamite_animation::LoopMode;
use renamite_io_ren::RenFile;
use renamite_player::Player;
use serde_json::{Value, json};

use crate::ops::{self, HandleTable};

pub struct Session {
    pub file: RenFile,
    player: Player,
    /// Stable per-session handles. Enumeration indices shift the moment a
    /// node is added or removed, so every id the tools hand out is a handle
    /// assigned on first sight and kept for the session's life.
    pub handles: HandleTable,
    path: Option<PathBuf>,
    dirty: bool,
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    pub fn new() -> Self {
        let file = RenFile::new(renamite_model::Document::empty(), "untitled");
        let player = Player::new(file.clone()).expect("an empty document opens");
        Self {
            file,
            player,
            handles: HandleTable::default(),
            path: None,
            dirty: false,
        }
    }

    /// The player owns machine state and the playhead, so every edit rebuilds
    /// it from the file. Authoring and then previewing is the loop; live
    /// tweaking of a playing rig is not a v2 goal.
    fn refresh_player(&mut self) {
        self.player = Player::new(self.file.clone()).expect("the session file opens");
    }

    pub fn document(&self) -> &renamite_model::Document {
        &self.file.document
    }

    pub fn main_comp(&self) -> Option<renamite_model::CompId> {
        Some(self.file.document.main)
    }

    pub fn composition_size(&self) -> (u32, u32) {
        ops::composition_size(&self.file)
    }

    pub fn mark_clean(&mut self) {
        self.dirty = false;
    }

    pub fn sync_handles(&mut self) {
        ops::sync_handles(&self.file, &mut self.handles);
    }

    pub fn project_new(
        &mut self,
        name: Option<&str>,
        width: u32,
        height: u32,
    ) -> Result<Value, String> {
        ops::project_new(&mut self.file, &mut self.handles, name, width, height)?;
        self.dirty = true;
        self.refresh_player();
        Ok(json!({"ok": true}))
    }

    pub fn project_open(&mut self, path: &Path) -> Result<Value, String> {
        let value = ops::project_open(&mut self.file, path)?;
        self.path = Some(path.to_path_buf());
        self.dirty = false;
        self.refresh_player();
        Ok(value)
    }

    pub fn project_save(&mut self, path: Option<&Path>) -> Result<Value, String> {
        let target = match path {
            Some(path) => path.to_path_buf(),
            None => self
                .path
                .clone()
                .ok_or("no path given and the document was never saved")?,
        };
        let value = ops::project_save(&mut self.file, &target)?;
        self.path = Some(target);
        self.dirty = false;
        Ok(value)
    }

    pub fn project_info(&mut self) -> Value {
        ops::project_info(&self.file, &mut self.handles)
    }

    pub fn draw_shape(&mut self, params: &Value) -> Result<Value, String> {
        let value = ops::draw_shape(&mut self.file, &mut self.handles, params)?;
        self.dirty = true;
        self.refresh_player();
        Ok(value)
    }

    pub fn set_paint(&mut self, params: &Value) -> Result<Value, String> {
        let value = ops::set_paint(&mut self.file, &mut self.handles, params)?;
        self.dirty = true;
        self.refresh_player();
        Ok(value)
    }

    pub fn transform(&mut self, params: &Value) -> Result<Value, String> {
        let value = ops::transform(&mut self.file, &mut self.handles, params)?;
        self.dirty = true;
        self.refresh_player();
        Ok(value)
    }

    pub fn delete(&mut self, params: &Value) -> Result<Value, String> {
        let value = ops::delete(&mut self.file, &mut self.handles, params)?;
        self.dirty = true;
        self.refresh_player();
        Ok(value)
    }

    pub fn timeline_set(&mut self, params: &Value) -> Result<Value, String> {
        let value = ops::timeline_set(&mut self.file, &mut self.handles, params)?;
        self.dirty = true;
        self.refresh_player();
        Ok(value)
    }

    pub fn timeline_remove(&mut self, params: &Value) -> Result<Value, String> {
        let value = ops::timeline_remove(&mut self.file, &mut self.handles, params)?;
        self.dirty = true;
        self.refresh_player();
        Ok(value)
    }

    pub fn timeline_info(&mut self, params: &Value) -> Result<Value, String> {
        ops::timeline_info(&self.file, &mut self.handles, params)
    }

    pub fn clip_new(&mut self, params: &Value) -> Result<Value, String> {
        let value = ops::clip_new(&mut self.file, params)?;
        self.dirty = true;
        self.refresh_player();
        Ok(value)
    }

    pub fn clip_track_set(&mut self, params: &Value) -> Result<Value, String> {
        let value = ops::clip_track_set(&mut self.file, &mut self.handles, params)?;
        self.dirty = true;
        self.refresh_player();
        Ok(value)
    }

    pub fn machine_new(&mut self, params: &Value) -> Result<Value, String> {
        let value = ops::machine_new(&mut self.file, params)?;
        self.dirty = true;
        self.refresh_player();
        Ok(value)
    }

    pub fn machine_input(&mut self, params: &Value) -> Result<Value, String> {
        let value = ops::machine_input(&mut self.file, params)?;
        self.dirty = true;
        self.refresh_player();
        Ok(value)
    }

    pub fn machine_state(&mut self, params: &Value) -> Result<Value, String> {
        let value = ops::machine_state(&mut self.file, params)?;
        self.dirty = true;
        Ok(value)
    }

    pub fn machine_transition(&mut self, params: &Value) -> Result<Value, String> {
        ops::machine_transition(&mut self.file, params)
    }

    pub fn validate(&self) -> Result<Value, String> {
        ops::validate(&self.file)
    }

    pub fn import_svg(&mut self, params: &Value) -> Result<Value, String> {
        let value = ops::import_svg(&mut self.file, params)?;
        self.dirty = true;
        self.refresh_player();
        Ok(value)
    }

    pub fn export_svg(&mut self, params: &Value) -> Result<Value, String> {
        ops::export_svg(&mut self.file, params)
    }

    pub fn render_png(&mut self, params: &Value) -> Result<Value, String> {
        let comp = self.composition_size();
        let scale = number(params, "scale").unwrap_or(1.0).max(0.01);
        let width = (comp.0 as f64 * scale).round().max(1.0) as u32;
        let height = (comp.1 as f64 * scale).round().max(1.0) as u32;
        // The session player carries machine state and the playhead, so a
        // playback or input_set call before this render applies here.
        match params.get("frame").and_then(Value::as_f64) {
            Some(frame) => self.seek(frame),
            None => {
                let _ = self.player.tick(1.0 / 60.0);
            }
        }
        let view = renamite_behavior_common::ViewTransform {
            scale,
            offset: glam::DVec2::new(
                (width as f64 - comp.0 as f64 * scale) * 0.5,
                (height as f64 - comp.1 as f64 * scale) * 0.5,
            ),
        };
        let mut bridge = renamite_render_bridge::SceneRenderer::new();
        let mut gpu = pollster::block_on(renamite_render_offscreen::OffscreenRenderer::new(
            width, height, 4,
        ))
        .map_err(|error| format!("offscreen renderer: {error}"))?;
        gpu.sync_document_images(&self.player.project.document)
            .map_err(|error| format!("image upload: {error}"))?;
        let prepared = bridge.prepare(self.player.scene(), &view);
        let mut repose = repose_core::Scene::default();
        bridge.append_repose_scene(&prepared, &mut repose);
        let png = gpu
            .render_png(&repose, None)
            .map_err(|error| format!("render: {error}"))?;
        match params.get("path").and_then(Value::as_str) {
            Some(path) => {
                std::fs::write(path, &png).map_err(|error| format!("{path}: {error}"))?;
                Ok(json!({"ok": true, "path": path, "width": width, "height": height}))
            }
            None => Ok(json!({
                "ok": true,
                "width": width,
                "height": height,
                "png_base64": ops::base64_encode(&png),
            })),
        }
    }

    /// Move the playhead to `frame`. `Player::scrub` leaves machine mode, which
    /// would orphan the inputs a rig's transitions gate on, so a machine is
    /// re-armed on the first seek and only ticked afterwards: re-arming mid
    /// session would reset the inputs the caller just set.
    fn seek(&mut self, frame: f64) {
        let rate = self.player.rate();
        let step = 1.0 / (rate.num as f64 / rate.den as f64).max(1.0);
        if let Some(machine) = self.file.start_machine {
            if self.player.active_machine_states().is_none() {
                self.player.play_machine(machine);
            }
            let mut ticks = 0;
            while (self.player.head() < frame || ticks == 0) && ticks < 100_000 {
                self.player.tick(step);
                ticks += 1;
            }
        } else {
            self.player.scrub(frame);
        }
    }

    pub fn playback(&mut self, params: &Value) -> Result<Value, String> {
        let action = params
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("play");
        match action {
            "play" => {
                let looped = params.get("loop").and_then(Value::as_bool).unwrap_or(true);
                let mode = if looped {
                    LoopMode::Loop
                } else {
                    LoopMode::Once
                };
                match self.file.start_machine {
                    Some(machine) => self.player.play_machine(machine),
                    None => {
                        self.player.play_timeline(mode);
                        true
                    }
                };
                true
            }
            "pause" => {
                self.player.pause();
                true
            }
            "scrub" => {
                let frame = params
                    .get("frame")
                    .and_then(Value::as_f64)
                    .ok_or("scrub needs a frame")?;
                self.player.scrub(frame);
                true
            }
            other => {
                return Err(format!(
                    "unknown playback action {other}: play, pause or scrub"
                ));
            }
        };
        Ok(json!({"ok": true, "action": action}))
    }

    pub fn input_set(&mut self, params: &Value) -> Result<Value, String> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or("input_set needs a name")?;
        let ok = if let Some(value) = params.get("bool").and_then(Value::as_bool) {
            self.player.set_bool(name, value)
        } else if let Some(value) = params.get("number").and_then(Value::as_f64) {
            self.player.set_number(name, value)
        } else {
            return Err("input_set needs bool or number".to_string());
        };
        Ok(json!({"ok": ok}))
    }

    pub fn input_fire(&mut self, params: &Value) -> Result<Value, String> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or("input_fire needs a name")?;
        Ok(json!({"ok": self.player.fire(name)}))
    }
}

fn number(params: &Value, key: &str) -> Result<f64, String> {
    ops::number(params, key)
}
