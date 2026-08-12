//! The `stream-json` codec (§10, spike #1).
//!
//! Built against **committed fixtures captured from real sessions** (`fixtures/stream/*.jsonl`),
//! not against a guess at the schema. See `docs/spikes.md` §1.
//!
//! # Parse defensively
//!
//! The message union is open — Claude Code adds event types between releases, and Phase 0 already
//! observed types that no documentation mentions (`system/thinking_tokens`, `rate_limit_event`).
//! An unrecognised line must never kill the stream, because that would kill a running task over a
//! cosmetic addition. Everything unknown becomes [`StreamEvent::Unknown`], is logged, and is
//! carried through to the raw log.
//!
//! # Three findings from the fixtures that the parser encodes
//!
//! 1. **`system/init` arrives once per _turn_**, not once per process.
//! 2. **`result.total_cost_usd` is cumulative for the session**, so per-turn cost is a delta.
//! 3. **Tool results and ping acknowledgements share `type: "user"`**; `isReplay: true` is the
//!    discriminator, and it is what makes a delivered ping observable.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One line of the `--output-format stream-json` stream.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    /// Session initialised. Emitted at the start of **every turn**.
    Init(Init),
    /// Coarse in-turn progress, e.g. `requesting`.
    Status { status: String },
    /// Context was compacted (spike #10). Nothing depends on reacting to it — the appended system
    /// prompt survives — but it belongs on the timeline.
    CompactBoundary {
        trigger: String,
        pre_tokens: u64,
        post_tokens: u64,
    },
    /// A hook fired inside the spawned session. These are the *operator's* hooks, inherited from
    /// their global settings.
    Hook { phase: HookPhase, name: String },
    /// An assistant message: thinking, text, and tool calls.
    Assistant(AssistantMessage),
    /// A user-role message. Either a tool result, or the echo of a turn reporch injected.
    User(UserMessage),
    /// Token-level delta, only with `--include-partial-messages`.
    PartialText { text: String },
    /// Turn boundary.
    Result(TurnResult),
    /// Rate-limit status.
    RateLimit {
        status: String,
        resets_at: Option<i64>,
    },
    /// Anything reporch does not model. Kept, never fatal.
    Unknown { kind: String, raw: Value },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookPhase {
    Started,
    Progress,
    Response,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Init {
    pub session_id: String,
    pub cwd: String,
    pub model: String,
    pub permission_mode: String,
    pub claude_code_version: String,
    /// The tools the model can actually see. **This is the §4.4 spawn assertion**: if
    /// `SendMessage` or `ListAgents` appear here, the deny rules did not take effect and the
    /// protocol is unenforced.
    pub tools: Vec<String>,
    pub skills: usize,
    pub slash_commands: usize,
    pub mcp_servers: usize,
    pub agents: usize,
}

impl Init {
    /// Which of the tools reporch denies are still present (§6.2, §4.4).
    ///
    /// `-p` mode silently ignores a settings payload that fails validation, so passing the flag
    /// proves nothing — only reading the tool list back does.
    pub fn surviving_denied_tools(&self) -> Vec<String> {
        crate::DENIED_TOOLS
            .iter()
            .filter(|t| self.tools.iter().any(|have| have == *t))
            .map(|t| t.to_string())
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct AssistantMessage {
    pub text: String,
    pub thinking: bool,
    /// Tool calls in this message, as `(name, one-line description)`.
    pub tool_uses: Vec<ToolUse>,
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolUse {
    pub id: String,
    pub name: String,
    /// A short human summary for the run screen's LAST column — the `description` field when the
    /// tool provides one, else the most identifying input.
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct UserMessage {
    /// `true` when this is the `--replay-user-messages` echo of a turn reporch injected: the
    /// delivery acknowledgement for a ping (§6.4).
    pub is_replay: bool,
    pub text: String,
    pub tool_result_count: usize,
}

/// A `result` message: the turn boundary.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TurnResult {
    pub subtype: String,
    pub is_error: bool,
    /// **Cumulative for the session**, not for this turn (§10 Deviation).
    pub total_cost_usd: f64,
    pub num_turns: u64,
    pub session_id: String,
    /// e.g. `completed`, `budget_exhausted`, `max_turns`.
    pub terminal_reason: Option<String>,
    pub stop_reason: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    /// Cumulative across all models used by this live process, from `modelUsage`.
    pub cumulative_usage: reporch_core::TokenUsage,
    /// The model provider, from `modelUsage.<model>.provider`. `firstParty` is what reporch needs
    /// (§4.5) — and this is a *runtime* confirmation, stronger than sniffing env vars.
    pub provider: Option<String>,
    pub errors: Vec<String>,
}

impl TurnResult {
    /// Whether this turn ended because the session hit `--max-budget-usd`.
    ///
    /// The process **exits** when this happens; it does not pause (spike #5).
    pub fn is_budget_exhausted(&self) -> bool {
        self.subtype == "error_max_budget_usd"
            || self.terminal_reason.as_deref() == Some("budget_exhausted")
    }
}

/// Parse one line. Returns `None` for blank lines only; malformed JSON becomes `Unknown`.
pub fn parse_line(line: &str) -> Option<StreamEvent> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let v: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => {
            return Some(StreamEvent::Unknown {
                kind: "unparseable".into(),
                raw: Value::String(line.chars().take(500).collect()),
            })
        }
    };
    Some(parse_value(v))
}

fn s(v: &Value, k: &str) -> String {
    v.get(k)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}
fn u(v: &Value, k: &str) -> u64 {
    v.get(k).and_then(Value::as_u64).unwrap_or(0)
}
fn len(v: &Value, k: &str) -> usize {
    v.get(k).and_then(Value::as_array).map_or(0, Vec::len)
}

pub fn parse_value(v: Value) -> StreamEvent {
    match v.get("type").and_then(Value::as_str).unwrap_or("") {
        "system" => parse_system(v),
        "assistant" => parse_assistant(&v),
        "user" => parse_user(&v),
        "result" => parse_result(&v),
        "stream_event" => parse_stream_event(v),
        "rate_limit_event" => {
            let info = v.get("rate_limit_info").cloned().unwrap_or(Value::Null);
            StreamEvent::RateLimit {
                status: s(&info, "status"),
                resets_at: info.get("resetsAt").and_then(Value::as_i64),
            }
        }
        other => StreamEvent::Unknown {
            kind: other.to_string(),
            raw: v,
        },
    }
}

fn parse_system(v: Value) -> StreamEvent {
    match v.get("subtype").and_then(Value::as_str).unwrap_or("") {
        "init" => StreamEvent::Init(Init {
            session_id: s(&v, "session_id"),
            cwd: s(&v, "cwd"),
            model: s(&v, "model"),
            permission_mode: s(&v, "permissionMode"),
            claude_code_version: s(&v, "claude_code_version"),
            tools: v
                .get("tools")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|t| t.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            skills: len(&v, "skills"),
            slash_commands: len(&v, "slash_commands"),
            mcp_servers: len(&v, "mcp_servers"),
            agents: len(&v, "agents"),
        }),
        "status" => StreamEvent::Status {
            status: s(&v, "status"),
        },
        "compact_boundary" => {
            let m = v.get("compact_metadata").cloned().unwrap_or(Value::Null);
            StreamEvent::CompactBoundary {
                trigger: s(&m, "trigger"),
                pre_tokens: u(&m, "pre_tokens"),
                post_tokens: u(&m, "post_tokens"),
            }
        }
        "hook_started" => StreamEvent::Hook {
            phase: HookPhase::Started,
            name: s(&v, "hook_name"),
        },
        "hook_progress" => StreamEvent::Hook {
            phase: HookPhase::Progress,
            name: s(&v, "hook_name"),
        },
        "hook_response" => StreamEvent::Hook {
            phase: HookPhase::Response,
            name: s(&v, "hook_name"),
        },
        other => StreamEvent::Unknown {
            kind: format!("system/{other}"),
            raw: v,
        },
    }
}

fn parse_assistant(v: &Value) -> StreamEvent {
    let msg = v.get("message").cloned().unwrap_or(Value::Null);
    let mut out = AssistantMessage {
        model: msg.get("model").and_then(Value::as_str).map(str::to_string),
        ..Default::default()
    };
    for block in msg
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match block.get("type").and_then(Value::as_str).unwrap_or("") {
            "text" => {
                let t = s(block, "text");
                if !out.text.is_empty() && !t.is_empty() {
                    out.text.push('\n');
                }
                out.text.push_str(&t);
            }
            "thinking" => out.thinking = true,
            "tool_use" => {
                let name = s(block, "name");
                let input = block.get("input").cloned().unwrap_or(Value::Null);
                out.tool_uses.push(ToolUse {
                    id: s(block, "id"),
                    summary: summarise_tool_use(&name, &input),
                    name,
                });
            }
            _ => {}
        }
    }
    StreamEvent::Assistant(out)
}

/// A one-line description of a tool call for the run screen's LAST column (§5.8).
///
/// Prefers the tool's own `description`, then the most identifying input field, so the column
/// reads like `editing app/widgets.py` rather than a JSON blob.
fn summarise_tool_use(name: &str, input: &Value) -> String {
    if let Some(d) = input.get("description").and_then(Value::as_str) {
        if !d.is_empty() {
            return d.to_string();
        }
    }
    let field = match name {
        "Bash" => "command",
        "Read" | "Edit" | "Write" | "NotebookEdit" => "file_path",
        "Glob" | "Grep" => "pattern",
        "WebFetch" => "url",
        "Task" => "prompt",
        _ => "",
    };
    let raw = input
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| name.to_string());
    let verb = match name {
        "Read" => "reading ",
        "Edit" | "Write" => "editing ",
        "Bash" => "$ ",
        "Grep" | "Glob" => "searching ",
        _ => "",
    };
    let one_line = raw.replace('\n', " ");
    format!("{verb}{}", truncate_chars(&one_line, 60))
}

/// Truncate on a character boundary. Repo names and tool inputs contain non-ASCII, and slicing
/// bytes would panic.
pub fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn parse_user(v: &Value) -> StreamEvent {
    let msg = v.get("message").cloned().unwrap_or(Value::Null);
    let mut out = UserMessage {
        is_replay: v.get("isReplay").and_then(Value::as_bool).unwrap_or(false),
        ..Default::default()
    };
    match msg.get("content") {
        Some(Value::String(t)) => out.text = t.clone(),
        Some(Value::Array(blocks)) => {
            for b in blocks {
                match b.get("type").and_then(Value::as_str).unwrap_or("") {
                    "text" => out.text.push_str(&s(b, "text")),
                    "tool_result" => out.tool_result_count += 1,
                    _ => {}
                }
            }
        }
        _ => {}
    }
    StreamEvent::User(out)
}

fn parse_result(v: &Value) -> StreamEvent {
    let usage = v.get("usage").cloned().unwrap_or(Value::Null);
    let model_usage = v.get("modelUsage").and_then(Value::as_object);
    let provider = model_usage
        .and_then(|m| m.values().next())
        .and_then(|mu| mu.get("provider"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let cumulative_usage = model_usage.map_or_else(reporch_core::TokenUsage::default, |models| {
        models
            .values()
            .fold(reporch_core::TokenUsage::default(), |sum, model| {
                sum.saturating_add(reporch_core::TokenUsage {
                    input: u(model, "inputTokens"),
                    output: u(model, "outputTokens"),
                    cache_read: u(model, "cacheReadInputTokens"),
                    cache_creation: u(model, "cacheCreationInputTokens"),
                })
            })
    });
    StreamEvent::Result(TurnResult {
        subtype: s(v, "subtype"),
        is_error: v.get("is_error").and_then(Value::as_bool).unwrap_or(false),
        total_cost_usd: v
            .get("total_cost_usd")
            .and_then(Value::as_f64)
            .unwrap_or(0.0),
        num_turns: u(v, "num_turns"),
        session_id: s(v, "session_id"),
        terminal_reason: v
            .get("terminal_reason")
            .and_then(Value::as_str)
            .map(str::to_string),
        stop_reason: v
            .get("stop_reason")
            .and_then(Value::as_str)
            .map(str::to_string),
        input_tokens: u(&usage, "input_tokens"),
        output_tokens: u(&usage, "output_tokens"),
        cache_read_tokens: u(&usage, "cache_read_input_tokens"),
        cache_creation_tokens: u(&usage, "cache_creation_input_tokens"),
        cumulative_usage,
        provider,
        errors: v
            .get("errors")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|e| e.as_str().unwrap_or("").to_string())
                    .collect()
            })
            .unwrap_or_default(),
    })
}

fn parse_stream_event(v: Value) -> StreamEvent {
    let ev = v.get("event").cloned().unwrap_or(Value::Null);
    if ev.get("type").and_then(Value::as_str) == Some("content_block_delta") {
        if let Some(t) = ev
            .get("delta")
            .and_then(|d| d.get("text"))
            .and_then(Value::as_str)
        {
            return StreamEvent::PartialText {
                text: t.to_string(),
            };
        }
    }
    StreamEvent::Unknown {
        kind: "stream_event".into(),
        raw: v,
    }
}

/// The outcome of a spawn assertion (§4.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnAssertion {
    pub ok: bool,
    pub surviving_tools: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_is_parsed_with_the_tool_list() {
        let ev = parse_line(
            r#"{"type":"system","subtype":"init","cwd":"/x","session_id":"abc",
                "tools":["Bash","Read"],"model":"claude-sonnet-5",
                "permissionMode":"bypassPermissions","claude_code_version":"2.1.226",
                "skills":[1,2],"slash_commands":[1],"mcp_servers":[],"agents":[1,2,3]}"#,
        )
        .unwrap();
        let StreamEvent::Init(i) = ev else {
            panic!("expected Init, got {ev:?}")
        };
        assert_eq!(i.session_id, "abc");
        assert_eq!(i.model, "claude-sonnet-5");
        assert_eq!(i.permission_mode, "bypassPermissions");
        assert_eq!(i.tools, vec!["Bash", "Read"]);
        assert_eq!(i.skills, 2);
        assert_eq!(i.agents, 3);
        assert_eq!(i.mcp_servers, 0);
    }

    /// The §4.4 spawn assertion, in both directions. Spike #8 measured exactly this: 60 tools
    /// with `SendMessage` present, 58 without.
    #[test]
    fn the_spawn_assertion_detects_surviving_tools() {
        let denied = Init {
            tools: vec!["Bash".into(), "Read".into()],
            ..Default::default()
        };
        assert!(denied.surviving_denied_tools().is_empty());

        let leaked = Init {
            tools: vec!["Bash".into(), "SendMessage".into(), "ListAgents".into()],
            ..Default::default()
        };
        assert_eq!(
            leaked.surviving_denied_tools(),
            vec!["SendMessage".to_string(), "ListAgents".to_string()]
        );
    }

    #[test]
    fn a_replayed_user_message_is_flagged_as_the_delivery_ack() {
        let ev = parse_line(
            r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"go"}]},
                "isReplay":true}"#,
        )
        .unwrap();
        let StreamEvent::User(u) = ev else {
            panic!("expected User")
        };
        assert!(u.is_replay, "isReplay is the ping delivery acknowledgement");
        assert_eq!(u.text, "go");
        assert_eq!(u.tool_result_count, 0);
    }

    /// Tool results share `type: "user"` with the replay. Confusing them would make reporch think
    /// every tool result was a ping landing.
    #[test]
    fn a_tool_result_is_not_mistaken_for_a_replay() {
        let ev = parse_line(
            r#"{"type":"user","message":{"role":"user","content":[
                {"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}"#,
        )
        .unwrap();
        let StreamEvent::User(u) = ev else {
            panic!("expected User")
        };
        assert!(!u.is_replay);
        assert_eq!(u.tool_result_count, 1);
    }

    #[test]
    fn result_carries_cost_tokens_and_provider() {
        let ev = parse_line(
            r#"{"type":"result","subtype":"success","is_error":false,"num_turns":4,
                "total_cost_usd":0.3846402,"session_id":"s1","terminal_reason":"completed",
                "stop_reason":"end_turn",
                "usage":{"input_tokens":8,"output_tokens":552,
                         "cache_read_input_tokens":161114,"cache_creation_input_tokens":54667},
                "modelUsage":{"claude-sonnet-5":{"provider":"firstParty"}}}"#,
        )
        .unwrap();
        let StreamEvent::Result(r) = ev else {
            panic!("expected Result")
        };
        assert_eq!(r.subtype, "success");
        assert!((r.total_cost_usd - 0.3846402).abs() < 1e-9);
        assert_eq!(r.cache_creation_tokens, 54667);
        assert_eq!(r.provider.as_deref(), Some("firstParty"));
        assert!(!r.is_budget_exhausted());
    }

    #[test]
    fn result_carries_cumulative_usage_across_models() {
        let ev = parse_line(
            r#"{"type":"result","subtype":"success","total_cost_usd":0.4,
                "usage":{"input_tokens":2,"output_tokens":7,
                         "cache_read_input_tokens":54667,"cache_creation_input_tokens":240},
                "modelUsage":{
                  "claude-sonnet-5":{"inputTokens":10,"outputTokens":559,
                    "cacheReadInputTokens":215781,"cacheCreationInputTokens":54907,
                    "provider":"firstParty"},
                  "claude-haiku-4-5":{"inputTokens":3,"outputTokens":11,
                    "cacheReadInputTokens":100,"cacheCreationInputTokens":20,
                    "provider":"firstParty"}}}"#,
        )
        .unwrap();
        let StreamEvent::Result(r) = ev else {
            panic!("expected Result")
        };
        assert_eq!(
            r.cumulative_usage,
            reporch_core::TokenUsage {
                input: 13,
                output: 570,
                cache_read: 215_881,
                cache_creation: 54_927,
            },
            "modelUsage is cumulative and model switches must be summed"
        );
    }

    /// Verbatim from `fixtures/stream/spike05-budget.jsonl`.
    #[test]
    fn budget_exhaustion_is_recognised() {
        let ev = parse_line(
            r#"{"type":"result","subtype":"error_max_budget_usd","is_error":true,
                "terminal_reason":"budget_exhausted","stop_reason":"tool_use",
                "total_cost_usd":0.1862916,"session_id":"s"}"#,
        )
        .unwrap();
        let StreamEvent::Result(r) = ev else {
            panic!("expected Result")
        };
        assert!(r.is_budget_exhausted());
        assert!(r.is_error);
    }

    #[test]
    fn compact_boundary_is_parsed() {
        let ev = parse_line(
            r#"{"type":"system","subtype":"compact_boundary",
                "compact_metadata":{"trigger":"manual","pre_tokens":35833,"post_tokens":7955}}"#,
        )
        .unwrap();
        assert_eq!(
            ev,
            StreamEvent::CompactBoundary {
                trigger: "manual".into(),
                pre_tokens: 35833,
                post_tokens: 7955
            }
        );
    }

    #[test]
    fn tool_uses_are_summarised_for_the_run_screen() {
        let ev = parse_line(
            r#"{"type":"assistant","message":{"model":"m","content":[
                {"type":"tool_use","id":"t1","name":"Bash",
                 "input":{"command":"ls -la","description":"List files"}}]}}"#,
        )
        .unwrap();
        let StreamEvent::Assistant(a) = ev else {
            panic!()
        };
        assert_eq!(a.tool_uses.len(), 1);
        assert_eq!(a.tool_uses[0].name, "Bash");
        assert_eq!(a.tool_uses[0].summary, "List files");
    }

    #[test]
    fn a_tool_use_without_a_description_falls_back_to_its_input() {
        let ev = parse_line(
            r#"{"type":"assistant","message":{"content":[
                {"type":"tool_use","id":"t","name":"Edit",
                 "input":{"file_path":"app/widgets.py"}}]}}"#,
        )
        .unwrap();
        let StreamEvent::Assistant(a) = ev else {
            panic!()
        };
        assert_eq!(a.tool_uses[0].summary, "editing app/widgets.py");
    }

    #[test]
    fn thinking_blocks_are_noted_but_not_shown() {
        let ev = parse_line(
            r#"{"type":"assistant","message":{"content":[
                {"type":"thinking","thinking":"hmm"},{"type":"text","text":"done"}]}}"#,
        )
        .unwrap();
        let StreamEvent::Assistant(a) = ev else {
            panic!()
        };
        assert!(a.thinking);
        assert_eq!(a.text, "done");
    }

    // ---- defensive parsing -------------------------------------------------

    /// The union is open. An unknown type must never kill a running task.
    #[test]
    fn unknown_event_types_survive() {
        let ev = parse_line(r#"{"type":"brand_new_thing","x":1}"#).unwrap();
        assert!(matches!(ev, StreamEvent::Unknown { ref kind, .. } if kind == "brand_new_thing"));

        let ev = parse_line(r#"{"type":"system","subtype":"thinking_tokens","n":5}"#).unwrap();
        assert!(
            matches!(ev, StreamEvent::Unknown { ref kind, .. } if kind == "system/thinking_tokens")
        );
    }

    #[test]
    fn malformed_json_becomes_unknown_not_an_error() {
        let ev = parse_line("{not json at all").unwrap();
        assert!(matches!(ev, StreamEvent::Unknown { ref kind, .. } if kind == "unparseable"));
    }

    #[test]
    fn blank_lines_are_skipped() {
        assert_eq!(parse_line(""), None);
        assert_eq!(parse_line("   \n"), None);
    }

    #[test]
    fn missing_fields_default_rather_than_panicking() {
        let ev = parse_line(r#"{"type":"result"}"#).unwrap();
        let StreamEvent::Result(r) = ev else { panic!() };
        assert_eq!(r.total_cost_usd, 0.0);
        assert_eq!(r.subtype, "");

        let ev = parse_line(r#"{"type":"system","subtype":"init"}"#).unwrap();
        let StreamEvent::Init(i) = ev else { panic!() };
        assert!(i.tools.is_empty());
    }

    #[test]
    fn truncation_respects_character_boundaries() {
        assert_eq!(truncate_chars("hello", 10), "hello");
        assert_eq!(truncate_chars("hello world", 8), "hello w…");
        // Multi-byte: slicing bytes here would panic.
        let s = "日本語のテキストです";
        let t = truncate_chars(s, 5);
        assert_eq!(t.chars().count(), 5);
    }
}
