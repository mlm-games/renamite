//! MCP prompts (`prompts/list`, `prompts/get`) and `completion/complete`.
//!
//! A prompt is a reusable renamite workflow: `prompts/get` hands the model one
//! templated user message, so a client can offer "build a hover rig" the way it
//! offers a slash command. Every prompt's arguments double as completion
//! sources, and the resource templates share the same [`Live`] catalogues, so
//! `completion/complete` suggests real node ids, clip names and machine names
//! instead of a fixed list that goes stale.

use serde_json::{Value, json};

use crate::backend::Backend;
use crate::resources::TEMPLATES;

/// How many values one `completion/complete` reply may carry (the spec's max).
const MAX_VALUES: usize = 100;

/// A catalogue completion values are read from, so they follow the project.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Live {
    /// Node ids and names in the open project.
    Nodes,
    /// Clip names in the open project.
    Clips,
    /// Machine names in the open project.
    Machines,
    /// Property paths a node carries.
    Properties,
    /// Import formats the server can read.
    Formats,
}

/// Where one prompt argument's suggestions come from.
#[derive(Clone, Copy)]
pub enum Values {
    /// A fixed list, written out here.
    Fixed(&'static [&'static str]),
    /// The live catalogue.
    Live(Live),
}

/// One prompt.
pub struct PromptDef {
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub template: &'static str,
    pub args: &'static [PromptArg],
}

/// One prompt argument.
pub struct PromptArg {
    pub name: &'static str,
    pub description: &'static str,
    pub required: bool,
    pub values: Option<Values>,
}

/// Every prompt, in `prompts/list` order.
pub static PROMPTS: &[PromptDef] = &[
    PromptDef {
        name: "rig-from-svg",
        title: "Rig an imported SVG",
        description: "Import an SVG, then build a hover/tap rig over it with a state machine.",
        template: "Import {path} and build an interactive rig over it. Start with import, then inspect_document. Give the shapes stable names, add a machine named \"interaction\" with hover and pressed states, and drive it through pointer events. Finish by rendering one frame of each state so I can see them.",
        args: &[PromptArg {
            name: "path",
            description: "The SVG file to import",
            required: true,
            values: None,
        }],
    },
    PromptDef {
        name: "lottie-roundtrip",
        title: "Lottie round-trip",
        description: "Import a Lottie file, keep it editable, export it back and diff.",
        template: "Import the Lottie file {path}. Inspect what came in, fix anything the importer dropped, then export it back to Lottie with strict enabled and report every compatibility warning.",
        args: &[PromptArg {
            name: "path",
            description: "The Lottie JSON file to import",
            required: true,
            values: None,
        }],
    },
    PromptDef {
        name: "animate-property",
        title: "Animate a property",
        description: "Key a node property across a frame range with easing.",
        template: "Animate {node}'s {prop} over frames {from} to {to}. Read the current value first, key the endpoints with property.key, set the easing with property.easing, then bake and report the evaluated value at each keyframe.",
        args: &[
            PromptArg {
                name: "node",
                description: "The node to animate, by id or name",
                required: true,
                values: Some(Values::Live(Live::Nodes)),
            },
            PromptArg {
                name: "prop",
                description: "The property path to animate",
                required: true,
                values: Some(Values::Live(Live::Properties)),
            },
            PromptArg {
                name: "from",
                description: "First frame",
                required: true,
                values: None,
            },
            PromptArg {
                name: "to",
                description: "Last frame",
                required: true,
                values: None,
            },
        ],
    },
    PromptDef {
        name: "audit",
        title: "Audit a project",
        description: "Validate deeply and report every structural problem.",
        template: "Audit the open project. Run validate with deep, walk every diagnostic, and for each one say what fixes it. If anything is safe to fix, fix it and re-validate.",
        args: &[],
    },
    PromptDef {
        name: "review-frames",
        title: "Review frames",
        description: "Render a frame range and describe what changed.",
        template: "Render every {step} frames from {from} to {to} and describe what changes between them. Point out anything that looks broken: misaligned shapes, missing paint, geometry that leaves the frame.",
        args: &[
            PromptArg {
                name: "from",
                description: "First frame",
                required: true,
                values: None,
            },
            PromptArg {
                name: "to",
                description: "Last frame",
                required: true,
                values: None,
            },
            PromptArg {
                name: "step",
                description: "Frames between renders",
                required: false,
                values: Some(Values::Fixed(&["1", "5", "10", "30"])),
            },
        ],
    },
];

pub fn list() -> Value {
    json!({
        "prompts": PROMPTS
            .iter()
            .map(|p| json!({
                "name": p.name,
                "title": p.title,
                "description": p.description,
                "arguments": p.args.iter().map(|a| json!({
                    "name": a.name,
                    "description": a.description,
                    "required": a.required,
                })).collect::<Vec<_>>(),
            }))
            .collect::<Vec<_>>(),
    })
}

/// Answer `prompts/get`. A missing required argument is an error.
pub fn get(name: &str, args: &Value) -> Result<Value, String> {
    let Some(prompt) = PROMPTS.iter().find(|p| p.name == name) else {
        return Err(format!("unknown prompt `{name}`"));
    };
    let object = args.as_object().cloned().unwrap_or_default();
    let mut text = prompt.template.to_string();
    for arg in prompt.args {
        let value = object.get(arg.name).and_then(Value::as_str);
        match (value, arg.required) {
            (Some(v), _) => {
                text = text.replace(&format!("{{{}}}", arg.name), v);
            }
            (None, true) => return Err(format!("missing required argument `{}`", arg.name)),
            (None, false) => {
                text = text.replace(&format!("{{{}}}", arg.name), "");
            }
        }
    }
    Ok(json!({
        "description": prompt.description,
        "messages": [{
            "role": "user",
            "content": {"type": "text", "text": text},
        }],
    }))
}

/// Read the live values one completion reference can suggest.
fn live_values(b: &mut dyn Backend, live: Live) -> Vec<String> {
    match live {
        Live::Nodes => crate::tools::node_catalogue(b.session()),
        Live::Clips => crate::tools::clip_catalogue(b.session()),
        Live::Machines => crate::tools::machine_catalogue(b.session()),
        Live::Properties => crate::tools::property_catalogue(b.session()),
        Live::Formats => vec!["svg".into(), "lottie".into(), "renb".into(), "ren".into()],
    }
}

/// Answer `completion/complete` for a prompt argument or a template variable.
///
/// An unknown reference comes back empty rather than as an error, since a
/// completion is asked for mid-typing.
pub fn complete(b: &mut dyn Backend, params: &Value) -> Value {
    let reference = params
        .get("ref")
        .and_then(|r| r.get("uri"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let prefix = params
        .get("argument")
        .and_then(|a| a.get("value"))
        .and_then(Value::as_str)
        .unwrap_or("");

    let live = if reference.starts_with("ref/prompt/") {
        let name = reference.trim_start_matches("ref/prompt/");
        PROMPTS
            .iter()
            .find(|p| p.name == name)
            .and_then(|p| p.args.first())
            .and_then(|a| a.values)
    } else if reference.starts_with("ref/resource/") {
        let uri = reference.trim_start_matches("ref/resource/");
        TEMPLATES
            .iter()
            .find(|t| uri == t.uri)
            .map(|t| t.live)
    } else {
        None
    };

    let mut values = live.map_or_else(Vec::new, |l| live_values(b, l));
    if !prefix.is_empty() {
        let lowered = prefix.to_ascii_lowercase();
        values.retain(|v| v.to_ascii_lowercase().starts_with(&lowered));
    }
    values.truncate(MAX_VALUES);
    json!({
        "completion": {
            "values": values,
            "total": values.len(),
            "hasMore": false,
        },
    })
}
