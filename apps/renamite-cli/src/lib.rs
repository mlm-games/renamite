//! renamite CLI - implemented as a library so every command is directly
//! testable (no subprocess spawning). `main.rs` only calls [`run`].

use anyhow::{Context, Result, anyhow, bail};
use clap::{CommandFactory, Parser, Subcommand};
use renamite_behavior_common::ViewTransform;
use renamite_io_ren::RenFile;
use renamite_player::Player;
use renamite_render_bridge::SceneRenderer;
use renamite_render_offscreen::OffscreenRenderer;
use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(name = "renamite")]
#[command(about = "Runtime and tooling for .ren animations")]
#[command(version, author)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Bake animation frames to JSON scenes (golden tests / export)
    Bake {
        input: PathBuf,
        #[arg(short, long, default_value = "60")]
        frames: usize,
        #[arg(short, long, default_value = "0.016666667")]
        dt: f64,
        #[arg(short, long, default_value = "scenes.json")]
        output: PathBuf,
    },

    /// Rasterize to PNG via the Repose WGPU renderer: a single frame
    /// (--frame) or a sequence (--frames)
    Render {
        input: PathBuf,
        #[arg(long, conflicts_with = "frames")]
        frame: Option<i64>,
        #[arg(long, conflicts_with = "frame")]
        frames: Option<usize>,
        #[arg(long, default_value = "0.016666667")]
        dt: f64,
        #[arg(long, default_value = "512")]
        width: u32,
        #[arg(long, default_value = "512")]
        height: u32,
        /// Single-frame output path
        #[arg(short, long)]
        out: Option<PathBuf>,
        /// Sequence output directory
        #[arg(long)]
        out_dir: Option<PathBuf>,
        #[arg(long, default_value = "frame")]
        prefix: String,
        /// "transparent", "white", "black", or hex RRGGBB[AA]
        #[arg(long, default_value = "white")]
        background: String,
    },

    /// Pack .ren -> binary .renb
    Pack {
        input: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
    },

    /// Unpack .renb -> pretty .ren
    Unpack {
        input: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
    },

    /// Show project info
    Info {
        input: PathBuf,
        /// Emit a machine-readable JSON summary
        #[arg(long)]
        json: bool,
    },

    /// Validate + normalize (optionally fix in place)
    Validate {
        input: PathBuf,
        #[arg(long)]
        fix: bool,
    },

    /// Structural diff between two .ren/.renb files
    Diff {
        a: PathBuf,
        b: PathBuf,
        /// Exit with status 1 if any differences are found
        #[arg(long)]
        fail_on_diff: bool,
    },

    /// Scaffold a new .ren project
    New {
        output: PathBuf,
        /// Template slug, e.g. "blank", "bouncing-ball" (run `renamite templates`)
        #[arg(long, default_value = "ellipse")]
        template: String,
    },

    /// List built-in project templates
    Templates {},

    /// Headless playback (prints machine events)
    Play {
        input: PathBuf,
        #[arg(short, long, default_value = "5.0")]
        duration: f64,
    },

    /// Export a .ren/.renb project to Lottie JSON.
    ExportLottie {
        input: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        /// Fail if the exporter emitted compatibility warnings.
        #[arg(long)]
        strict: bool,
    },

    /// Convert Lottie JSON to a Renamite project.
    ImportLottie {
        input: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        /// Fail if unsupported objects were skipped.
        #[arg(long)]
        strict: bool,
    },

    /// Generate shell completions
    Completions { shell: clap_complete::Shell },
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    dispatch(cli.command)
}

/// Exposed for tests: parse from an explicit argv, bypassing `std::env::args`.
pub fn run_from<I, T>(args: I) -> Result<()>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    let cli = Cli::try_parse_from(args)?;
    dispatch(cli.command)
}

fn dispatch(command: Commands) -> Result<()> {
    match command {
        Commands::Bake {
            input,
            frames,
            dt,
            output,
        } => cmd_bake(input, frames, dt, output),
        Commands::Render {
            input,
            frame,
            frames,
            dt,
            width,
            height,
            out,
            out_dir,
            prefix,
            background,
        } => cmd_render(
            input, frame, frames, dt, width, height, out, out_dir, prefix, background,
        ),
        Commands::Pack { input, output } => cmd_pack(input, output),
        Commands::Unpack { input, output } => cmd_unpack(input, output),
        Commands::Info { input, json } => cmd_info(input, json),
        Commands::Validate { input, fix } => cmd_validate(input, fix),
        Commands::Diff { a, b, fail_on_diff } => cmd_diff(a, b, fail_on_diff),
        Commands::New { output, template } => cmd_new(output, template),
        Commands::Templates {} => cmd_templates(),
        Commands::Play { input, duration } => cmd_play(input, duration),
        Commands::ExportLottie {
            input,
            output,
            strict,
        } => cmd_export_lottie(input, output, strict),
        Commands::ImportLottie {
            input,
            output,
            strict,
        } => cmd_import_lottie(input, output, strict),
        Commands::Completions { shell } => {
            let mut cmd = Cli::command();
            let name = cmd.get_name().to_string();
            clap_complete::generate(shell, &mut cmd, name, &mut std::io::stdout());
            Ok(())
        }
    }
}

fn cmd_bake(input: PathBuf, frames: usize, dt: f64, output: PathBuf) -> Result<()> {
    let text = std::fs::read_to_string(&input)
        .with_context(|| format!("failed to read {}", input.display()))?;
    let mut player = Player::from_ren_str(&text)
        .with_context(|| format!("failed to load {}", input.display()))?;
    let scenes = player.bake(frames, dt);
    let json = serde_json::to_string_pretty(&scenes)?;
    std::fs::write(&output, json)?;
    println!("Baked {frames} frames -> {}", output.display());
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cmd_render(
    input: PathBuf,
    frame: Option<i64>,
    frames: Option<usize>,
    dt: f64,
    width: u32,
    height: u32,
    out: Option<PathBuf>,
    out_dir: Option<PathBuf>,
    prefix: String,
    background: String,
) -> Result<()> {
    if frame.is_none() && frames.is_none() {
        bail!("specify either --frame N or --frames N");
    }

    let bg = parse_background(&background)?;
    let text = std::fs::read_to_string(&input)
        .with_context(|| format!("failed to read {}", input.display()))?;
    let mut player = Player::from_ren_str(&text)?;
    let comp_size = player.project.document.compositions[player.project.document.main].size;
    let view = export_view(comp_size, width, height);
    let bg_clear = bg.map(|[r, g, b, a]| {
        [
            r as f64 / 255.0,
            g as f64 / 255.0,
            b as f64 / 255.0,
            a as f64 / 255.0,
        ]
    });

    let mut bridge = SceneRenderer::new();
    let mut gpu = pollster::block_on(OffscreenRenderer::new(width, height, 4))?;
    gpu.sync_document_images(&player.project.document)?;

    match (frame, frames) {
        (Some(f), None) => {
            player.engine.scrub(&player.project, f as f64);
            let png = rasterize_png(&mut bridge, &mut gpu, player.scene(), &view, bg_clear)?;
            let out = out.ok_or_else(|| anyhow!("--out is required with --frame"))?;
            std::fs::write(&out, png)?;
            println!("Rendered frame {f} -> {}", out.display());
            Ok(())
        }
        (None, Some(n)) => {
            let out_dir = out_dir.ok_or_else(|| anyhow!("--out-dir is required with --frames"))?;
            std::fs::create_dir_all(&out_dir)?;
            let scenes = player.bake(n, dt);
            for (i, scene) in scenes.iter().enumerate() {
                let png = rasterize_png(&mut bridge, &mut gpu, scene, &view, bg_clear)?;
                let path = out_dir.join(format!("{prefix}_{i:05}.png"));
                std::fs::write(&path, png)?;
            }
            println!("Rendered {n} frames -> {}", out_dir.display());
            Ok(())
        }
        (None, None) => unreachable!("guard at top of cmd_render"),
        (Some(_), Some(_)) => unreachable!("clap conflicts_with prevents this"),
    }
}

/// World -> pixel "contain" fit for the main composition.
fn export_view(comp_size: (u32, u32), out_w: u32, out_h: u32) -> ViewTransform {
    let (cw, ch) = (comp_size.0 as f64, comp_size.1 as f64);
    if cw <= 0.0 || ch <= 0.0 || out_w == 0 || out_h == 0 {
        return ViewTransform::identity();
    }
    let scale = (out_w as f64 / cw).min(out_h as f64 / ch);
    let ox = (out_w as f64 - cw * scale) * 0.5;
    let oy = (out_h as f64 - ch * scale) * 0.5;
    ViewTransform {
        scale,
        offset: glam::DVec2::new(ox, oy),
    }
}

fn rasterize_png(
    bridge: &mut SceneRenderer,
    gpu: &mut OffscreenRenderer,
    scene: &renamite_model::Scene,
    view: &ViewTransform,
    bg: Option<[f64; 4]>,
) -> Result<Vec<u8>> {
    let prepared = bridge.prepare(scene, view);
    let mut repose = repose_core::Scene::default();
    bridge.append_repose_scene(&prepared, &mut repose);
    gpu.render_png(&repose, bg)
}

fn parse_background(s: &str) -> Result<Option<[u8; 4]>> {
    match s {
        "transparent" | "none" => Ok(None),
        "white" => Ok(Some([255, 255, 255, 255])),
        "black" => Ok(Some([0, 0, 0, 255])),
        hex => {
            let hex = hex.trim_start_matches('#');
            let bytes = match hex.len() {
                6 => [
                    u8::from_str_radix(&hex[0..2], 16)?,
                    u8::from_str_radix(&hex[2..4], 16)?,
                    u8::from_str_radix(&hex[4..6], 16)?,
                    255,
                ],
                8 => [
                    u8::from_str_radix(&hex[0..2], 16)?,
                    u8::from_str_radix(&hex[2..4], 16)?,
                    u8::from_str_radix(&hex[4..6], 16)?,
                    u8::from_str_radix(&hex[6..8], 16)?,
                ],
                _ => bail!(
                    "invalid background '{s}': expected 'transparent', 'white', 'black', or hex RRGGBB[AA]"
                ),
            };
            Ok(Some(bytes))
        }
    }
}

fn cmd_pack(input: PathBuf, output: PathBuf) -> Result<()> {
    let text = std::fs::read_to_string(&input)?;
    let mut file: RenFile = renamite_io_ren::open(&text)?;
    file.normalize();
    std::fs::write(&output, renamite_io_ren::save_binary(&file)?)?;
    println!("Packed {} -> {}", input.display(), output.display());
    Ok(())
}

fn cmd_unpack(input: PathBuf, output: PathBuf) -> Result<()> {
    let bytes = std::fs::read(&input)?;
    let mut file: RenFile = renamite_io_ren::open_binary(&bytes)?;
    file.normalize();
    std::fs::write(&output, renamite_io_ren::save(&file)?)?;
    println!("Unpacked {} -> {}", input.display(), output.display());
    Ok(())
}

#[derive(Serialize)]
struct InfoSummary {
    path: String,
    name: String,
    format_version: u32,
    compositions: usize,
    nodes: usize,
    clips: usize,
    machines: usize,
    start_machine: Option<String>,
    main: MainCompInfo,
}

#[derive(Serialize)]
struct MainCompInfo {
    name: String,
    width: u32,
    height: u32,
    fps: f64,
    in_frame: i64,
    out_frame: i64,
}

fn cmd_info(input: PathBuf, json: bool) -> Result<()> {
    let file = load_file(&input)?;
    let comp = &file.document.compositions[file.document.main];
    let summary = InfoSummary {
        path: input.display().to_string(),
        name: file.meta.name.clone(),
        format_version: file.format_version,
        compositions: file.document.compositions.len(),
        nodes: file.document.nodes.len(),
        clips: file.clips.len(),
        machines: file.machines.len(),
        start_machine: file
            .start_machine
            .and_then(|id| file.machines.get(id))
            .map(|m| m.name.clone()),
        main: MainCompInfo {
            name: comp.name.clone(),
            width: comp.size.0,
            height: comp.size.1,
            fps: comp.rate.fps(),
            in_frame: comp.range.0.0,
            out_frame: comp.range.1.0,
        },
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
        return Ok(());
    }

    println!("File:           {}", summary.path);
    println!("Name:           {}", summary.name);
    println!("Format:         v{}", summary.format_version);
    println!("Compositions:   {}", summary.compositions);
    println!("Nodes:          {}", summary.nodes);
    println!("Clips:          {}", summary.clips);
    println!("Machines:       {}", summary.machines);
    if let Some(name) = &summary.start_machine {
        println!("Start machine:  {name}");
    }
    println!("\nMain composition:");
    println!("  Name:  {}", summary.main.name);
    println!("  Size:  {}x{}", summary.main.width, summary.main.height);
    println!("  Rate:  {:.2} fps", summary.main.fps);
    println!(
        "  Range: {} - {}",
        summary.main.in_frame, summary.main.out_frame
    );
    Ok(())
}

fn cmd_validate(input: PathBuf, fix: bool) -> Result<()> {
    let mut file = load_file(&input)?;
    let before_json = serde_json::to_string(&file)?;
    file.normalize();
    file.garbage_collect();

    if fix {
        std::fs::write(&input, renamite_io_ren::save_binary(&file)?)?;
        println!("Normalized and saved {}", input.display());
    } else if serde_json::to_string(&file)? == before_json {
        println!("{} is valid", input.display());
    } else {
        println!("{} needs normalization (use --fix)", input.display());
        std::process::exit(1);
    }
    Ok(())
}

fn cmd_diff(a: PathBuf, b: PathBuf, fail_on_diff: bool) -> Result<()> {
    let fa = load_file(&a)?;
    let fb = load_file(&b)?;
    let va = serde_json::to_value(&fa)?;
    let vb = serde_json::to_value(&fb)?;

    let mut diffs = Vec::new();
    diff_values("", &va, &vb, &mut diffs);

    if diffs.is_empty() {
        println!("No structural differences.");
    } else {
        println!("{} difference(s):", diffs.len());
        for d in &diffs {
            println!("  {d}");
        }
        if fail_on_diff {
            std::process::exit(1);
        }
    }
    Ok(())
}

/// Minimal recursive structural diff. Object keys are compared by name;
/// arrays of differing length are reported wholesale (no element alignment).
fn diff_values(path: &str, a: &Value, b: &Value, out: &mut Vec<String>) {
    match (a, b) {
        (Value::Object(ma), Value::Object(mb)) => {
            let mut keys: Vec<&String> = ma.keys().chain(mb.keys()).collect();
            keys.sort();
            keys.dedup();
            for k in keys {
                let sub = if path.is_empty() {
                    k.clone()
                } else {
                    format!("{path}.{k}")
                };
                match (ma.get(k), mb.get(k)) {
                    (Some(av), Some(bv)) => diff_values(&sub, av, bv, out),
                    (Some(_), None) => out.push(format!("- {sub} (removed)")),
                    (None, Some(_)) => out.push(format!("+ {sub} (added)")),
                    (None, None) => unreachable!(),
                }
            }
        }
        (Value::Array(aa), Value::Array(ba)) => {
            if aa.len() != ba.len() {
                out.push(format!("~ {path} (array len {} -> {})", aa.len(), ba.len()));
            } else {
                for (i, (av, bv)) in aa.iter().zip(ba.iter()).enumerate() {
                    diff_values(&format!("{path}[{i}]"), av, bv, out);
                }
            }
        }
        _ => {
            if a != b {
                out.push(format!("~ {path}: {a} -> {b}"));
            }
        }
    }
}

fn cmd_new(output: PathBuf, template: String) -> Result<()> {
    let name = name_from_path(&output);
    let mut file = match template.as_str() {
        // Legacy alias predating the renamite-examples template set.
        "ellipse" => scaffold_ellipse(name.clone()),
        other => match renamite_examples::parse_template(other) {
            Some(id) => renamite_examples::build_template(id),
            None => {
                let known: Vec<&str> = std::iter::once("ellipse")
                    .chain(renamite_examples::templates().iter().map(|t| t.id.slug()))
                    .collect();
                bail!(
                    "unknown template '{other}' (expected one of: {})",
                    known.join(", ")
                )
            }
        },
    };
    file.meta.name = name;

    let ext = output.extension().and_then(|s| s.to_str()).unwrap_or("ren");
    match ext {
        "renb" => std::fs::write(&output, renamite_io_ren::save_binary(&file)?)?,
        _ => std::fs::write(&output, renamite_io_ren::save(&file)?)?,
    }
    println!("Created {}", output.display());
    Ok(())
}

fn cmd_templates() -> Result<()> {
    println!("{}", templates_text());
    Ok(())
}

fn templates_text() -> String {
    let mut out = String::from("Available templates (use with `renamite new --template <slug>`):\n");
    for t in renamite_examples::templates() {
        out.push_str(&format!("  {:<18} {}\n", t.id.slug(), t.description));
    }
    out
}

fn scaffold_ellipse(name: String) -> RenFile {
    use renamite_animation::Animated;
    use renamite_model::{
        Color, Document, FillRule, Node, NodeKind, Parent, ShapeKind, StyleKind, StylePaint,
    };

    let mut doc = Document::empty();
    let comp = doc.main;
    let (w, h) = doc.compositions[comp].size;
    let center = glam::DVec2::new(w as f64 / 2.0, h as f64 / 2.0);

    let shape = doc.create_node(Node::new(
        "Ellipse",
        NodeKind::Shape(ShapeKind::Ellipse {
            pos: Animated::new(center),
            size: Animated::new(glam::DVec2::new(180.0, 180.0)),
        }),
    ));
    let fill = doc.create_node(Node::new(
        "Fill",
        NodeKind::Style(StyleKind::Fill {
            paint: StylePaint::solid(Color::rgba(0.96, 0.42, 0.18, 1.0)),
            rule: FillRule::NonZero,
        }),
    ));
    doc.attach(shape, Parent::Comp(comp), 0).unwrap();
    doc.attach(fill, Parent::Comp(comp), 1).unwrap();

    RenFile::new(doc, name)
}

fn name_from_path(p: &Path) -> String {
    p.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Untitled")
        .to_string()
}

fn cmd_play(input: PathBuf, duration: f64) -> Result<()> {
    let text = std::fs::read_to_string(&input)?;
    let mut player = Player::from_ren_str(&text)?;
    let dt = 1.0 / 60.0;
    let ticks = (duration / dt) as usize;

    println!("Playing {} for {duration:.1}s...", input.display());
    for _ in 0..ticks {
        for ev in player.tick(dt) {
            println!("  {ev}");
        }
    }
    println!("Done. Final head: {:.2}", player.head());
    Ok(())
}

fn cmd_export_lottie(input: PathBuf, output: PathBuf, strict: bool) -> Result<()> {
    let file = load_file(&input)?;
    let report = renamite_io_lottie::export_with_report(&file.document)?;
    if strict && !report.warnings.is_empty() {
        for warning in &report.warnings {
            eprintln!("warning at {}: {}", warning.path, warning.message);
        }
        bail!(
            "Lottie export produced {} compatibility warning(s)",
            report.warnings.len()
        );
    }
    for warning in &report.warnings {
        eprintln!("warning at {}: {}", warning.path, warning.message);
    }
    std::fs::write(&output, serde_json::to_vec_pretty(&report.value)?)?;
    println!("Exported {} -> {}", input.display(), output.display());
    Ok(())
}

fn cmd_import_lottie(input: PathBuf, output: PathBuf, strict: bool) -> Result<()> {
    let value: Value = serde_json::from_slice(&std::fs::read(&input)?)?;
    let report = renamite_io_lottie::import_with_report(&value)?;
    if strict && !report.warnings.is_empty() {
        for warning in &report.warnings {
            eprintln!("warning at {}: {}", warning.path, warning.message);
        }
        bail!(
            "Lottie import produced {} compatibility warning(s)",
            report.warnings.len()
        );
    }
    for warning in &report.warnings {
        eprintln!("warning at {}: {}", warning.path, warning.message);
    }
    let file = RenFile::new(report.value, name_from_path(&input));
    match output.extension().and_then(|extension| extension.to_str()) {
        Some("renb") => {
            std::fs::write(&output, renamite_io_ren::save_binary(&file)?)?;
        }
        _ => {
            std::fs::write(&output, renamite_io_ren::save(&file)?)?;
        }
    }
    println!("Imported {} -> {}", input.display(), output.display());
    Ok(())
}

fn load_file(path: &Path) -> Result<RenFile> {
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
    match ext {
        "renb" => Ok(renamite_io_ren::open_binary(&std::fs::read(path)?)?),
        _ => Ok(renamite_io_ren::open(&std::fs::read_to_string(path)?)?),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_named_colors() {
        assert_eq!(parse_background("transparent").unwrap(), None);
        assert_eq!(
            parse_background("white").unwrap(),
            Some([255, 255, 255, 255])
        );
        assert_eq!(parse_background("black").unwrap(), Some([0, 0, 0, 255]));
    }

    #[test]
    fn parses_hex_with_and_without_alpha() {
        assert_eq!(parse_background("#ff0000").unwrap(), Some([255, 0, 0, 255]));
        assert_eq!(
            parse_background("00ff0080").unwrap(),
            Some([0, 255, 0, 0x80])
        );
    }

    #[test]
    fn rejects_garbage_background() {
        assert!(parse_background("not-a-color").is_err());
        assert!(parse_background("#ff00").is_err());
    }

    #[test]
    fn diff_detects_added_removed_changed() {
        let a = json!({ "x": 1, "y": 2, "obj": { "same": 1 } });
        let b = json!({ "x": 5, "z": 3, "obj": { "same": 1 } });
        let mut diffs = Vec::new();
        diff_values("", &a, &b, &mut diffs);
        assert!(diffs.iter().any(|d| d.contains("~ x: 1 -> 5")));
        assert!(diffs.iter().any(|d| d.contains("- y (removed)")));
        assert!(diffs.iter().any(|d| d.contains("+ z (added)")));
        assert!(!diffs.iter().any(|d| d.contains("obj")));
    }

    #[test]
    fn diff_reports_array_length_change_wholesale() {
        let a = json!({ "arr": [1, 2, 3] });
        let b = json!({ "arr": [1, 2] });
        let mut diffs = Vec::new();
        diff_values("", &a, &b, &mut diffs);
        assert_eq!(diffs, vec!["~ arr (array len 3 -> 2)"]);
    }

    #[test]
    fn render_rejects_neither_frame_nor_frames() {
        let err = run_from(["renamite", "render", "x.ren"]).unwrap_err();
        assert!(err.to_string().contains("--frame"));
    }

    #[test]
    fn render_rejects_both_frame_and_frames() {
        let result = Cli::try_parse_from([
            "renamite", "render", "x.ren", "--frame", "1", "--frames", "10",
        ]);
        assert!(result.is_err(), "clap must reject mutually exclusive flags");
    }

    #[test]
    fn new_ellipse_roundtrips_through_pack_and_open() {
        let dir = tempfile::tempdir().unwrap();
        let ren_path = dir.path().join("scene.ren");
        cmd_new(ren_path.clone(), "ellipse".into()).unwrap();

        let file = load_file(&ren_path).unwrap();
        assert_eq!(file.document.nodes.len(), 2); // shape + fill
        assert_eq!(file.meta.name, "scene");

        let renb_path = dir.path().join("scene.renb");
        cmd_pack(ren_path, renb_path.clone()).unwrap();
        let repacked = load_file(&renb_path).unwrap();
        assert_eq!(repacked.document.nodes.len(), 2);
    }

    #[test]
    fn new_blank_has_no_nodes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.ren");
        cmd_new(path.clone(), "blank".into()).unwrap();
        let file = load_file(&path).unwrap();
        assert_eq!(file.document.nodes.len(), 0);
    }

    #[test]
    fn new_rejects_unknown_template() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.ren");
        assert!(cmd_new(path, "not-a-template".into()).is_err());
    }

    #[test]
    fn templates_lists_every_builtin_slug() {
        let text = templates_text();
        for t in renamite_examples::templates() {
            assert!(
                text.contains(t.id.slug()),
                "templates output must mention {}",
                t.id.slug()
            );
        }
    }

    #[test]
    fn parse_template_accepts_slugs_and_display_names() {
        use renamite_examples::TemplateId;
        for id in TemplateId::all() {
            assert_eq!(renamite_examples::parse_template(id.slug()), Some(*id));
            assert_eq!(
                renamite_examples::parse_template(id.display_name()),
                Some(*id)
            );
            assert_eq!(
                renamite_examples::parse_template(&id.slug().to_uppercase()),
                Some(*id),
                "template lookup must be case-insensitive"
            );
        }
        assert_eq!(renamite_examples::parse_template("nope"), None);
    }

    #[test]
    fn new_with_each_template_roundtrips() {
        use renamite_examples::TemplateId;
        for id in TemplateId::all() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(format!("{}.ren", id.slug()));
            cmd_new(path.clone(), id.slug().into()).unwrap();

            let loaded = load_file(&path).unwrap();
            let mut expected = renamite_examples::build_template(*id);
            expected.meta.name = id.slug().to_string();
            assert_eq!(
                serde_json::to_value(&loaded).unwrap(),
                serde_json::to_value(&expected).unwrap(),
                "template {} must survive save->load roundtrip",
                id.slug()
            );

            let renb = dir.path().join(format!("{}.renb", id.slug()));
            cmd_pack(path, renb.clone()).unwrap();
            let packed = load_file(&renb).unwrap();
            assert_eq!(
                serde_json::to_value(&packed).unwrap(),
                serde_json::to_value(&expected).unwrap(),
                "template {} must survive binary pack->unpack roundtrip",
                id.slug()
            );
        }
    }

    #[test]
    #[ignore]
    fn render_single_frame_writes_valid_png() {
        let dir = tempfile::tempdir().unwrap();
        let ren = dir.path().join("scene.ren");
        cmd_new(ren.clone(), "ellipse".into()).unwrap();

        let png = dir.path().join("out.png");
        cmd_render(
            ren,
            Some(0),
            None,
            1.0 / 60.0,
            64,
            64,
            Some(png.clone()),
            None,
            "frame".into(),
            "white".into(),
        )
        .unwrap();

        let bytes = std::fs::read(&png).unwrap();
        assert_eq!(
            &bytes[..8],
            &[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']
        );
    }

    #[test]
    #[ignore]
    fn render_sequence_writes_numbered_files() {
        let dir = tempfile::tempdir().unwrap();
        let ren = dir.path().join("scene.ren");
        cmd_new(ren.clone(), "ellipse".into()).unwrap();

        let out_dir = dir.path().join("frames");
        cmd_render(
            ren,
            None,
            Some(3),
            1.0 / 60.0,
            32,
            32,
            None,
            Some(out_dir.clone()),
            "f".into(),
            "transparent".into(),
        )
        .unwrap();

        for i in 0..3 {
            assert!(out_dir.join(format!("f_{i:05}.png")).exists());
        }
    }

    #[test]
    fn diff_of_identical_files_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.ren");
        let b = dir.path().join("b.ren");
        cmd_new(a.clone(), "ellipse".into()).unwrap();
        std::fs::copy(&a, &b).unwrap();

        let fa = load_file(&a).unwrap();
        let fb = load_file(&b).unwrap();
        let mut diffs = Vec::new();
        diff_values(
            "",
            &serde_json::to_value(&fa).unwrap(),
            &serde_json::to_value(&fb).unwrap(),
            &mut diffs,
        );
        assert!(diffs.is_empty());
    }

    #[test]
    fn validate_reports_clean_file_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("scene.ren");
        cmd_new(path.clone(), "ellipse".into()).unwrap();

        let before = std::fs::read(&path).unwrap();
        cmd_validate(path.clone(), false).unwrap(); // no --fix: must not rewrite
        let after = std::fs::read(&path).unwrap();
        assert_eq!(before, after);
    }
}
