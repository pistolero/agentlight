//! On-demand display excerpts; never include reasoning blocks or tool payloads.
use crate::{codex_child_threads, codex_db, codex_thread, env_path, home, open_db, Claude, State};
use serde_json::{json, Value};
use std::{
    fs,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

const FRAME: usize = 4096;
const MAX_ENTRIES: usize = 30;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Role {
    User,
    Assistant,
    Tool,
}
impl Role {
    fn code(self) -> &'static str {
        match self {
            Role::User => "u",
            Role::Assistant => "a",
            Role::Tool => "t",
        }
    }
    fn legacy(self) -> &'static str {
        match self {
            Role::User => "user: ",
            Role::Assistant => "assistant: ",
            Role::Tool => "Tool: ",
        }
    }
}
pub type Entry = (Role, String);

/// Reduce to the display's ASCII font: transliterate common typography,
/// drop emoji and symbols, and collapse runs of blank lines.
fn clean(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\n' | ' '..='~' => out.push(c),
            '\t' | '\u{a0}' | '\u{2009}' | '\u{202f}' | '\u{2002}' | '\u{2003}' => out.push(' '),
            '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{2032}' | '\u{b4}' => out.push('\''),
            '\u{201c}' | '\u{201d}' | '\u{201e}' | '\u{2033}' | '\u{ab}' | '\u{bb}' => {
                out.push('"')
            }
            '\u{2010}'..='\u{2015}' | '\u{2212}' => out.push('-'),
            '\u{2026}' => out.push_str("..."),
            '\u{2192}' | '\u{27f6}' => out.push_str("->"),
            '\u{2190}' | '\u{27f5}' => out.push_str("<-"),
            '\u{21d2}' => out.push_str("=>"),
            '\u{2194}' => out.push_str("<->"),
            '\u{2022}' | '\u{b7}' | '\u{25cf}' | '\u{25e6}' | '\u{2023}' => out.push('*'),
            '\u{2713}' | '\u{2714}' | '\u{2705}' => out.push_str("[x]"),
            '\u{2717}' | '\u{2718}' | '\u{274c}' => out.push_str("[ ]"),
            '\u{d7}' => out.push('x'),
            '\u{2264}' => out.push_str("<="),
            '\u{2265}' => out.push_str(">="),
            '\u{2260}' => out.push_str("!="),
            '\u{2248}' => out.push('~'),
            c if c.is_alphanumeric() => out.push('?'),
            _ => (),
        }
    }
    let mut collapsed = String::with_capacity(out.len());
    let mut newlines = 0;
    for c in out.trim().chars() {
        if c == '\n' {
            newlines += 1;
            if newlines > 2 {
                continue;
            }
        } else {
            newlines = 0;
        }
        collapsed.push(c);
    }
    collapsed
}
fn content(value: &Value, output: &mut Vec<Entry>, role: Role) {
    if let Some(text) = value.as_str() {
        let text = clean(text);
        if !text.is_empty() {
            output.push((role, text));
        }
    }
    if let Some(parts) = value.as_array() {
        for part in parts {
            match part["type"].as_str().unwrap_or("") {
                "text" | "input_text" | "output_text" => content(&part["text"], output, role),
                "tool_use" | "tool" => {
                    if let Some(name) = part["name"].as_str() {
                        output.push((Role::Tool, clean(name)));
                    }
                }
                _ => (),
            }
        }
    }
}
fn role_of(name: &str) -> Role {
    if name == "user" {
        Role::User
    } else {
        Role::Assistant
    }
}
fn transcript(path: &Path, include_sidechain: bool) -> Option<Vec<Entry>> {
    let mut f = fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let start = len.saturating_sub(256 * 1024);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    f.take(256 * 1024).read_to_end(&mut bytes).ok()?;
    let mut output = vec![];
    for (i, line) in bytes.split(|b| *b == b'\n').enumerate() {
        if start > 0 && i == 0 {
            continue;
        }
        let Ok(v) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        if !include_sidechain && v["isSidechain"] == true {
            continue;
        }
        match v["type"].as_str().unwrap_or("") {
            kind @ ("user" | "assistant") => {
                content(&v["message"]["content"], &mut output, role_of(kind))
            }
            "response_item" if v["payload"]["type"] == "message" => content(
                &v["payload"]["content"],
                &mut output,
                role_of(v["payload"]["role"].as_str().unwrap_or("assistant")),
            ),
            _ => (),
        }
    }
    Some(output)
}
fn opencode(id: &str) -> Option<Vec<Entry>> {
    if !id.starts_with("ses_") {
        return None;
    }
    let db = open_db(
        &env_path("XDG_DATA_HOME", home().join(".local/share")).join("opencode/opencode.db"),
    )?;
    let mut stmt=db.prepare("SELECT type, substr(data,1,262144) FROM session_message WHERE session_id=?1 AND type IN ('user','assistant') ORDER BY seq DESC LIMIT 20").ok()?;
    let rows = stmt
        .query_map([id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .ok()?;
    let mut rows: Vec<_> = rows.flatten().collect();
    rows.reverse();
    let mut output = vec![];
    for (role, data) in rows {
        let Ok(v) = serde_json::from_str::<Value>(&data) else {
            continue;
        };
        if role == "user" {
            content(&v["text"], &mut output, Role::User);
        } else {
            content(&v["content"], &mut output, Role::Assistant);
        }
    }
    Some(output)
}
fn safe(id: &str) -> bool {
    !id.is_empty() && !id.contains(['/', '\\']) && id != "." && id != ".."
}
/// The Claude session id behind a `claude:<pid>` agent id.
fn claude_session(pid: u32) -> Option<(PathBuf, String)> {
    let root = env_path("CLAUDE_CONFIG_DIR", home().join(".claude"));
    let record = fs::read(root.join("sessions").join(format!("{pid}.json")))
        .ok()
        .and_then(|b| serde_json::from_slice::<Claude>(&b).ok())?;
    safe(&record.session_id).then_some((root, record.session_id))
}
pub(crate) fn claude_file(root: &Path, relative: &Path) -> Option<PathBuf> {
    fs::read_dir(root.join("projects"))
        .ok()?
        .flatten()
        .map(|e| e.path().join(relative))
        .find(|p| p.is_file())
}
fn codex_child_entries(db: &rusqlite::Connection, parent: &str, child: &str) -> Option<Vec<Entry>> {
    if !safe(child) {
        return None;
    }
    let thread = codex_child_threads(db, parent)
        .into_iter()
        .find(|c| c.thread.id == child)?
        .thread;
    transcript(&thread.rollout_path, false)
}
fn codex(pid: u32, child: Option<&str>) -> Option<Vec<Entry>> {
    let cwd = fs::read_link(format!("/proc/{pid}/cwd")).ok()?;
    let thread = codex_thread(cwd.to_string_lossy().as_ref())?;
    match child {
        None => transcript(&thread.rollout_path, false),
        Some(child) => {
            let root = env_path("CODEX_HOME", home().join(".codex"));
            codex_child_entries(&codex_db(&root)?, &thread.id, child)
        }
    }
}
/// Recent conversation entries for an agent, or for one of its subagents.
pub fn entries(id: &str, child: Option<&str>) -> Option<Vec<Entry>> {
    if let Some(pid) = id
        .strip_prefix("claude:")
        .and_then(|p| p.parse::<u32>().ok())
    {
        let (root, session) = claude_session(pid)?;
        match child {
            None => claude_file(&root, Path::new(&format!("{session}.jsonl")))
                .and_then(|p| transcript(&p, false)),
            Some(child) => {
                // Child ids are subagent metadata stems: agent-<id>.meta
                let stem = child.strip_suffix(".meta").unwrap_or(child);
                if !safe(stem) {
                    return None;
                }
                let relative = Path::new(&session)
                    .join("subagents")
                    .join(format!("{stem}.jsonl"));
                claude_file(&root, &relative).and_then(|p| transcript(&p, true))
            }
        }
    } else if let Some(session) = id.strip_prefix("opencode:") {
        opencode(child.unwrap_or(session))
    } else if let Some(pid) = id
        .strip_prefix("codex:")
        .and_then(|p| p.parse::<u32>().ok())
    {
        codex(pid, child)
    } else {
        None
    }
}
fn demo(child: bool) -> Vec<Entry> {
    if child {
        vec![
            (
                Role::User,
                "Inspect the display driver and report how frames are swapped.".into(),
            ),
            (Role::Tool, "Read".into()),
            (
                Role::Assistant,
                "Frames are double-buffered; the panel switches on VSYNC.".into(),
            ),
        ]
    } else {
        vec![
            (
                Role::User,
                "Make the status cards easier to read from across the room.".into(),
            ),
            (
                Role::Assistant,
                "I'll tint each card by status and add a bright status pill.".into(),
            ),
            (Role::Tool, "Edit".into()),
            (
                Role::Assistant,
                "Done - cards now use soft tints with an accent edge.".into(),
            ),
        ]
    }
}
fn encoded(v: &Value) -> usize {
    serde_json::to_vec(v).map(|b| b.len()).unwrap_or(usize::MAX)
}
/// Keep the newest entries that fit `budget` encoded bytes; trim the start of
/// the oldest kept entry instead of dropping it when it alone overflows.
fn bounded(entries: Vec<Entry>, budget: usize) -> Vec<Entry> {
    let mut chosen = vec![];
    let mut used = 0;
    for (role, text) in entries.into_iter().rev().take(MAX_ENTRIES) {
        let size = encoded(&json!([role.code(), text])) + 1;
        if used + size <= budget {
            used += size;
            chosen.push((role, text));
            continue;
        }
        // clean() guarantees ASCII, so byte slicing is safe.
        let mut start = 0;
        while start < text.len() {
            start = (start + 64).min(text.len());
            let candidate = format!("...{}", &text[start..]);
            if used + encoded(&json!([role.code(), candidate])) + 1 <= budget {
                if text.len() - start > 16 {
                    chosen.push((role, candidate));
                }
                break;
            }
        }
        break;
    }
    chosen.reverse();
    chosen
}
fn legacy_text(entries: &[Entry]) -> String {
    entries
        .iter()
        .map(|(r, t)| format!("{}{t}", r.legacy()))
        .collect::<Vec<_>>()
        .join("\n")
}
/// Build the reply frame. `logv` 2 sends typed entries; older displays get text.
pub fn reply(id: &str, child: Option<&str>, entries: Option<Vec<Entry>>, logv: u64) -> Value {
    let status = match &entries {
        None => "unavailable",
        Some(e) if e.is_empty() => "empty",
        Some(_) => "ok",
    };
    let entries = entries.unwrap_or_default();
    if logv >= 2 {
        let mut frame = json!({"v":1,"t":"tail","id":id,"state":status,"entries":[]});
        if let Some(child) = child {
            frame["child"] = json!(child);
        }
        let budget = FRAME.saturating_sub(encoded(&frame) + 8);
        let rows: Vec<Value> = bounded(entries, budget)
            .into_iter()
            .map(|(r, t)| json!([r.code(), t]))
            .collect();
        frame["entries"] = Value::Array(rows);
        return frame;
    }
    let text = match status {
        "unavailable" => "Log unavailable for this session/source.".into(),
        "empty" => "No recent readable messages in the log tail.".into(),
        _ => legacy_text(&bounded(entries, 2600)),
    };
    let mut frame = json!({"v":1,"t":"tail","id":id,"text":text});
    while encoded(&frame) > FRAME {
        let text = frame["text"].as_str().unwrap();
        let start = 100.min(text.len());
        frame["text"] = Value::String(format!("[...] {}", &text[start..]));
    }
    frame
}
/// Answer a display's `tail` request for an agent or subagent in `state`.
pub fn answer(request: &Value, state: &State, demo_mode: bool, logv: u64) -> Option<Value> {
    if request["v"] != 1 || request["t"] != "tail" {
        return None;
    }
    let id = request["id"].as_str()?;
    let agent = state.agents.iter().find(|a| a.id == id)?;
    let child = match request["child"].as_str() {
        Some(c) => Some(agent.children.iter().find(|x| x.id == c)?.id.as_str()),
        None => None,
    };
    let rows = if demo_mode {
        Some(demo(child.is_some()))
    } else {
        entries(id, child)
    };
    Some(reply(id, child, rows, logv))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn excludes_reasoning_and_tool_payloads() {
        let v = json!([{"type":"reasoning","text":"secret reasoning"},{"type":"text","text":"Hello"},{"type":"tool","name":"shell","state":{"input":"secret argument"}}]);
        let mut lines = vec![];
        content(&v, &mut lines, Role::Assistant);
        assert_eq!(
            lines,
            vec![
                (Role::Assistant, "Hello".into()),
                (Role::Tool, "shell".into())
            ]
        );
    }
    #[test]
    fn transliterates_typography_and_drops_emoji() {
        assert_eq!(
            clean("It\u{2019}s \u{201c}done\u{201d} \u{2014} ok\u{2026} \u{2192} next \u{1f680}"),
            "It's \"done\" - ok... -> next"
        );
        assert_eq!(clean("caf\u{e9}"), "caf?");
        assert_eq!(clean("a\n\n\n\n\nb"), "a\n\nb");
    }
    #[test]
    fn frames_fit_and_keep_newest() {
        let many: Vec<Entry> = (0..100)
            .map(|i| {
                (
                    Role::Assistant,
                    format!("message {i} {}", "\"x\" ".repeat(40)),
                )
            })
            .collect();
        let v2 = reply("claude:1", Some("agent-a.meta"), Some(many.clone()), 2);
        assert!(encoded(&v2) <= FRAME);
        let rows = v2["entries"].as_array().unwrap();
        assert!(rows.last().unwrap()[1]
            .as_str()
            .unwrap()
            .starts_with("message 99"));
        let huge = vec![(Role::User, format!("{}END", "y".repeat(6000)))];
        let v2 = reply("x", None, Some(huge.clone()), 2);
        assert!(encoded(&v2) <= FRAME);
        assert!(v2["entries"][0][1].as_str().unwrap().ends_with("END"));
        let v1 = reply("x", None, Some(huge), 1);
        assert!(encoded(&v1) <= FRAME && v1["text"].as_str().unwrap().ends_with("END"));
        assert_eq!(reply("x", None, None, 2)["state"], "unavailable");
        assert_eq!(reply("x", None, Some(vec![]), 2)["state"], "empty");
    }
    #[test]
    fn claude_subagent_transcript_includes_sidechain() {
        let dir = std::env::temp_dir().join(format!("agentlight-sub-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("agent-x.jsonl");
        fs::write(&path, concat!(
            r#"{"isSidechain":true,"type":"user","message":{"content":"Explore the repo"}}"#, "\n",
            r#"{"isSidechain":true,"type":"assistant","message":{"content":[{"type":"text","text":"Found it"}]}}"#, "\n",
        )).unwrap();
        assert_eq!(transcript(&path, true).unwrap().len(), 2);
        assert!(transcript(&path, false).unwrap().is_empty());
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn codex_worker_logs_are_scoped_to_the_parent() {
        let dir = std::env::temp_dir().join(format!("agentlight-codex-log-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("worker.jsonl");
        fs::write(&path, concat!(
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Inspect controllers"}]}}"#, "\n",
            r#"{"type":"response_item","payload":{"type":"reasoning","summary":[{"type":"summary_text","text":"private reasoning"}]}}"#, "\n",
            r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Controllers fixed"}]}}"#, "\n",
        )).unwrap();
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE threads (id TEXT, rollout_path TEXT, source TEXT, title TEXT, name TEXT, archived INTEGER, updated_at INTEGER, created_at INTEGER);").unwrap();
        let source = json!({"subagent":{"thread_spawn":{"parent_thread_id":"parent","agent_path":"/root/controllers"}}});
        db.execute(
            "INSERT INTO threads VALUES ('worker',?1,?2,'','',0,1000,1000)",
            rusqlite::params![path.to_str().unwrap(), source.to_string()],
        )
        .unwrap();
        assert_eq!(
            codex_child_entries(&db, "parent", "worker").unwrap(),
            vec![
                (Role::User, "Inspect controllers".into()),
                (Role::Assistant, "Controllers fixed".into()),
            ]
        );
        assert!(codex_child_entries(&db, "other-parent", "worker").is_none());
        assert!(codex_child_entries(&db, "parent", "missing").is_none());
        assert!(codex_child_entries(&db, "parent", "../worker").is_none());
        fs::remove_dir_all(dir).unwrap();
    }
}
