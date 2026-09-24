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
#[cfg(target_arch = "wasm32")]
use std::fs;
use std::fs::File;
use std::io::Read;
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
        /// Seconds per baked frame. Defaults to 1 / composition rate.
        #[arg(short, long)]
        dt: Option<f64>,
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
        /// Seconds per baked frame. Defaults to 1 / composition rate.
        #[arg(long)]
        dt: Option<f64>,
        #[arg(long, default_value = "512")]
        width: u32,
        #[arg(long, default_value = "512")]
        height: u32,
        /// Single-frame output path
        #[arg(short, long, conflicts_with = "out_dir")]
        out: Option<PathBuf>,
        /// Sequence output directory
        #[arg(long, conflicts_with = "out")]
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

    /// Validate + normalize (optionally fix in place, or deep-validate)
    Validate {
        input: PathBuf,
        #[arg(long)]
        fix: bool,
        /// Run deep structural validation and print diagnostics
        #[arg(long)]
        deep: bool,
        /// Emit the deep validation report as JSON
        #[arg(long, requires = "deep")]
        json: bool,
        /// Exit 1 on warnings as well as errors in deep mode
        #[arg(long, requires = "deep")]
        warnings_as_errors: bool,
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

    /// Export a .ren/.renb project to a static SVG frame snapshot.
    ExportSvg {
        input: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        #[arg(long, default_value = "0")]
        frame: f64,
        /// Fail if the exporter emitted compatibility warnings.
        #[arg(long)]
        strict: bool,
    },

    /// Convert an SVG file to a Renamite project.
    ImportSvg {
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
        Commands::Validate {
            input,
            fix,
            deep,
            json,
            warnings_as_errors,
        } => cmd_validate(input, fix, deep, json, warnings_as_errors),
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
        Commands::ExportSvg {
            input,
            output,
            frame,
            strict,
        } => cmd_export_svg(input, output, frame, strict),
        Commands::ImportSvg {
            input,
            output,
            strict,
        } => cmd_import_svg(input, output, strict),
        Commands::Completions { shell } => {
            let mut cmd = Cli::command();
            let name = cmd.get_name().to_string();
            clap_complete::generate(shell, &mut cmd, name, &mut std::io::stdout());
            Ok(())
        }
    }
}

fn cmd_bake(input: PathBuf, frames: usize, dt: Option<f64>, output: PathBuf) -> Result<()> {
    validate_frame_count(frames, "--frames")?;
    if let Some(dt) = dt {
        validate_positive_duration(dt, "--dt")?;
    }
    let file = load_file(&input).with_context(|| format!("failed to load {}", input.display()))?;
    let mut player = Player::new(file)
        .with_context(|| format!("failed to open player for {}", input.display()))?;
    let dt = dt.unwrap_or_else(|| default_dt_for(&player));
    validate_positive_duration(dt, "--dt")?;
    let scenes = player.bake(frames, dt);
    let json = serde_json::to_string_pretty(&scenes)?;
    atomic_write(&output, json.as_bytes())?;
    println!("Baked {frames} frames -> {}", output.display());
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cmd_render(
    input: PathBuf,
    frame: Option<i64>,
    frames: Option<usize>,
    dt: Option<f64>,
    width: u32,
    height: u32,
    out: Option<PathBuf>,
    out_dir: Option<PathBuf>,
    prefix: String,
    background: String,
) -> Result<()> {
    match (frame, frames) {
        (Some(_), None) if out.is_none() => {
            bail!("--out is required with --frame")
        }
        (None, Some(_)) if out_dir.is_none() => {
            bail!("--out-dir is required with --frames")
        }
        (Some(_), None) if out_dir.is_some() => {
            bail!("--out-dir cannot be used with --frame")
        }
        (None, Some(_)) if out.is_some() => {
            bail!("--out cannot be used with --frames")
        }
        (None, None) => bail!("specify either --frame N or --frames N"),
        (Some(_), Some(_)) => bail!("specify only one of --frame or --frames"),
        _ => {}
    }
    validate_dimensions(width, height)?;
    if let Some(frames) = frames {
        validate_frame_count(frames, "--frames")?;
    }
    if let Some(dt) = dt {
        validate_positive_duration(dt, "--dt")?;
    }
    validate_render_prefix(&prefix)?;

    let bg = parse_background(&background)?;
    let mut player = Player::new(load_file(&input)?)
        .with_context(|| format!("failed to open player for {}", input.display()))?;
    let comp_size = player
        .project
        .document
        .compositions
        .get(player.project.document.main)
        .ok_or_else(|| anyhow!("main composition is missing"))?
        .size;
    let view = export_view(comp_size, width, height);
    let bg_clear = bg.map(|[r, g, b, a]| {
        [
            r as f64 / 255.0,
            g as f64 / 255.0,
            b as f64 / 255.0,
            a as f64 / 255.0,
        ]
    });

    let sequence_dir = if let Some(n) = frames {
        let dir = out_dir
            .clone()
            .ok_or_else(|| anyhow!("--out-dir is required with --frames"))?;
        std::fs::create_dir_all(&dir)?;
        let resolved_dt = dt.unwrap_or_else(|| default_dt_for(&player));
        validate_positive_duration(resolved_dt, "--dt")?;
        Some((n, dir, resolved_dt))
    } else {
        None
    };

    let mut bridge = SceneRenderer::new();
    let mut gpu = pollster::block_on(OffscreenRenderer::new(width, height, 4))?;
    gpu.sync_document_images(&player.project.document)?;

    match (frame, frames) {
        (Some(f), None) => {
            player.scrub(f as f64);
            let png = rasterize_png(&mut bridge, &mut gpu, player.scene(), &view, bg_clear)?;
            let out = out.ok_or_else(|| anyhow!("--out is required with --frame"))?;
            atomic_write(&out, &png)?;
            println!("Rendered frame {f} -> {}", out.display());
            Ok(())
        }
        (None, Some(n)) => {
            let (resolved_n, out_dir, resolved_dt) =
                sequence_dir.ok_or_else(|| anyhow!("--out-dir is required with --frames"))?;
            if resolved_n != n {
                bail!("render frame count changed while preparing output");
            }
            let scenes = player.bake(n, resolved_dt);
            for (i, scene) in scenes.iter().enumerate() {
                let png = rasterize_png(&mut bridge, &mut gpu, scene, &view, bg_clear)?;
                let path = out_dir.join(format!("{prefix}_{i:05}.png"));
                if !path.starts_with(&out_dir) || path.parent() != Some(out_dir.as_path()) {
                    bail!("render output path escaped {}", out_dir.display());
                }
                atomic_write(&path, &png)?;
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
            if !hex.is_ascii() || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                bail!(
                    "invalid background '{s}': expected 'transparent', 'white', 'black', or hex RRGGBB[AA]"
                );
            }
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
    let mut file = load_file(&input)?;
    file.normalize();
    atomic_write(&output, &renamite_io_ren::save_binary(&file)?)?;
    println!("Packed {} -> {}", input.display(), output.display());
    Ok(())
}

fn cmd_unpack(input: PathBuf, output: PathBuf) -> Result<()> {
    let file = load_file(&input)?;
    atomic_write(&output, renamite_io_ren::save(&file)?.as_bytes())?;
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
    let comp = file
        .document
        .compositions
        .get(file.document.main)
        .ok_or_else(|| anyhow!("main composition is missing"))?;
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

fn cmd_validate(
    input: PathBuf,
    fix: bool,
    deep: bool,
    json: bool,
    warnings_as_errors: bool,
) -> Result<()> {
    let RawFile {
        file: original,
        format,
    } = load_raw_file(&input)?;
    let original_bytes = save_for_format(&original, format)?;
    let mut normalized = original.clone();
    normalized.normalize();
    normalized.garbage_collect();
    let normalized_bytes = save_for_format(&normalized, format)?;
    let changed = original_bytes != normalized_bytes;

    let report = if deep || fix {
        Some(renamite_validate::validate(if fix {
            &normalized
        } else {
            &original
        }))
    } else {
        None
    };

    if let Some(report) = &report {
        if json {
            println!("{}", serde_json::to_string_pretty(report)?);
        } else if deep {
            for diagnostic in &report.diagnostics {
                println!(
                    "{:?}: {}: {}",
                    diagnostic.severity, diagnostic.path, diagnostic.message
                );
            }
            println!(
                "{} error(s), {} warning(s)",
                report.error_count(),
                report.warning_count()
            );
        }
        if report.has_errors() || (deep && warnings_as_errors && report.warning_count() > 0) {
            bail!(
                "validation failed: {} error(s), {} warning(s)",
                report.error_count(),
                report.warning_count()
            );
        }
    }

    if fix {
        if changed {
            atomic_write(&input, &normalized_bytes)?;
            if !json {
                println!("Normalized and saved {}", input.display());
            }
        } else if !json {
            println!("{} is already normalized", input.display());
        }
    } else if !deep {
        if changed {
            bail!("{} needs normalization (use --fix)", input.display());
        }
        if !json {
            println!("{} is valid", input.display());
        }
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
            bail!(
                "structural differences found: {} difference(s)",
                diffs.len()
            );
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

    let bytes = save_for_output(&file, &output)?;
    atomic_write(&output, &bytes)?;
    println!("Created {}", output.display());
    Ok(())
}

fn cmd_templates() -> Result<()> {
    println!("{}", templates_text());
    Ok(())
}

fn templates_text() -> String {
    let mut out =
        String::from("Available templates (use with `renamite new --template <slug>`):\n");
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

fn validate_frame_count(frames: usize, flag: &str) -> Result<()> {
    const MAX_FRAMES: usize = 1_000_000;
    if frames == 0 || frames > MAX_FRAMES {
        bail!("{flag} must be between 1 and {MAX_FRAMES}");
    }
    Ok(())
}

fn validate_dimensions(width: u32, height: u32) -> Result<()> {
    const MAX_DIMENSION: u32 = 16_384;
    const MAX_PIXELS: u64 = 64 * 1024 * 1024;
    if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
        bail!("image dimensions must be between 1 and {MAX_DIMENSION}");
    }
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or_else(|| anyhow!("image dimensions overflow"))?;
    if pixels > MAX_PIXELS {
        bail!("image dimensions exceed the {MAX_PIXELS}-pixel limit");
    }
    Ok(())
}

fn validate_positive_duration(value: f64, flag: &str) -> Result<()> {
    if !value.is_finite() || value <= 0.0 {
        bail!("{flag} must be finite and greater than zero");
    }
    Ok(())
}

fn validate_nonnegative_duration(value: f64, flag: &str) -> Result<()> {
    if !value.is_finite() || value < 0.0 {
        bail!("{flag} must be finite and non-negative");
    }
    Ok(())
}

fn validate_render_prefix(prefix: &str) -> Result<()> {
    if prefix.is_empty()
        || prefix == "."
        || prefix == ".."
        || prefix
            .chars()
            .any(|character| matches!(character, '/' | '\\' | '\0'))
    {
        bail!("--prefix must be a single file-name component");
    }
    let path = Path::new(prefix);
    if path.components().count() != 1
        || path.file_name().and_then(|name| name.to_str()) != Some(prefix)
    {
        bail!("--prefix must be a single file-name component");
    }
    Ok(())
}

/// Seconds per frame derived from the composition rate.
/// Falls back to 1/60 when the rate is zero, non-finite, or unavailable.
fn default_dt_for(player: &Player) -> f64 {
    let fps = player.rate().fps();
    if fps > 0.0 && fps.is_finite() {
        1.0 / fps
    } else {
        1.0 / 60.0
    }
}

fn cmd_play(input: PathBuf, duration: f64) -> Result<()> {
    validate_nonnegative_duration(duration, "--duration")?;
    let mut player = Player::new(load_file(&input)?)?;
    let dt = default_dt_for(&player);
    validate_positive_duration(dt, "composition frame duration")?;
    let tick_count = duration / dt;
    const MAX_TICKS: f64 = 10_000_000.0;
    if !tick_count.is_finite() || tick_count > MAX_TICKS {
        bail!("playback duration is too large");
    }
    let ticks = tick_count as usize;

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
    let report = renamite_io_lottie::export_project_with_report(
        &file.document,
        file.clip_order.len(),
        file.machine_order.len(),
        file.start_machine.is_some(),
    )?;
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
    atomic_write(&output, &serde_json::to_vec_pretty(&report.value)?)?;
    println!("Exported {} -> {}", input.display(), output.display());
    Ok(())
}

fn cmd_import_lottie(input: PathBuf, output: PathBuf, strict: bool) -> Result<()> {
    let bytes = read_limited(
        &input,
        renamite_io_lottie::MAX_LOTTIE_BYTES as u64,
        "Lottie JSON",
    )?;
    let report = renamite_io_lottie::import_bytes(&bytes)?;
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
        Some(extension) if extension.eq_ignore_ascii_case("renb") => {
            atomic_write(&output, &renamite_io_ren::save_binary(&file)?)?;
        }
        _ => {
            atomic_write(&output, renamite_io_ren::save(&file)?.as_bytes())?;
        }
    }
    println!("Imported {} -> {}", input.display(), output.display());
    Ok(())
}

fn cmd_export_svg(input: PathBuf, output: PathBuf, frame: f64, strict: bool) -> Result<()> {
    if !frame.is_finite() {
        bail!("--frame must be finite");
    }
    let file = load_file(&input)?;
    let report = renamite_io_svg::export_project_with_report(
        &file.document,
        file.document.main,
        frame,
        file.clip_order.len(),
        file.machine_order.len(),
        file.start_machine.is_some(),
    )?;
    if strict && !report.warnings.is_empty() {
        for warning in &report.warnings {
            eprintln!("warning at {}: {}", warning.path, warning.message);
        }
        bail!(
            "SVG export produced {} compatibility warning(s)",
            report.warnings.len()
        );
    }
    for warning in &report.warnings {
        eprintln!("warning at {}: {}", warning.path, warning.message);
    }
    atomic_write(&output, report.value.as_bytes())?;
    println!(
        "Exported {} frame {frame} -> {}",
        input.display(),
        output.display()
    );
    Ok(())
}

fn cmd_import_svg(input: PathBuf, output: PathBuf, strict: bool) -> Result<()> {
    let bytes = read_limited(&input, renamite_io_svg::MAX_INPUT_BYTES as u64, "SVG")?;
    let report = renamite_io_svg::import_with_report(&bytes)?;
    if strict && !report.warnings.is_empty() {
        for warning in &report.warnings {
            eprintln!("warning at {}: {}", warning.path, warning.message);
        }
        bail!(
            "SVG import produced {} compatibility warning(s)",
            report.warnings.len()
        );
    }
    for warning in &report.warnings {
        eprintln!("warning at {}: {}", warning.path, warning.message);
    }
    let file = RenFile::new(report.value, name_from_path(&input));
    match output.extension().and_then(|extension| extension.to_str()) {
        Some(extension) if extension.eq_ignore_ascii_case("renb") => {
            atomic_write(&output, &renamite_io_ren::save_binary(&file)?)?;
        }
        _ => {
            atomic_write(&output, renamite_io_ren::save(&file)?.as_bytes())?;
        }
    }
    println!("Imported {} -> {}", input.display(), output.display());
    Ok(())
}

#[derive(Clone, Copy, Debug)]
enum InputFormat {
    Text,
    Binary,
}

struct RawFile {
    file: RenFile,
    format: InputFormat,
}

fn read_limited(path: &Path, max_bytes: u64, label: &str) -> Result<Vec<u8>> {
    let mut file =
        File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut bytes = Vec::new();
    file.by_ref()
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .with_context(|| format!("failed to read {}", path.display()))?;
    if bytes.len() as u64 > max_bytes {
        bail!("{label} input exceeds the {max_bytes}-byte limit");
    }
    Ok(bytes)
}

fn load_raw_file(path: &Path) -> Result<RawFile> {
    let bytes = read_limited(
        path,
        renamite_io_ren::MAX_TEXT_BYTES.max(renamite_io_ren::MAX_BINARY_BYTES) as u64,
        "ren",
    )?;
    if renamite_io_ren::is_binary(&bytes) {
        return Ok(RawFile {
            file: renamite_io_ren::open_binary_unormalized(&bytes)?,
            format: InputFormat::Binary,
        });
    }
    let text = std::str::from_utf8(&bytes)
        .with_context(|| format!("{} is neither valid UTF-8 .ren nor .renb", path.display()))?;
    Ok(RawFile {
        file: renamite_io_ren::open_unormalized(text)?,
        format: InputFormat::Text,
    })
}

fn load_file(path: &Path) -> Result<RenFile> {
    let mut file = load_raw_file(path)?.file;
    file.normalize();
    Ok(file)
}

/// Atomic file write: temp + rename so a crash/power loss can't leave a
/// truncated corrupt project (previously direct truncate+write).
#[cfg(not(target_arch = "wasm32"))]
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    renamite_platform::atomic_write(path, bytes)?;
    Ok(())
}

#[cfg(target_arch = "wasm32")]
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .ok_or_else(|| anyhow!("path has no file name: {}", path.display()))?;

    let (temporary, mut file) = (0..128)
        .find_map(|_| {
            let name = format!(
                ".{}.{}.{}.tmp",
                file_name.to_string_lossy(),
                std::process::id(),
                TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
            );
            let temporary = parent.join(name);
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
            {
                Ok(file) => Some(Ok((temporary, file))),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => None,
                Err(error) => Some(Err(error)),
            }
        })
        .ok_or_else(|| anyhow!("could not allocate a unique temporary file"))??;

    let result: Result<()> = (|| {
        match fs::metadata(path) {
            Ok(metadata) => fs::set_permissions(&temporary, metadata.permissions())?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        file.write_all(bytes)
            .with_context(|| format!("failed to write {}", temporary.display()))?;
        file.sync_all()
            .with_context(|| format!("failed to sync {}", temporary.display()))?;
        drop(file);
        fs::rename(&temporary, path).with_context(|| {
            format!(
                "failed to replace {} (temp {})",
                path.display(),
                temporary.display()
            )
        })?;
        sync_parent(parent)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(all(unix, target_arch = "wasm32"))]
fn sync_parent(parent: &Path) -> std::io::Result<()> {
    fs::File::open(parent)?.sync_all()
}

#[cfg(all(not(unix), target_arch = "wasm32"))]
fn sync_parent(_parent: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Serialize a project matching the output extension (.renb = binary).
fn save_for_output(file: &RenFile, output: &Path) -> Result<Vec<u8>> {
    let format = match output.extension().and_then(|s| s.to_str()) {
        Some(extension) if extension.eq_ignore_ascii_case("renb") => InputFormat::Binary,
        _ => InputFormat::Text,
    };
    save_for_format(file, format)
}

fn save_for_format(file: &RenFile, format: InputFormat) -> Result<Vec<u8>> {
    let (bytes, max) = match format {
        InputFormat::Binary => (
            renamite_io_ren::save_binary(file)?,
            renamite_io_ren::MAX_BINARY_BYTES,
        ),
        InputFormat::Text => (
            renamite_io_ren::save(file)?.into_bytes(),
            renamite_io_ren::MAX_TEXT_BYTES,
        ),
    };
    if bytes.len() > max {
        bail!("serialized project is too large");
    }
    Ok(bytes)
}
