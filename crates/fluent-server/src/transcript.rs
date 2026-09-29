//! The Markdown transcript `fluent demo record` writes.
//!
//! The MCP handler describes every `tools/list` and `tools/call` it answers
//! as a `trace` event with the [`TARGET`] target: the principal's
//! [`sub_hash`](crate::mcp::sub_hash), the tool names listed, or the tool
//! called, its argument *names*, its outcome and duration and its result,
//! [redacted](redact). Nothing records these events unless a
//! [`TranscriptLayer`] is installed, and [`logging::no_message_bodies`] keeps
//! them out of the logs whatever `RUST_LOG` enables.
//!
//! The layer appends one Markdown section per event to one file and flushes
//! it, so the transcript is complete whenever the server stops.
//!
//! [`logging::no_message_bodies`]: crate::logging::no_message_bodies

use std::collections::BTreeMap;
use std::fmt::{self, Write as _};
use std::fs::{File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{Map, Value};
use time::OffsetDateTime;
use tracing::field::{Field, Visit};
use tracing::{Event, Metadata, Subscriber};
use tracing_subscriber::filter::{FilterFn, filter_fn};
use tracing_subscriber::layer::{Context, Layer};

/// The target of the handler's transcript events.
pub const TARGET: &str = "fluent_transcript";

/// Keys whose string values are replaced by their length: the unsigned
/// transaction, address bytes and skill bodies.
const REPLACED: [&str; 3] = ["unsigned_tx_cbor_hex", "hex", "markdown"];

/// Bech32 prefixes of the addresses [`redact`] shortens.
const ADDRESS_PREFIXES: [&str; 4] = ["addr1", "addr_test1", "stake1", "stake_test1"];

/// Characters an address keeps at each end once shortened.
const ADDRESS_KEPT: usize = 12;

/// What a transcript records of a tool result: the same JSON with every
/// address shortened to its first and last characters and the values under
/// [`REPLACED`] keys (the transaction CBOR, address hex, skill bodies)
/// replaced by their length. Hashes, amounts and error codes are kept, so the
/// transaction can still be told apart from any other.
pub fn redact(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| {
                    let value = match value {
                        Value::String(text) if REPLACED.contains(&key.as_str()) => {
                            Value::String(format!("<redacted: {} characters>", text.len()))
                        }
                        other => redact(other),
                    };
                    (key.clone(), value)
                })
                .collect::<Map<_, _>>(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(redact).collect()),
        Value::String(text) => Value::String(shorten_address(text)),
        other => other.clone(),
    }
}

/// An address shortened to `addr_test1qrxc…wqkef`; any other text unchanged.
fn shorten_address(text: &str) -> String {
    let is_address = ADDRESS_PREFIXES.iter().any(|p| text.starts_with(p))
        && text.len() > 2 * ADDRESS_KEPT + 1
        && text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
    if !is_address {
        return text.to_string();
    }
    format!(
        "{}…{}",
        &text[..ADDRESS_KEPT],
        &text[text.len() - ADDRESS_KEPT..]
    )
}

/// Only the handler's transcript events.
pub fn only_transcript() -> FilterFn<fn(&Metadata<'_>) -> bool> {
    filter_fn(is_transcript as fn(&Metadata<'_>) -> bool)
}

fn is_transcript(metadata: &Metadata<'_>) -> bool {
    metadata.target() == TARGET
}

/// What the transcript header says about the recording.
#[derive(Debug, Clone)]
pub struct Recording {
    /// The configuration file the server loaded.
    pub config: PathBuf,
    /// `stdio` or `HTTP`.
    pub transport: &'static str,
}

/// Writes the transcript events to one Markdown file.
pub struct TranscriptLayer {
    path: PathBuf,
    state: Mutex<State>,
}

struct State {
    file: File,
    entries: usize,
}

impl TranscriptLayer {
    /// Creates `dir` when needed and a new transcript in it, named by the
    /// current UTC time, and writes its header. Never overwrites a file.
    pub fn create(dir: &Path, recording: &Recording) -> io::Result<TranscriptLayer> {
        std::fs::create_dir_all(dir)?;
        let started = OffsetDateTime::now_utc();
        let path = dir.join(format!("transcript-{}.md", compact(started)));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        file.write_all(header(recording, started).as_bytes())?;
        file.flush()?;
        Ok(TranscriptLayer {
            path,
            state: Mutex::new(State { file, entries: 0 }),
        })
    }

    /// The transcript file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl<S: Subscriber> Layer<S> for TranscriptLayer {
    fn on_event(&self, event: &Event<'_>, _context: Context<'_, S>) {
        if event.metadata().target() != TARGET {
            return;
        }
        let mut fields = Fields::default();
        event.record(&mut fields);
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        state.entries += 1;
        let entry = entry(state.entries, &fields.0, OffsetDateTime::now_utc());
        // A transcript that cannot be written must not stop the server.
        let _ = state
            .file
            .write_all(entry.as_bytes())
            .and_then(|()| state.file.flush());
    }
}

/// An event's fields, as text.
#[derive(Default)]
struct Fields(BTreeMap<&'static str, String>);

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name(), value.to_string());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.0.insert(field.name(), value.to_string());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.0.insert(field.name(), format!("{value:?}"));
    }
}

fn header(recording: &Recording, started: OffsetDateTime) -> String {
    format!(
        "# Tx3 Fluent transcript\n\n\
         - Recorded by: `fluent demo record` {version}\n\
         - Configuration: `{config}`\n\
         - Transport: {transport}\n\
         - Started: {started}\n\n\
         Each section is one `tools/list` or `tools/call` the server answered, in \
         order. Argument values are never recorded, only argument names. Results are \
         redacted: addresses are shortened to their ends, and the unsigned transaction \
         CBOR, address hex and skill bodies are replaced by their length. Pair this \
         transcript with the client's conversation export by time, tool and \
         transaction hash.\n",
        version = env!("CARGO_PKG_VERSION"),
        config = recording.config.display(),
        transport = recording.transport,
        started = rfc3339(started),
    )
}

/// One transcript section.
fn entry(number: usize, fields: &BTreeMap<&'static str, String>, at: OffsetDateTime) -> String {
    let field = |name: &str| fields.get(name).map(String::as_str).unwrap_or_default();
    let code_list = |names: &str| {
        let names: Vec<String> = names
            .split(',')
            .filter(|n| !n.is_empty())
            .map(|n| format!("`{n}`"))
            .collect();
        if names.is_empty() {
            "none".to_string()
        } else {
            names.join(", ")
        }
    };
    let principal = match field("sub_hash") {
        "" => "local (stdio or unauthenticated)".to_string(),
        hash => format!("`sub:{hash}`"),
    };

    let mut text = String::new();
    let kind = field("kind");
    if kind == "tools/call" {
        let _ = writeln!(text, "\n## {number}. `tools/call` · `{}`\n", field("tool"));
    } else {
        let _ = writeln!(text, "\n## {number}. `{kind}`\n");
    }
    let _ = writeln!(text, "- Time: {}", rfc3339(at));
    let _ = writeln!(text, "- Principal: {principal}");
    if kind == "tools/call" {
        let _ = writeln!(text, "- Arguments: {}", code_list(field("arguments")));
        let _ = writeln!(
            text,
            "- Outcome: `{}` in {} ms",
            field("outcome"),
            field("duration_ms")
        );
        let result = serde_json::from_str::<Value>(field("result"))
            .and_then(|value| serde_json::to_string_pretty(&value))
            .unwrap_or_else(|_| field("result").to_string());
        let _ = writeln!(text, "\n```json\n{result}\n```");
    } else {
        let tools = field("tools");
        let count = tools.split(',').filter(|n| !n.is_empty()).count();
        let _ = writeln!(text, "- Tools ({count}): {}", code_list(tools));
    }
    text
}

/// `2026-09-29T20:31:02Z`.
fn rfc3339(at: OffsetDateTime) -> String {
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        at.year(),
        u8::from(at.month()),
        at.day(),
        at.hour(),
        at.minute(),
        at.second()
    )
}

/// `20260929T203102Z`, for file names.
fn compact(at: OffsetDateTime) -> String {
    rfc3339(at).replace(['-', ':'], "")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn redaction_shortens_addresses_and_replaces_bulk_values() {
        let address = "addr_test1qrxchm0g4la6hqfd9wq6vuuldx7l20az52t7lvgpgujr8pvwmpzru5kuf4mpmvtaf0hlsjtz7t4r2h7tj9v3c02dhljq0wqkef";
        let redacted = redact(&json!({
            "tx_hash": "b2698db18245555a24e2e38a1a1c1a9623465aacf28b19ac93f9287093e58f73",
            "unsigned_tx_cbor_hex": "84a400",
            "summary": {"outputs": [{"address": address, "lovelace": 3_000_000}]},
            "markdown": "# skill",
            "network": "preprod",
        }));
        let shortened = format!("{}…{}", &address[..12], &address[address.len() - 12..]);
        assert_eq!(
            redacted,
            json!({
                "tx_hash": "b2698db18245555a24e2e38a1a1c1a9623465aacf28b19ac93f9287093e58f73",
                "unsigned_tx_cbor_hex": "<redacted: 6 characters>",
                "summary": {"outputs": [{"address": shortened, "lovelace": 3_000_000}]},
                "markdown": "<redacted: 7 characters>",
                "network": "preprod",
            })
        );
        assert!(!redacted.to_string().contains(address));
    }

    #[test]
    fn other_text_is_kept() {
        assert_eq!(shorten_address("addr_test1short"), "addr_test1short");
        assert_eq!(shorten_address("preprod"), "preprod");
        let sentence = "addr_test1 is the prefix of every preprod address in this long sentence";
        assert_eq!(shorten_address(sentence), sentence);
    }

    #[test]
    fn entries_list_names_and_never_values() {
        let at = OffsetDateTime::UNIX_EPOCH;
        let mut fields = BTreeMap::new();
        fields.insert("kind", "tools/call".to_string());
        fields.insert("tool", "transfer_preprod_transfer".to_string());
        fields.insert("sub_hash", "0123456789ab".to_string());
        fields.insert("arguments", "quantity,sender".to_string());
        fields.insert("outcome", "ok".to_string());
        fields.insert("duration_ms", "12".to_string());
        fields.insert("result", "{\"tx_hash\":\"ab\"}".to_string());
        let text = entry(2, &fields, at);
        assert_eq!(
            text,
            "\n## 2. `tools/call` · `transfer_preprod_transfer`\n\n\
             - Time: 1970-01-01T00:00:00Z\n\
             - Principal: `sub:0123456789ab`\n\
             - Arguments: `quantity`, `sender`\n\
             - Outcome: `ok` in 12 ms\n\n\
             ```json\n{\n  \"tx_hash\": \"ab\"\n}\n```\n"
        );

        let mut fields = BTreeMap::new();
        fields.insert("kind", "tools/list".to_string());
        fields.insert(
            "tools",
            "fluent_get_skill,fluent_inspect_address".to_string(),
        );
        assert_eq!(
            entry(1, &fields, at),
            "\n## 1. `tools/list`\n\n\
             - Time: 1970-01-01T00:00:00Z\n\
             - Principal: local (stdio or unauthenticated)\n\
             - Tools (2): `fluent_get_skill`, `fluent_inspect_address`\n"
        );
    }
}
