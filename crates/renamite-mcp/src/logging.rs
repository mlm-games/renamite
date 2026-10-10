//! `logging/setLevel` and the `notifications/message` it turns on.
//!
//! The **binary** owns the choice of logger and calls [`install`]; the library
//! never does. Nothing is sent before the client asks for it, because MCP has
//! no business pushing log lines at a client that never asked.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, Ordering};

use log::{Level, LevelFilter, Record};
use serde_json::{Value, json};

/// The severity from the last `logging/setLevel`, as an index into [`LEVELS`];
/// [`OFF`] means silent.
static LEVEL: AtomicU8 = AtomicU8::new(OFF);

const OFF: u8 = u8::MAX;

/// Records waiting to be drained into the next `notifications/message` batch.
static QUEUE: Mutex<Vec<Value>> = Mutex::new(Vec::new());

/// RFC 5424 severities, least to most severe, as the spec orders them.
const LEVELS: [&str; 8] = [
    "debug",
    "info",
    "notice",
    "warning",
    "error",
    "critical",
    "alert",
    "emergency",
];

/// The severity a `logging/setLevel` level names, and the `log` level it
/// filters at. `None` for anything the spec doesn't name.
fn parse(name: &str) -> Option<(u8, Level)> {
    let index = match name {
        "debug" => 0,
        "info" => 1,
        "notice" | "warning" => 2,
        "error" | "critical" | "alert" | "emergency" => 3,
        _ => return None,
    };
    let log_level = match index {
        0 => Level::Debug,
        1 => Level::Info,
        2 => Level::Warn,
        _ => Level::Error,
    };
    Some((index as u8, log_level))
}

/// Install the process-wide logger. Call from the binary only.
pub fn install() {
    let _ = log::set_logger(&LOGGER);
}

struct McpLogger;

impl log::Log for McpLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        let level = LEVEL.load(Ordering::Relaxed);
        level != OFF
            && LEVELS
                .get(level as usize)
                .and_then(|name| parse(name))
                .is_some_and(|(_, l)| metadata.level() <= l)
    }

    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record) {
            return;
        }
        let level = LEVEL.load(Ordering::Relaxed);
        let severity = LEVELS.get(level as usize).copied().unwrap_or("info");
        if let Ok(mut queue) = QUEUE.lock() {
            queue.push(json!({
                "level": severity,
                "logger": record.target(),
                "data": record.args().to_string(),
            }));
        }
    }

    fn flush(&self) {
        if let Ok(mut queue) = QUEUE.lock() {
            queue.clear();
        }
    }
}

static LOGGER: McpLogger = McpLogger;

/// Set the level. An unknown severity leaves the previous level in place.
pub fn set_level(name: &str) -> Result<(), ()> {
    let (index, _) = parse(name).ok_or(())?;
    LEVEL.store(index, Ordering::Relaxed);
    log::set_max_level(LevelFilter::Off);
    Ok(())
}

/// The records queued since the last call.
pub fn drain() -> Vec<Value> {
    let Ok(mut queue) = QUEUE.lock() else {
        return Vec::new();
    };
    std::mem::take(&mut *queue)
}
