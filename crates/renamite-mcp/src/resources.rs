//! MCP resources: the fixed documents, the templates an agent can address parts
//! of a project by, and reading either.
//!
//! The templates matter for a real project. `renamite://document/json` returns
//! the whole model, which on a real rig is thousands of lines an agent pays for
//! again on every change; the templates hand out one node, one clip or one
//! machine instead, and they are what `completion/complete` suggests ids from.

use serde_json::{Value, json};

use crate::backend::Backend;
use crate::prompts::Live;

/// The open project's summary: compositions, counts, and the playhead.
pub const DOC_URI: &str = "renamite://document";
/// The whole project model as JSON.
pub const DOC_JSON_URI: &str = "renamite://document/json";

/// Why a resource could not be read. The distinction matters: the server turns
/// it into `-32002` (no such resource) or `-32602` (bad URI), and guessing from
/// the message text would not hold.
#[derive(Debug)]
pub enum ReadError {
    NotFound(String),
    Invalid(String),
    Backend(String),
}

/// One addressable template: the URI with `{var}`, and what the variable is.
pub struct Template {
    pub uri: &'static str,
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub mime: &'static str,
    pub live: Live,
}

/// Every template, in `resources/templates/list` order.
pub static TEMPLATES: &[Template] = &[
    Template {
        uri: "renamite://node/{id}",
        name: "node",
        title: "One node",
        description: "One node: id, name, kind, bounds, transform, opacity, children. A large group is sliced with depth and childLimit, which report childCount when truncated.",
        mime: "application/json",
        live: Live::Nodes,
    },
    Template {
        uri: "renamite://clip/{id}",
        name: "clip",
        title: "One clip",
        description: "One clip: its name, tracks and keyframes.",
        mime: "application/json",
        live: Live::Clips,
    },
    Template {
        uri: "renamite://machine/{id}",
        name: "machine",
        title: "One machine",
        description: "One machine: its states, transitions and inputs.",
        mime: "application/json",
        live: Live::Machines,
    },
];

/// The fixed resources, in `resources/list` order.
pub fn list() -> Value {
    json!({
        "resources": [
            {"uri": DOC_URI, "name": "document", "title": "Open project (summary)", "description": "Compositions, node/clip/machine counts, and the playhead", "mimeType": "application/json"},
            {"uri": DOC_JSON_URI, "name": "document-json", "title": "Open project (full model)", "description": "The complete project as JSON. The fidelity path, not the way to look around", "mimeType": "application/json"},
        ],
    })
}

pub fn templates() -> Value {
    json!({
        "resourceTemplates": TEMPLATES
            .iter()
            .map(|t| json!({
                "uriTemplate": t.uri,
                "name": t.name,
                "title": t.title,
                "description": t.description,
                "mimeType": t.mime,
            }))
            .collect::<Vec<_>>(),
    })
}

/// Which fixed resource a URI names, if any.
fn fixed(uri: &str) -> Option<&'static str> {
    if uri == DOC_URI {
        Some("document")
    } else if uri == DOC_JSON_URI {
        Some("document/json")
    } else {
        None
    }
}

pub fn read(b: &mut dyn Backend, uri: &str) -> Result<Value, ReadError> {
    if let Some(name) = fixed(uri) {
        let document = crate::tools::document_summary(b.session());
        let json = crate::tools::document_json(b.session());
        return if name == "document" {
            Ok(document)
        } else {
            Ok(json)
        };
    }
    let (template, var) = match uri.split_once('/') {
        Some((prefix, var)) => (prefix, var),
        None => return Err(ReadError::NotFound(uri.to_string())),
    };
    let scheme = format!("{template}/{var}");
    let _ = scheme;
    let Some(t) = TEMPLATES.iter().find(|t| uri.starts_with(&format!("{}", t.uri.rsplit('/').next().unwrap_or("")))) else {
        return Err(ReadError::NotFound(uri.to_string()));
    };
    let _ = t;
    read_template(b, uri)
}

/// Read one template URI.
fn read_template(b: &mut dyn Backend, uri: &str) -> Result<Value, ReadError> {
    let family = uri.strip_prefix("renamite://").unwrap_or(uri);
    let (family, var) = family
        .split_once('/')
        .ok_or_else(|| ReadError::Invalid(format!("not a resource template: {uri}")))?;
    let session = b.session();
    match family {
        "node" => crate::tools::node_resource(session, var).ok_or_else(|| {
            ReadError::Invalid(format!("no node with id or name `{var}`"))
        }),
        "clip" => crate::tools::clip_resource(session, var).ok_or_else(|| {
            ReadError::Invalid(format!("no clip with id or name `{var}`"))
        }),
        "machine" => crate::tools::machine_resource(session, var).ok_or_else(|| {
            ReadError::Invalid(format!("no machine with id or name `{var}`"))
        }),
        _ => Err(ReadError::NotFound(uri.to_string())),
    }
}
