use anyhow::{Context, Result};
mod logs;
mod ota;
mod wifi;
use clap::Parser;
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Parser)]
#[command(about = "USB dashboard for local coding-agent sessions")]
struct Args {
    /// Serial device to probe; otherwise USB serial ports are discovered automatically.
    #[arg(long)]
    port: Option<String>,
    /// Show synthetic sessions instead of inspecting local agents.
    #[arg(long)]
    demo: bool,
    /// Print a single state frame and exit without opening a serial device.
    #[arg(long)]
    once: bool,
    /// Advertise and serve approved displays over encrypted local Wi-Fi.
    #[arg(long)]
    wifi: bool,
    /// Run only the Wi-Fi server; leave serial ports free for setup/flashing.
    #[arg(long, requires = "wifi")]
    wifi_only: bool,
    /// Install this firmware image (firmware.bin) on approved Wi-Fi displays running a different build.
    #[arg(long, requires = "wifi", value_name = "FIRMWARE_BIN")]
    ota: Option<PathBuf>,
    /// Provision the attached display over USB, with an explicit approval prompt.
    #[arg(long, requires = "port", conflicts_with_all = ["once", "wifi_list", "wifi_revoke"])]
    wifi_setup: bool,
    /// 2.4 GHz network name used with --wifi-setup. Password is prompted securely.
    #[arg(long, requires = "wifi_setup")]
    ssid: Option<String>,
    /// List displays approved for remote access.
    #[arg(long)]
    wifi_list: bool,
    /// Revoke remote access for a display ID from --wifi-list.
    #[arg(long)]
    wifi_revoke: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
struct Agent {
    id: String,
    kind: String,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    activity: Option<String>,
    status: String,
    age: u64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    children: Vec<Child>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tasks: Option<TaskProgress>,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
struct TaskProgress {
    completed: u32,
    total: u32,
}
#[derive(Clone, Debug, Serialize)]
struct Child {
    id: String,
    name: String,
    status: String,
    age: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    activity: Option<String>,
}
#[derive(Serialize)]
struct State {
    v: u8,
    t: &'static str,
    now: u64,
    tz: i64,
    agents: Vec<Agent>,
    more: usize,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}
fn env_path(key: &str, fallback: PathBuf) -> PathBuf {
    std::env::var_os(key).map(PathBuf::from).unwrap_or(fallback)
}
fn name(path: &str) -> String {
    let path = Path::new(path);
    // Claude worktrees live under <project>/.claude/worktrees/<worktree>.
    let mut project = path;
    for ancestor in path.ancestors() {
        if ancestor.file_name().is_some_and(|n| n == ".claude")
            && path
                .strip_prefix(ancestor)
                .is_ok_and(|p| p.starts_with("worktrees"))
        {
            project = ancestor.parent().unwrap_or(path);
            break;
        }
    }
    let text = project
        .file_name()
        .and_then(|x| x.to_str())
        .unwrap_or("session");
    limit_text(text, 64)
}
fn limit_text(text: &str, max: usize) -> String {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}
fn activity(text: &str) -> Option<String> {
    let cleaned: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        None
    } else {
        Some(limit_text(cleaned, 96))
    }
}
fn child_name(description: &str, kind: &str) -> String {
    let title = activity(description)
        .or_else(|| activity(kind))
        .unwrap_or_else(|| "Subagent".into());
    limit_text(&title, 40)
}
fn state(mut agents: Vec<Agent>) -> State {
    agents.sort_by(|a, b| (&a.name, &a.kind, &a.id).cmp(&(&b.name, &b.kind, &b.id)));
    let more = agents.len().saturating_sub(24);
    agents.truncate(24);
    // localtime_r accounts for the current timezone and daylight-saving time.
    let t = now() as libc::time_t;
    let mut local: libc::tm = unsafe { std::mem::zeroed() };
    let tz = unsafe {
        if libc::localtime_r(&t, &mut local).is_null() {
            0
        } else {
            local.tm_gmtoff
        }
    };
    State {
        v: 1,
        t: "state",
        now: now(),
        tz,
        agents,
        more,
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Claude {
    pid: u32,
    #[serde(default)]
    cwd: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    waiting_for: serde_json::Value,
    #[serde(default)]
    status_updated_at: u64,
    #[serde(default)]
    updated_at: u64,
    #[serde(default)]
    started_at: u64,
    #[serde(default)]
    spare: bool,
    #[serde(default)]
    parked_job_id: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    job_id: String,
}
impl Claude {
    fn waiting(&self) -> bool {
        match &self.waiting_for {
            serde_json::Value::Null | serde_json::Value::Bool(false) => false,
            serde_json::Value::String(s) => !s.is_empty(),
            _ => true,
        }
    }
    // A shell parked on a background job is shown through that job's record.
    // Idle background jobs stay visible even with no terminal attached; empty
    // daemon placeholders are removed by the history check in claude_sessions.
    fn visible(&self) -> bool {
        self.parked_job_id.is_empty()
    }
}
fn claude_has_history(record: &Claude) -> bool {
    let root = env_path("CLAUDE_CONFIG_DIR", home().join(".claude"));
    claude_history_at(record, &root)
}
fn claude_history_at(record: &Claude, root: &Path) -> bool {
    let safe = |id: &str| !id.is_empty() && !id.contains(['/', '\\']) && id != "." && id != "..";
    let mut ids = vec![record.session_id.clone()];
    if safe(&record.job_id) {
        let job = root.join("jobs").join(&record.job_id);
        if job
            .join("parent-transcript.jsonl")
            .metadata()
            .is_ok_and(|m| m.len() > 0)
        {
            return true;
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct History {
            #[serde(default)]
            resume_session_id: String,
        }
        if let Some(history) = fs::read(job.join("state.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<History>(&b).ok())
        {
            ids.push(history.resume_session_id);
        }
    }
    fs::read_dir(root.join("projects"))
        .into_iter()
        .flatten()
        .flatten()
        .any(|p| {
            ids.iter().filter(|id| safe(id)).any(|id| {
                p.path()
                    .join(format!("{id}.jsonl"))
                    .metadata()
                    .is_ok_and(|m| m.len() > 0)
            })
        })
}

fn claude_sessions() -> Vec<Agent> {
    let dir = env_path("CLAUDE_CONFIG_DIR", home().join(".claude")).join("sessions");
    let records: Vec<Claude> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let filename = e.file_name().to_string_lossy().into_owned();
            let pid = filename.strip_suffix(".json")?.parse::<u32>().ok()?;
            let r: Claude = serde_json::from_slice(&fs::read(e.path()).ok()?).ok()?;
            if pid != r.pid || r.spare || !process_alive(pid) {
                return None;
            }
            Some(r)
        })
        .collect();
    records
        .iter()
        .filter(|r| r.visible())
        // A parked shell can point to an empty daemon placeholder after its
        // real conversation has closed. Do not invent a WAITING session for it.
        .filter(|r| r.kind != "bg" || r.status != "idle" || r.waiting() || claude_has_history(r))
        .map(|r| {
            let pid = r.pid;
            let ms = [r.status_updated_at, r.updated_at, r.started_at]
                .into_iter()
                .find(|v| *v > 0)
                .unwrap_or(now() * 1000);
            let age = now().saturating_sub(ms / 1000);
            let waiting = r.waiting();
            let status = if waiting || r.status == "waiting" {
                "waiting"
            } else if r.status == "busy" {
                "working"
            } else if age >= 7200 {
                "idle"
            } else {
                "waiting"
            };
            Agent {
                id: format!("claude:{pid}"),
                kind: "claude".into(),
                name: name(&r.cwd),
                activity: activity(&r.name),
                status: status.into(),
                age,
                children: claude_children(&r.session_id),
                tasks: claude_tasks(&r.session_id, r.pid),
            }
        })
        .collect()
}
fn claude_tasks(session_id: &str, pid: u32) -> Option<TaskProgress> {
    let env = fs::read(format!("/proc/{pid}/environ")).unwrap_or_default();
    let override_id = env
        .split(|b| *b == 0)
        .filter_map(|s| s.strip_prefix(b"CLAUDE_CODE_TASK_LIST_ID="))
        .find_map(|s| std::str::from_utf8(s).ok())
        .filter(|s| !s.is_empty());
    let session_id = override_id.unwrap_or(session_id);
    if session_id.is_empty()
        || session_id.contains(['/', '\\'])
        || session_id == "."
        || session_id == ".."
    {
        return None;
    }
    let root = env_path("CLAUDE_CONFIG_DIR", home().join(".claude"));
    read_tasks(&root.join("tasks").join(session_id))
}
fn read_tasks(dir: &Path) -> Option<TaskProgress> {
    #[derive(Deserialize)]
    struct Task {
        status: String,
    }
    let mut progress = TaskProgress {
        completed: 0,
        total: 0,
    };
    for file in fs::read_dir(dir).ok()?.flatten() {
        let path = file.path();
        if path.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let Some(task) = fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Task>(&b).ok())
        else {
            continue;
        };
        match task.status.as_str() {
            "completed" => {
                progress.total += 1;
                progress.completed += 1;
            }
            "pending" | "in_progress" => progress.total += 1,
            _ => (),
        }
    }
    (progress.total > 0).then_some(progress)
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeChildMeta {
    #[serde(default)]
    agent_type: String,
    #[serde(default)]
    description: String,
    finished: Option<bool>,
    #[serde(default)]
    stopped_by_user: bool,
}
fn claude_children(session_id: &str) -> Vec<Child> {
    let root = env_path("CLAUDE_CONFIG_DIR", home().join(".claude"));
    claude_children_at(session_id, &root, now())
}
fn claude_children_at(session_id: &str, root: &Path, at: u64) -> Vec<Child> {
    if session_id.is_empty() || session_id.contains(['/', '\\']) {
        return vec![];
    }
    let projects = root.join("projects");
    let mut result = Vec::new();
    for project in fs::read_dir(projects).into_iter().flatten().flatten() {
        let directory = project.path().join(session_id).join("subagents");
        for file in fs::read_dir(directory).into_iter().flatten().flatten() {
            let path = file.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let Some(meta) = fs::read(&path)
                .ok()
                .and_then(|b| serde_json::from_slice::<ClaudeChildMeta>(&b).ok())
            else {
                continue;
            };
            // Stopping an agent can set stoppedByUser without setting finished.
            // Neither marker is required for running worktree-isolated agents.
            let finished = meta.finished.unwrap_or(false) || meta.stopped_by_user;
            let updated = file
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|t| t.as_secs())
                .unwrap_or(0);
            let age = at.saturating_sub(updated);
            if finished && age > 120 {
                continue;
            }
            let id = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("subagent");
            result.push(Child {
                id: limit_text(id, 48),
                name: child_name(&meta.description, &meta.agent_type),
                status: if finished { "done" } else { "working" }.into(),
                age,
                activity: activity(&meta.description),
            });
        }
    }
    result.sort_by(|a, b| a.id.cmp(&b.id));
    result.truncate(6);
    result
}
fn process_alive(pid: u32) -> bool {
    fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|s| s.rsplit_once(") ").map(|(_, rest)| !rest.starts_with('Z')))
        .unwrap_or(false)
}
fn open_db(path: &Path) -> Option<Connection> {
    let db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    db.busy_timeout(Duration::from_millis(100)).ok()?;
    Some(db)
}

fn opencode_status(dir: &str, session_id: Option<&str>) -> (String, u64) {
    let dbpath =
        env_path("XDG_DATA_HOME", home().join(".local/share")).join("opencode/opencode.db");
    let Some(db) = open_db(&dbpath) else {
        return ("waiting".into(), 0);
    };
    let v2 = "WITH RECURSIVE scope(id) AS (SELECT id FROM session_v2 WHERE (?2 = '' AND directory = ?1) OR id = ?2 UNION SELECT c.id FROM session_v2 c JOIN scope s ON c.parent_id=s.id) SELECT m.type, coalesce(json_extract(m.data,'$.error.type'),''), (m.type='idle' OR (m.type='assistant' AND json_extract(m.data,'$.time.completed') IS NOT NULL AND coalesce(json_extract(m.data,'$.finish'),'') != 'tool-calls')), m.time_updated FROM session_message m WHERE m.session_id IN (SELECT id FROM scope) AND m.type IN ('user','assistant','idle') ORDER BY m.time_created DESC,m.seq DESC LIMIT 1";
    let v1 = "WITH RECURSIVE scope(id) AS (SELECT id FROM session WHERE (?2 = '' AND directory = ?1) OR id = ?2 UNION SELECT c.id FROM session c JOIN scope s ON c.parent_id=s.id) SELECT json_extract(m.data,'$.role'), coalesce(json_extract(m.data,'$.error.name'),''), json_extract(m.data,'$.time.completed') IS NOT NULL, m.time_updated FROM message m WHERE m.session_id IN (SELECT id FROM scope) ORDER BY m.time_created DESC LIMIT 1";
    let query = |q| {
        db.query_row(q, [dir, session_id.unwrap_or("")], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, bool>(2)?,
                r.get::<_, u64>(3)?,
            ))
        })
    };
    let row = match query(v2) {
        Err(rusqlite::Error::SqliteFailure(_, Some(ref m))) if m.contains("no such table") => {
            query(v1)
        }
        other => other,
    };
    match row {
        Ok((kind, err, completed, updated)) => {
            let age = now().saturating_sub(updated / 1000);
            let status = if !err.is_empty() {
                "error"
            } else if kind == "idle" || completed {
                if age >= 7200 {
                    "idle"
                } else {
                    "waiting"
                }
            } else {
                "working"
            };
            (status.into(), age)
        }
        Err(_) => ("waiting".into(), 0),
    }
}
fn opencode_children(session_id: &str) -> Vec<Child> {
    let path = env_path("XDG_DATA_HOME", home().join(".local/share")).join("opencode/opencode.db");
    let Some(db) = open_db(&path) else {
        return vec![];
    };
    let query="SELECT id, coalesce(agent,''), coalesce(title,''), coalesce(time_busy,0), coalesce(time_idle,0), coalesce(time_updated,0), coalesce(idle_outcome,'') FROM session_v2 WHERE parent_id=?1 ORDER BY time_created DESC LIMIT 20";
    let Ok(mut stmt) = db.prepare(query) else {
        return vec![];
    };
    let Ok(rows) = stmt.query_map([session_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, u64>(3)?,
            r.get::<_, u64>(4)?,
            r.get::<_, u64>(5)?,
            r.get::<_, String>(6)?,
        ))
    }) else {
        return vec![];
    };
    let mut children = Vec::new();
    for (id, kind, title, busy, idle, updated, outcome) in rows.flatten() {
        // An unstarted or old session is history, not a currently spawned child.
        if busy == 0 {
            continue;
        }
        let running = busy > idle;
        let age = now().saturating_sub((if running { busy } else { idle.max(updated) }) / 1000);
        if !running && age > 120 {
            continue;
        }
        children.push(Child {
            id: limit_text(&id, 48),
            name: child_name(&title, &kind),
            status: if running {
                "working"
            } else if outcome == "failed" {
                "error"
            } else {
                "done"
            }
            .into(),
            age,
            activity: activity(&title),
        });
    }
    children.sort_by(|a, b| a.id.cmp(&b.id));
    children.truncate(6);
    children
}

#[derive(Deserialize)]
struct RolloutEvent {
    timestamp: String,
    #[serde(rename = "type")]
    kind: String,
    payload: RolloutPayload,
}
#[derive(Deserialize)]
struct RolloutPayload {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    error: Option<serde::de::IgnoredAny>,
}

struct CodexThread {
    id: String,
    rollout_path: PathBuf,
    title: Option<String>,
}
fn codex_thread(dir: &str) -> Option<CodexThread> {
    let root = env_path("CODEX_HOME", home().join(".codex"));
    codex_thread_at(&root, dir)
}
fn codex_thread_at(root: &Path, dir: &str) -> Option<CodexThread> {
    let db = codex_db(root)?;
    // Newer Codex databases keep the displayed session name separately from
    // the initial prompt in title. Internal subagent threads are not cards.
    let scope = "FROM threads WHERE cwd=?1 AND archived=0 AND source NOT IN ('exec','mcp') AND source NOT LIKE '%\"subagent\"%' ORDER BY updated_at DESC,created_at DESC LIMIT 1";
    let named =
        format!("SELECT id, rollout_path, coalesce(nullif(trim(name),''),title,'') {scope}");
    let legacy = format!("SELECT id, rollout_path, coalesce(title,'') {scope}");
    let mut query = db.prepare(&named).or_else(|_| db.prepare(&legacy)).ok()?;
    query
        .query_row([dir], |r| {
            Ok(CodexThread {
                id: r.get(0)?,
                rollout_path: PathBuf::from(r.get::<_, String>(1)?),
                title: activity(&r.get::<_, String>(2)?),
            })
        })
        .ok()
}
fn codex_db(root: &Path) -> Option<Connection> {
    let dbpath = fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let version = name
                .strip_prefix("state_")?
                .strip_suffix(".sqlite")?
                .parse::<u32>()
                .ok()?;
            Some((version, e.path()))
        })
        .max_by_key(|(version, _)| *version)
        .map(|(_, p)| p);
    dbpath.as_deref().and_then(open_db)
}
struct CodexChildThread {
    thread: CodexThread,
    label: String,
    archived: bool,
    updated: u64,
}
fn codex_child_threads(db: &Connection, parent: &str) -> Vec<CodexChildThread> {
    // The source metadata records the direct parent even for workers in other
    // worktrees. The spawn-edge status can stay open after a worker finishes.
    let scope = "FROM threads WHERE CASE WHEN json_valid(source) THEN json_extract(source,'$.subagent.thread_spawn.parent_thread_id') END = ?1 ORDER BY updated_at DESC,created_at DESC";
    let named = format!("SELECT id, rollout_path, source, coalesce(nullif(trim(name),''),title,''), archived, updated_at {scope}");
    let legacy = format!(
        "SELECT id, rollout_path, source, coalesce(title,''), archived, updated_at {scope}"
    );
    let Ok(mut query) = db.prepare(&named).or_else(|_| db.prepare(&legacy)) else {
        return vec![];
    };
    let Ok(rows) = query.query_map([parent], |r| {
        let source: String = r.get(2)?;
        let source: serde_json::Value = serde_json::from_str(&source).unwrap_or_default();
        let spawn = &source["subagent"]["thread_spawn"];
        let title = activity(&r.get::<_, String>(3)?);
        let task = spawn["agent_path"]
            .as_str()
            .and_then(|p| Path::new(p).file_name())
            .and_then(|p| p.to_str())
            .and_then(activity);
        let label = title
            .clone()
            .or(task)
            .or_else(|| spawn["agent_nickname"].as_str().and_then(activity))
            .or_else(|| spawn["agent_role"].as_str().and_then(activity))
            .unwrap_or_else(|| "Subagent".into());
        Ok(CodexChildThread {
            thread: CodexThread {
                id: r.get(0)?,
                rollout_path: PathBuf::from(r.get::<_, String>(1)?),
                title,
            },
            label,
            archived: r.get(4)?,
            updated: r.get(5)?,
        })
    }) else {
        return vec![];
    };
    rows.flatten().collect()
}
fn codex_children(parent: &str) -> Vec<Child> {
    let root = env_path("CODEX_HOME", home().join(".codex"));
    let Some(db) = codex_db(&root) else {
        return vec![];
    };
    codex_children_at(&db, parent, now())
}
fn codex_children_at(db: &Connection, parent: &str, at: u64) -> Vec<Child> {
    let mut children = Vec::new();
    for child in codex_child_threads(db, parent) {
        let state = if child.archived {
            Some(("done", at.saturating_sub(child.updated)))
        } else {
            codex_rollout_status(&child.thread.rollout_path, at)
        };
        let Some((status, age)) = state else {
            continue;
        };
        if status != "working" && age > 120 {
            continue;
        }
        children.push(Child {
            id: limit_text(&child.thread.id, 48),
            name: child_name(&child.label, "Subagent"),
            status: status.into(),
            age,
            activity: activity(&child.label),
        });
    }
    // Active workers must not be displaced by recently completed siblings.
    children.sort_by(|a, b| (a.status != "working", &a.id).cmp(&(b.status != "working", &b.id)));
    children.truncate(6);
    children
}
fn codex_rollout_status(path: &Path, at: u64) -> Option<(&'static str, u64)> {
    let mut file = fs::File::open(path).ok()?;
    let mut cursor = file.metadata().ok()?.len();
    let mut line_end = cursor;
    // A long tool result can push task_started out of the last chunk. Scan
    // backwards until a lifecycle event is found, retaining at most one chunk.
    while cursor > 0 {
        let chunk_start = cursor.saturating_sub(262144);
        file.seek(SeekFrom::Start(chunk_start)).ok()?;
        let mut bytes = vec![0; (cursor - chunk_start) as usize];
        file.read_exact(&mut bytes).ok()?;
        let starts = bytes
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, b)| **b == b'\n')
            .map(|(i, _)| chunk_start + i as u64 + 1)
            .chain((chunk_start == 0).then_some(0));
        for start in starts {
            if start < line_end {
                let event = if line_end <= cursor {
                    serde_json::from_slice::<RolloutEvent>(
                        &bytes[(start - chunk_start) as usize..(line_end - chunk_start) as usize],
                    )
                } else {
                    // Stream a line spanning chunks; serde skips tool payloads
                    // and message bodies without retaining those strings.
                    file.seek(SeekFrom::Start(start)).ok()?;
                    serde_json::from_reader::<_, RolloutEvent>(std::io::BufReader::new(
                        (&mut file).take(line_end - start),
                    ))
                };
                if let Ok(event) = event {
                    if let Some(status) = codex_lifecycle(event, at) {
                        return Some(status);
                    }
                }
            }
            line_end = start.saturating_sub(1);
        }
        cursor = chunk_start;
    }
    None
}
fn codex_lifecycle(event: RolloutEvent, at: u64) -> Option<(&'static str, u64)> {
    if event.kind != "event_msg" {
        return None;
    }
    let status = match event.payload.kind.as_str() {
        "task_started" => "working",
        "task_complete" if event.payload.error.is_some() => "error",
        "task_complete" | "turn_aborted" => "done",
        _ => return None,
    };
    let timestamp = chrono::DateTime::parse_from_rfc3339(&event.timestamp).ok()?;
    Some((
        status,
        at.saturating_sub(timestamp.timestamp().max(0) as u64),
    ))
}
fn codex_status(path: &Path) -> (String, u64) {
    let (status, age) = codex_rollout_status(path, now()).unwrap_or(("waiting", 0));
    let status = if status != "working" && age >= 7200 {
        "idle"
    } else if status == "done" {
        "waiting"
    } else {
        status
    };
    (status.into(), age)
}

#[derive(Deserialize)]
struct TabsFile {
    #[serde(default)]
    cwd: std::collections::HashMap<String, FolderTabs>,
}
#[derive(Deserialize)]
struct FolderTabs {
    #[serde(default)]
    tabs: Vec<OpenTab>,
}
#[derive(Deserialize)]
struct OpenTab {
    #[serde(rename = "sessionID")]
    session_id: String,
    #[serde(default)]
    title: String,
}
fn opencode_tabs(dir: &str) -> Vec<OpenTab> {
    let path = env_path("XDG_STATE_HOME", home().join(".local/state"))
        .join("opencode/latest/tui/tabs.json");
    let Some(file) = fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice::<TabsFile>(&b).ok())
    else {
        return vec![];
    };
    let mut seen = HashSet::new();
    file.cwd
        .into_iter()
        .find(|(cwd, _)| Path::new(cwd) == Path::new(dir))
        .map(|(_, folder)| {
            folder
                .tabs
                .into_iter()
                .filter(|t| !t.session_id.is_empty() && seen.insert(t.session_id.clone()))
                .collect()
        })
        .unwrap_or_default()
}

fn process_sessions() -> Vec<Agent> {
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    let entries = match fs::read_dir("/proc") {
        Ok(e) => e,
        Err(_) => return result,
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        if !process_alive(pid) {
            continue;
        }
        let args = fs::read(entry.path().join("cmdline")).unwrap_or_default();
        let args: Vec<_> = args
            .split(|b| *b == 0)
            .filter(|s| !s.is_empty())
            .map(String::from_utf8_lossy)
            .collect();
        let Some(exe) = args.first() else { continue };
        let executable = Path::new(exe.as_ref())
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        if executable != "opencode" && executable != "codex" {
            continue;
        }
        if args.iter().skip(1).any(|s| {
            matches!(
                s.as_ref(),
                "app-server" | "mcp-server" | "exec" | "serve" | "--help" | "--version"
            )
        }) {
            continue;
        }
        let Ok(cwd) = fs::read_link(entry.path().join("cwd")) else {
            continue;
        };
        let dir = cwd.to_string_lossy();
        if !seen.insert((executable.to_owned(), cwd.clone())) {
            continue;
        }
        if executable == "opencode" {
            let tabs = opencode_tabs(&dir);
            if !tabs.is_empty() {
                for tab in tabs {
                    let (status, age) = opencode_status(&dir, Some(&tab.session_id));
                    result.push(Agent {
                        id: limit_text(&format!("opencode:{}", tab.session_id), 48),
                        kind: executable.into(),
                        name: name(&dir),
                        activity: activity(&tab.title),
                        status,
                        age,
                        children: opencode_children(&tab.session_id),
                        tasks: None,
                    });
                }
                continue;
            }
        }
        let codex = if executable == "codex" {
            codex_thread(&dir)
        } else {
            None
        };
        let (status, age) = if executable == "opencode" {
            opencode_status(&dir, None)
        } else {
            codex
                .as_ref()
                .map(|thread| codex_status(&thread.rollout_path))
                .unwrap_or_else(|| ("waiting".into(), 0))
        };
        result.push(Agent {
            id: format!("{executable}:{pid}"),
            kind: executable.into(),
            name: name(&dir),
            children: codex
                .as_ref()
                .map(|thread| codex_children(&thread.id))
                .unwrap_or_default(),
            activity: codex.and_then(|thread| thread.title),
            status,
            age,
            tasks: None,
        });
    }
    result
}
fn collect(demo: bool) -> Vec<Agent> {
    if demo {
        return ["working", "waiting", "error", "idle"]
            .into_iter()
            .enumerate()
            .map(|(i, s)| Agent {
                id: format!("demo:{i}"),
                kind: ["claude", "opencode", "codex", "opencode"][i].into(),
                name: ["api-server", "dashboard", "firmware", "docs"][i].into(),
                activity: activity(
                    [
                        "Implementing status cards",
                        "Reviewing layout",
                        "Fixing USB",
                        "Updating documentation",
                    ][i],
                ),
                status: s.into(),
                age: (i as u64 + 1) * 47,
                children: if i == 0 {
                    vec![Child {
                        id: "demo-child".into(),
                        name: "Explore".into(),
                        status: "working".into(),
                        age: 42,
                        activity: activity("Inspecting the display"),
                    }]
                } else {
                    vec![]
                },
                tasks: if i == 0 {
                    Some(TaskProgress {
                        completed: 1,
                        total: 3,
                    })
                } else {
                    None
                },
            })
            .collect();
    }
    let mut agents = claude_sessions();
    agents.extend(process_sessions());
    agents
}

fn main() -> Result<()> {
    let args = Args::parse();
    if args.wifi_setup {
        return wifi::setup(&args);
    }
    if args.wifi_list {
        return wifi::list();
    }
    if let Some(id) = &args.wifi_revoke {
        return wifi::revoke(id);
    }
    let _wifi = if args.wifi {
        let image = match &args.ota {
            Some(path) => {
                let image = ota::load(path)?;
                eprintln!(
                    "Firmware {} ready for Wi-Fi displays ({} KB).",
                    image.build,
                    image.bytes.len() / 1024
                );
                Some(std::sync::Arc::new(image))
            }
            None => None,
        };
        Some(wifi::start(args.demo, image)?)
    } else {
        None
    };
    if args.wifi_only {
        loop {
            thread::park();
        }
    }
    if args.once {
        println!("{}", serde_json::to_string(&state(collect(args.demo)))?);
        return Ok(());
    }
    loop {
        let ports = if let Some(ref p) = args.port {
            vec![p.clone()]
        } else {
            serialport::available_ports().unwrap_or_default().into_iter().filter(|p| matches!(&p.port_type, serialport::SerialPortType::UsbPort(info) if info.vid == 0x303a)).map(|p| p.port_name).collect()
        };
        for path in ports {
            let Ok(mut port) = serialport::new(&path, 115200)
                .timeout(Duration::from_millis(200))
                .open()
            else {
                continue;
            };
            let _ = port.write_data_terminal_ready(false);
            let _ = port.write_request_to_send(false);
            let _ = port.write_all(b"{\"v\":1,\"t\":\"hello\"}\n");
            let mut line = Vec::new();
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            let mut identified = false;
            let mut logv = 1;
            while std::time::Instant::now() < deadline {
                let mut b = [0];
                if port.read(&mut b).ok() != Some(1) {
                    continue;
                }
                if b[0] == b'\n' {
                    if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&line) {
                        if v["t"] == "hello" && v["device"] == "managents" && v["proto"] == 1 {
                            identified = true;
                            logv = v["logv"].as_u64().unwrap_or(1);
                            break;
                        }
                    }
                    line.clear();
                } else if line.len() < 4096 {
                    line.push(b[0]);
                }
            }
            if !identified {
                continue;
            }
            eprintln!("Connected to {path}");
            loop {
                let mut s = state(collect(args.demo));
                let mut frame = serde_json::to_vec(&s).context("encode state")?;
                while frame.len() > 4096 && !s.agents.is_empty() {
                    if let Some(agent) = s.agents.iter_mut().rev().find(|a| !a.children.is_empty())
                    {
                        agent.children.pop();
                    } else {
                        s.agents.pop();
                        s.more += 1;
                    }
                    frame = serde_json::to_vec(&s)?;
                }
                frame.push(b'\n');
                if port.write_all(&frame).is_err() {
                    eprintln!("Disconnected from {path}");
                    break;
                }
                // Keep the state cadence while receiving on-demand log requests.
                let until = std::time::Instant::now() + Duration::from_secs(2);
                let mut incoming = Vec::new();
                let mut overflow = false;
                let mut answered = false;
                while std::time::Instant::now() < until {
                    let mut bytes = [0u8; 128];
                    let count = match port.read(&mut bytes) {
                        Ok(n) => n,
                        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => continue,
                        Err(_) => break,
                    };
                    for byte in &bytes[..count] {
                        if *byte == b'\n' {
                            if !overflow && !answered {
                                if let Some(response) =
                                    serde_json::from_slice::<serde_json::Value>(&incoming)
                                        .ok()
                                        .and_then(|r| logs::answer(&r, &s, args.demo, logv))
                                {
                                    let mut frame = serde_json::to_vec(&response)?;
                                    frame.push(b'\n');
                                    port.write_all(&frame).context("send log tail")?;
                                    answered = true;
                                }
                            }
                            incoming.clear();
                            overflow = false;
                        } else if incoming.len() < 512 && !overflow {
                            incoming.push(*byte);
                        } else {
                            overflow = true;
                        }
                    }
                }
            }
        }
        thread::sleep(Duration::from_secs(2));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tail_answers_only_known_agents_and_children() {
        let s = state(collect(true));
        let parent = s.agents.iter().find(|a| !a.children.is_empty()).unwrap();
        let child = &parent.children[0].id;
        let ask = |v: serde_json::Value| logs::answer(&v, &s, true, 2);
        let agent = ask(serde_json::json!({"v":1,"t":"tail","id":parent.id})).unwrap();
        assert_eq!(agent["state"], "ok");
        assert!(agent.get("child").is_none());
        let sub = ask(serde_json::json!({"v":1,"t":"tail","id":parent.id,"child":child})).unwrap();
        assert_eq!(sub["child"], child.as_str());
        assert_ne!(sub["entries"], agent["entries"]);
        assert!(ask(serde_json::json!({"v":1,"t":"tail","id":"nope"})).is_none());
        assert!(ask(serde_json::json!({"v":1,"t":"tail","id":parent.id,"child":"nope"})).is_none());
        let legacy = logs::answer(
            &serde_json::json!({"v":1,"t":"tail","id":parent.id}),
            &s,
            true,
            1,
        )
        .unwrap();
        assert!(legacy["text"].as_str().unwrap().starts_with("user: "));
    }
    #[test]
    fn demo_is_valid_and_bounded() {
        let s = state(collect(true));
        let v = serde_json::to_value(s).unwrap();
        assert_eq!(v["v"], 1);
        assert_eq!(v["agents"].as_array().unwrap().len(), 4);
    }
    #[test]
    fn cap_and_order() {
        let a = Agent {
            id: "x".into(),
            kind: "x".into(),
            name: "x".into(),
            activity: None,
            status: "idle".into(),
            age: 0,
            children: vec![],
            tasks: None,
        };
        let s = state(vec![a; 30]);
        assert_eq!(s.more, 6);
        assert_eq!(s.agents.len(), 24);
    }
    #[test]
    fn order_does_not_depend_on_status_age_or_input_order() {
        let make = |id: &str, name: &str, status: &str, age| Agent {
            id: id.into(),
            kind: "opencode".into(),
            name: name.into(),
            activity: None,
            status: status.into(),
            age,
            children: vec![],
            tasks: None,
        };
        let first = state(vec![
            make("b", "beta", "working", 1),
            make("a", "alpha", "idle", 500),
        ]);
        let second = state(vec![
            make("a", "alpha", "working", 0),
            make("b", "beta", "waiting", 9),
        ]);
        assert_eq!(
            first.agents.iter().map(|a| &a.id).collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        assert_eq!(
            first.agents.iter().map(|a| &a.id).collect::<Vec<_>>(),
            second.agents.iter().map(|a| &a.id).collect::<Vec<_>>()
        );
    }
    #[test]
    fn project_names_handle_worktrees_and_long_names() {
        assert_eq!(
            name("/home/user/recdot/.claude/worktrees/android"),
            "recdot"
        );
        assert_eq!(
            name("/home/user/long-project-name-not-truncated"),
            "long-project-name-not-truncated"
        );
        assert_eq!(limit_text("ééé", 3), "é");
    }
    #[test]
    fn claude_activity_is_distinct_from_project_name() {
        let record: Claude = serde_json::from_str(r#"{"pid":1,"cwd":"/projects/dreammapper","name":"support-s3square-board","status":"busy"}"#).unwrap();
        assert_eq!(name(&record.cwd), "dreammapper");
        assert_eq!(
            activity(&record.name).as_deref(),
            Some("support-s3square-board")
        );
        assert_eq!(activity(" \n "), None);
    }
    #[test]
    fn opencode_tabs_keep_distinct_titles_and_ids() {
        let file: TabsFile = serde_json::from_str(r#"{"cwd":{"/project":{"tabs":[{"sessionID":"one","title":"First"},{"sessionID":"two","title":"Second"}]}}}"#).unwrap();
        assert_eq!(file.cwd["/project"].tabs.len(), 2);
        assert_eq!(file.cwd["/project"].tabs[1].title, "Second");
    }
    #[test]
    fn codex_cards_use_saved_names_and_support_legacy_titles() {
        let root =
            std::env::temp_dir().join(format!("agentlight-codex-test-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let legacy = Connection::open(root.join("state_4.sqlite")).unwrap();
        legacy.execute_batch("CREATE TABLE threads (id TEXT, cwd TEXT, rollout_path TEXT, title TEXT, source TEXT, archived INTEGER, updated_at INTEGER, created_at INTEGER);
            INSERT INTO threads VALUES ('legacy','/project','legacy.jsonl','Legacy title','cli',0,100,100);").unwrap();
        drop(legacy);
        assert_eq!(
            codex_thread_at(&root, "/project").unwrap().title.as_deref(),
            Some("Legacy title")
        );

        let db = Connection::open(root.join("state_5.sqlite")).unwrap();
        db.execute_batch("CREATE TABLE threads (id TEXT, cwd TEXT, rollout_path TEXT, title TEXT, name TEXT, source TEXT, archived INTEGER, updated_at INTEGER, created_at INTEGER);
            INSERT INTO threads VALUES ('current','/project','current.jsonl','Original prompt','Fix subagent count mismatch','vscode',0,10,10);").unwrap();
        for (cwd, source, archived) in [
            ("/other", "cli", 0),
            ("/project", "cli", 1),
            ("/project", "exec", 0),
            ("/project", "mcp", 0),
            ("/project", r#"{"subagent":{"other":"guardian"}}"#, 0),
        ] {
            db.execute("INSERT INTO threads VALUES ('internal',?1,'internal.jsonl','Internal title','Internal name',?2,?3,20,20)", rusqlite::params![cwd, source, archived]).unwrap();
        }
        let thread = codex_thread_at(&root, "/project").unwrap();
        assert_eq!(thread.title.as_deref(), Some("Fix subagent count mismatch"));
        assert_eq!(thread.rollout_path, Path::new("current.jsonl"));
        for name in [Some(""), Some("   "), None] {
            db.execute(
                "UPDATE threads SET name=?1 WHERE rollout_path='current.jsonl'",
                [name],
            )
            .unwrap();
            assert_eq!(
                codex_thread_at(&root, "/project").unwrap().title.as_deref(),
                Some("Original prompt")
            );
        }
        db.execute(
            "UPDATE threads SET title='  ' WHERE rollout_path='current.jsonl'",
            [],
        )
        .unwrap();
        assert!(codex_thread_at(&root, "/project").unwrap().title.is_none());
        assert!(codex_thread_at(&root, "/missing").is_none());
        drop(db);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn codex_workers_follow_direct_parent_and_latest_lifecycle() {
        let root =
            std::env::temp_dir().join(format!("agentlight-workers-test-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let db = Connection::open_in_memory().unwrap();
        // Older databases have no saved name or spawn-edge table.
        db.execute_batch("CREATE TABLE threads (id TEXT, rollout_path TEXT, source TEXT, title TEXT, archived INTEGER, updated_at INTEGER, created_at INTEGER);").unwrap();
        let insert = |id: &str,
                      parent: &str,
                      kind: &str,
                      timestamp: u64,
                      archived: bool,
                      updated: u64| {
            let source = serde_json::json!({"subagent":{"thread_spawn":{
                "parent_thread_id":parent,"agent_path":format!("/root/{id}"),"agent_nickname":"Worker nickname"
            }}});
            let path = root.join(format!("{id}.jsonl"));
            let event = serde_json::json!({
                "timestamp":chrono::DateTime::from_timestamp(timestamp as i64,0).unwrap().to_rfc3339(),
                "type":"event_msg","payload":{"type":kind,"error":if id == "failed" { serde_json::json!({"message":"failed"}) } else { serde_json::Value::Null }}
            });
            fs::write(&path, format!("{{broken\n{event}\n")).unwrap();
            db.execute(
                "INSERT INTO threads VALUES (?1,?2,?3,'',?4,?5,700)",
                rusqlite::params![
                    id,
                    path.to_str().unwrap(),
                    source.to_string(),
                    archived,
                    updated
                ],
            )
            .unwrap();
        };
        insert("working", "parent", "task_started", 800, false, 1000);
        insert("done", "parent", "task_complete", 1000, false, 1000);
        insert("failed", "parent", "task_complete", 1000, false, 1000);
        insert("aborted", "parent", "turn_aborted", 1000, false, 1000);
        insert("archived", "parent", "task_started", 800, true, 1000);
        insert("old-done", "parent", "task_complete", 999, false, 1120);
        insert("old-archived", "parent", "task_started", 800, true, 999);
        insert("other-parent", "other", "task_started", 1000, false, 1000);
        insert("grandchild", "working", "task_started", 1000, false, 1000);
        insert("unstarted", "parent", "token_count", 1000, false, 1000);
        // Invalid sources and automatic reviews must not become workers.
        for source in ["cli", "{bad", r#"{"subagent":{"other":"guardian"}}"#] {
            db.execute(
                "INSERT INTO threads VALUES ('internal','missing',?1,'',0,1120,700)",
                [source],
            )
            .unwrap();
        }
        let recent = codex_children_at(&db, "parent", 1120);
        assert_eq!(recent.len(), 5);
        assert_eq!(recent[0].id, "working");
        assert_eq!(recent[0].name, "working");
        assert_eq!(recent[0].status, "working");
        assert_eq!(recent[0].age, 320);
        for child in &recent[1..] {
            assert_eq!(child.age, 120);
            assert_eq!(
                child.status,
                if child.id == "failed" {
                    "error"
                } else {
                    "done"
                }
            );
        }
        let expired = codex_children_at(&db, "parent", 1121);
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].id, "working");
        // A follow-up turn on the same worker makes it active again.
        let resumed = serde_json::json!({"timestamp":"1970-01-01T00:18:30Z","type":"event_msg","payload":{"type":"task_started"}});
        fs::OpenOptions::new()
            .append(true)
            .open(root.join("done.jsonl"))
            .unwrap()
            .write_all(format!("{resumed}\n").as_bytes())
            .unwrap();
        assert!(codex_children_at(&db, "parent", 1121)
            .iter()
            .any(|c| c.id == "done" && c.status == "working" && c.age == 11));
        // Completed workers cannot crowd active workers out of the six pills.
        for i in 0..6 {
            insert(
                &format!("active-{i}"),
                "parent",
                "task_started",
                1000,
                false,
                1000,
            );
        }
        let capped = codex_children_at(&db, "parent", 1120);
        assert_eq!(capped.len(), 6);
        assert!(capped.iter().all(|c| c.status == "working"));
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn codex_lifecycle_survives_large_tool_results_and_completion_lines() {
        let path =
            std::env::temp_dir().join(format!("agentlight-lifecycle-{}.jsonl", std::process::id()));
        let started = r#"{"timestamp":"1970-01-01T00:16:40Z","type":"event_msg","payload":{"type":"task_started"}}"#;
        let output = serde_json::json!({"timestamp":"1970-01-01T00:16:41Z","type":"response_item","payload":{"type":"function_call_output","output":"é".repeat(300000)}});
        fs::write(&path, format!("{started}\n{output}\n")).unwrap();
        assert_eq!(codex_rollout_status(&path, 1120), Some(("working", 120)));
        let complete = serde_json::json!({"timestamp":"1970-01-01T00:18:20Z","type":"event_msg","payload":{"type":"task_complete","last_agent_message":"Done. ".repeat(60000)}});
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(format!("{complete}\n{{partial").as_bytes())
            .unwrap();
        assert_eq!(codex_rollout_status(&path, 1120), Some(("done", 20)));
        // The first event is also valid when the file has no trailing newline.
        fs::write(&path, started).unwrap();
        assert_eq!(codex_rollout_status(&path, 1120), Some(("working", 120)));
        fs::remove_file(path).unwrap();
    }
    #[test]
    fn claude_child_metadata_and_demo_protocol() {
        let child: ClaudeChildMeta = serde_json::from_str(
            r#"{"agentType":"Explore","description":"Inspect UI","finished":false}"#,
        )
        .unwrap();
        assert_eq!(child.agent_type, "Explore");
        assert_eq!(child.description, "Inspect UI");
        assert_eq!(child.finished, Some(false));
        let running: ClaudeChildMeta = serde_json::from_str(r#"{"agentType":"general-purpose","description":"Implement feature","spawnedWithWorktree":true}"#).unwrap();
        assert!(!running.finished.unwrap_or(false));
        let frame = serde_json::to_value(state(collect(true))).unwrap();
        let children = frame["agents"]
            .as_array()
            .unwrap()
            .iter()
            .find_map(|a| a.get("children"))
            .unwrap();
        assert_eq!(children[0]["name"], "Explore");
        assert_eq!(children[0]["status"], "working");
    }
    #[test]
    fn child_labels_prefer_task_over_generic_role() {
        assert_eq!(
            child_name("Fix display layout", "general-purpose"),
            "Fix display layout"
        );
        assert_eq!(child_name("", "Explore"), "Explore");
        assert_eq!(child_name("", ""), "Subagent");
        assert!(child_name(&"é".repeat(40), "Explore").len() <= 40);
    }
    #[test]
    fn stopped_claude_children_expire_even_without_finished_marker() {
        let root =
            std::env::temp_dir().join(format!("agentlight-children-test-{}", std::process::id()));
        let dir = root.join("projects/project/session/subagents");
        fs::create_dir_all(&dir).unwrap();
        for (name, body) in [
            (
                "agent-running.meta.json",
                r#"{"description":"Running","spawnedWithWorktree":true}"#,
            ),
            (
                "agent-stopped.meta.json",
                r#"{"description":"Stopped","stoppedByUser":true}"#,
            ),
            (
                "agent-stopped-false.meta.json",
                r#"{"description":"Stopped before finish","finished":false,"stoppedByUser":true}"#,
            ),
            (
                "agent-finished.meta.json",
                r#"{"description":"Finished","finished":true}"#,
            ),
        ] {
            let path = dir.join(name);
            fs::write(&path, body).unwrap();
            fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_times(
                    fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(1000)),
                )
                .unwrap();
        }
        let recent = claude_children_at("session", &root, 1120);
        assert_eq!(recent.len(), 4);
        for child in &recent {
            assert_eq!(child.age, 120);
            assert_eq!(
                child.status,
                if child.name == "Running" {
                    "working"
                } else {
                    "done"
                }
            );
        }
        let expired = claude_children_at("session", &root, 1121);
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].name, "Running");
        assert_eq!(expired[0].status, "working");
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn idle_detached_jobs_stay_visible_but_parked_shells_do_not() {
        let record = |fields: &str| serde_json::from_str::<Claude>(fields).unwrap();
        let detached = record(r#"{"pid":1,"kind":"bg","jobId":"old","status":"idle"}"#);
        let current = record(r#"{"pid":2,"kind":"bg","jobId":"current","status":"idle"}"#);
        let parked = record(r#"{"pid":3,"kind":"interactive","parkedJobId":"current"}"#);
        let busy = record(r#"{"pid":4,"kind":"bg","jobId":"other","status":"busy"}"#);
        let interactive =
            record(r#"{"pid":5,"kind":"interactive","cwd":"/same/project","status":"idle"}"#);
        assert!(detached.visible());
        assert!(current.visible());
        assert!(!parked.visible());
        assert!(busy.visible());
        assert!(interactive.visible());
    }
    #[test]
    fn empty_background_placeholder_is_not_conversation_history() {
        let root =
            std::env::temp_dir().join(format!("agentlight-history-test-{}", std::process::id()));
        let project = root.join("projects").join("project");
        fs::create_dir_all(&project).unwrap();
        let record: Claude = serde_json::from_str(
            r#"{"pid":1,"sessionId":"placeholder","kind":"bg","status":"idle","jobId":"job"}"#,
        )
        .unwrap();
        assert!(!claude_history_at(&record, &root));
        fs::write(project.join("placeholder.jsonl"), "{}\n").unwrap();
        assert!(claude_history_at(&record, &root));
        fs::remove_file(project.join("placeholder.jsonl")).unwrap();
        fs::remove_dir(&project).unwrap();
        fs::remove_dir(root.join("projects")).unwrap();
        fs::remove_dir(root).unwrap();
    }
    #[test]
    fn task_counts_ignore_invalid_and_unknown_entries() {
        let dir = std::env::temp_dir().join(format!("agentlight-task-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        for (name, body) in [
            ("1.json", r#"{"status":"completed"}"#),
            ("2.json", r#"{"status":"pending"}"#),
            ("3.json", r#"{"status":"in_progress"}"#),
            ("4.json", r#"{"status":"deleted"}"#),
            ("bad.json", "broken"),
            ("other.txt", r#"{"status":"completed"}"#),
        ] {
            fs::write(dir.join(name), body).unwrap();
        }
        assert_eq!(
            read_tasks(&dir),
            Some(TaskProgress {
                completed: 1,
                total: 3
            })
        );
        for entry in fs::read_dir(&dir).unwrap().flatten() {
            fs::remove_file(entry.path()).unwrap();
        }
        fs::remove_dir(dir).unwrap();
        assert_eq!(claude_tasks("../bad", 0), None);
    }
}
