//! Local Kilo CLI sessions. The native CLI and daemon share a database, so
//! only live terminal processes create cards; database history alone does not.
use crate::logs::{clean, Entry, Role};
use crate::{
    activity, child_name, env_path, home, limit_text, name, open_db, Agent, Child, Ctx,
    TaskProgress,
};
use rusqlite::Connection;
use std::{
    fs,
    path::{Path, PathBuf},
};

pub fn is_session_command(executable: &str, args: &[&str]) -> bool {
    if !matches!(executable, "kilo" | ".kilo")
        || args.iter().any(|s| matches!(*s, "--help" | "--version"))
    {
        return false;
    }
    let mut args = args.iter().skip(1);
    while let Some(arg) = args.next() {
        if matches!(
            *arg,
            "--session"
                | "-s"
                | "--model"
                | "-m"
                | "--agent"
                | "--prompt"
                | "--port"
                | "--hostname"
                | "--attach"
                | "--dir"
                | "--variant"
                | "--log-level"
        ) {
            args.next();
            continue;
        }
        if arg.starts_with('-') {
            continue;
        }
        return !matches!(
            *arg,
            "serve"
                | "daemon"
                | "db"
                | "session"
                | "export"
                | "import"
                | "auth"
                | "models"
                | "mcp"
                | "upgrade"
                | "uninstall"
                | "stats"
                | "debug"
        );
    }
    true
}

pub fn database_path(data: &Path, override_path: Option<&str>) -> Option<PathBuf> {
    match override_path {
        Some(":memory:") => None,
        Some(s) if !s.is_empty() => Some(data.join("kilo").join(s)),
        _ => Some(data.join("kilo/kilo.db")),
    }
}
fn database(pid: u32) -> Option<Connection> {
    let env = fs::read(format!("/proc/{pid}/environ")).unwrap_or_default();
    let value = |key: &str| {
        env.split(|b| *b == 0).find_map(|v| {
            let (k, v) = std::str::from_utf8(v).ok()?.split_once('=')?;
            (k == key).then_some(v)
        })
    };
    let data = value("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| env_path("XDG_DATA_HOME", home().join(".local/share")));
    let configured = std::env::var("KILO_DB").ok();
    open_db(&database_path(
        &data,
        value("KILO_DB").or(configured.as_deref()),
    )?)
}
pub fn session_arg(args: &[&str]) -> Option<String> {
    args.iter().enumerate().find_map(|(i, s)| {
        s.strip_prefix("--session=")
            .or_else(|| {
                matches!(*s, "--session" | "-s")
                    .then(|| args.get(i + 1).copied())
                    .flatten()
            })
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    })
}
fn parent(db: &Connection, dir: &str, session: Option<&str>) -> Option<(String, String)> {
    db.query_row("SELECT id,title FROM session WHERE directory=?1 AND parent_id IS NULL AND time_archived IS NULL AND (?2='' OR id=?2) ORDER BY time_updated DESC,time_created DESC,id DESC LIMIT 1",[dir,session.unwrap_or("")],|r| Ok((r.get(0)?,r.get(1)?))).ok()
}
fn status(db: &Connection, session: &str, at: u64) -> Option<(String, u64)> {
    let (role,error,completed,finish,updated): (String,String,Option<u64>,String,u64) = db.query_row(
        "SELECT coalesce(json_extract(data,'$.role'),''),coalesce(json_extract(data,'$.error.name'),''),json_extract(data,'$.time.completed'),coalesce(json_extract(data,'$.finish'),''),time_updated FROM message WHERE session_id=?1 ORDER BY time_created DESC,id DESC LIMIT 1",
        [session],|r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).ok()?;
    let age = at.saturating_sub(completed.unwrap_or(updated) / 1000);
    // A cancelled turn isn't a failure: it's over, and it's the user's move.
    let base = if !error.is_empty() && error != "MessageAbortedError" {
        "error"
    } else if role == "assistant" && completed.is_some() && finish != "tool-calls" {
        if age >= 7200 {
            "idle"
        } else {
            "waiting"
        }
    } else {
        "working"
    };
    Some(if base == "error" {
        (base.into(), age)
    } else {
        crate::opencode_waits(db, "", Some(session)).resolve((base, age), at)
    })
}
fn context(db: &Connection, session: &str) -> Option<Ctx> {
    let used: u64 = db.query_row("SELECT coalesce(json_extract(data,'$.tokens.input'),0)+coalesce(json_extract(data,'$.tokens.cache.read'),0)+coalesce(json_extract(data,'$.tokens.cache.write'),0) AS used FROM message WHERE session_id=?1 AND json_extract(data,'$.role')='assistant' AND used>0 ORDER BY time_created DESC,id DESC LIMIT 1",[session],|r|r.get(0)).ok()?;
    Some(Ctx { used, limit: None })
}
/// Progress of the session's built-in todo list. As with Claude's tasks, only
/// open and finished items count; cancelled ones leave the total. Kilo keeps a
/// finished list in the table after it stops showing it, so a list with nothing
/// left to do counts as no list.
fn tasks(db: &Connection, session: &str) -> Option<TaskProgress> {
    let (completed, total): (u32, u32) = db
        .query_row(
            "SELECT coalesce(sum(status='completed'),0),coalesce(sum(status IN ('completed','pending','in_progress')),0) FROM todo WHERE session_id=?1",
            [session],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok()?;
    (completed < total).then_some(TaskProgress { completed, total })
}
/// The newest tool still unfinished in the session's latest message.
fn unfinished_tool(db: &Connection, session: &str) -> Option<(String, u64)> {
    db.query_row(
        "SELECT json_extract(data,'$.tool'),coalesce(json_extract(data,'$.state.time.start'),time_updated) FROM part WHERE session_id=?1 AND message_id=(SELECT id FROM message WHERE session_id=?1 ORDER BY time_created DESC,id DESC LIMIT 1) AND json_extract(data,'$.type')='tool' AND json_extract(data,'$.state.status') IN ('pending','running') AND (json_extract(data,'$.tool')<>'background_process' OR json_extract(data,'$.state.input.action')='start') ORDER BY time_created DESC,id DESC LIMIT 1",
        [session],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, u64>(1)? / 1000)),
    )
    .ok()
}
fn children(db: &Connection, parent: &str, at: u64) -> Vec<Child> {
    let Ok(mut query) = db.prepare("SELECT id,title FROM session WHERE parent_id=?1 AND time_archived IS NULL ORDER BY time_created DESC,id DESC") else { return vec![]; };
    let Ok(rows) = query.query_map([parent], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    }) else {
        return vec![];
    };
    let mut children = vec![];
    for (id, title) in rows.flatten() {
        let Some((state, age)) = status(db, &id, at) else {
            continue;
        };
        let active = matches!(state.as_str(), "working" | "waiting_task");
        if !active && age > 120 {
            continue;
        }
        children.push(Child {
            id: limit_text(&id, 48),
            name: child_name(&title, "Subagent"),
            status: if active {
                "working"
            } else if state == "error" {
                "error"
            } else {
                "done"
            }
            .into(),
            age,
            activity: activity(&title),
        });
    }
    children.sort_by(|a, b| (a.status != "working", &a.id).cmp(&(b.status != "working", &b.id)));
    children.truncate(6);
    children
}
fn agent(db: &Connection, pid: u32, dir: &str, selected: Option<&str>, at: u64) -> Option<Agent> {
    let (session, title) = parent(db, dir, selected)?;
    let (status, age) = status(db, &session, at).unwrap_or_else(|| {
        let updated: u64 = db
            .query_row(
                "SELECT time_updated FROM session WHERE id=?1",
                [&session],
                |r| r.get(0),
            )
            .unwrap_or(at * 1000);
        let age = at.saturating_sub(updated / 1000);
        (if age >= 7200 { "idle" } else { "waiting" }.into(), age)
    });
    // Kilo doesn't store permission prompts, so infer one from a stalled tool.
    let children = children(db, &session, at);
    let stopped = (status == "working")
        .then(|| {
            crate::codex_stopped_on_user(
                unfinished_tool(db, &session).as_ref(),
                at,
                |since| crate::process_started_since(since, &[".kilo", "kilo"]),
                children.iter().any(|c| c.status == "working"),
            )
        })
        .flatten();
    let (status, age) = match stopped {
        Some(waited) => ("waiting".to_string(), waited),
        None => (status, age),
    };
    Some(Agent {
        id: format!("kilo:{pid}"),
        kind: "kilo".into(),
        name: name(dir),
        activity: activity(&title),
        status,
        age,
        children,
        ctx: context(db, &session),
        tasks: tasks(db, &session),
        limits: vec![],
    })
}
/// Live Kilo terminals in a directory, oldest first.
fn terminals(dir: &str) -> Vec<u32> {
    let mut found: Vec<(u64, u32)> = fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let pid = entry.file_name().to_str()?.parse::<u32>().ok()?;
            let exe = fs::read_link(entry.path().join("exe")).ok()?;
            let executable = exe.file_name()?.to_str()?;
            let bytes = fs::read(entry.path().join("cmdline")).ok()?;
            let text = String::from_utf8_lossy(&bytes);
            let args: Vec<&str> = text.split('\0').filter(|s| !s.is_empty()).collect();
            if !is_session_command(executable, &args) {
                return None;
            }
            if fs::read_link(entry.path().join("cwd")).ok()?.to_str()? != dir {
                return None;
            }
            let stat = fs::read_to_string(entry.path().join("stat")).ok()?;
            let start = stat
                .rsplit_once(") ")?
                .1
                .split_whitespace()
                .nth(19)?
                .parse()
                .ok()?;
            Some((start, pid))
        })
        .collect();
    found.sort();
    found.into_iter().map(|(_, pid)| pid).collect()
}
/// The `count` most recently active root sessions in a directory, oldest
/// created first. Kilo doesn't bump a session's own timestamp as it works, so
/// activity comes from its latest message.
fn active_sessions(db: &Connection, dir: &str, count: usize) -> Vec<String> {
    let Ok(mut query) = db.prepare(
        "SELECT id FROM (SELECT s.id,s.time_created FROM session s WHERE s.directory=?1 AND s.parent_id IS NULL AND s.time_archived IS NULL ORDER BY coalesce((SELECT max(m.time_updated) FROM message m WHERE m.session_id=s.id),s.time_updated) DESC LIMIT ?2) ORDER BY time_created,id",
    ) else {
        return vec![];
    };
    query
        .query_map(rusqlite::params![dir, count as i64], |r| r.get(0))
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
}
/// Kilo doesn't record which terminal owns a session. With several terminals in
/// one directory, pair them in order: a terminal is usually started just before
/// the session it creates.
fn session_for_terminal(db: &Connection, dir: &str, pid: u32) -> Option<String> {
    let terminals = terminals(dir);
    if terminals.len() < 2 {
        return None;
    }
    let place = terminals.iter().position(|p| *p == pid)?;
    active_sessions(db, dir, terminals.len())
        .into_iter()
        .nth(place)
}
pub fn collect(pid: u32, dir: &str, args: &[&str]) -> Option<Agent> {
    let db = database(pid)?;
    let selected = session_arg(args).or_else(|| session_for_terminal(&db, dir, pid));
    agent(&db, pid, dir, selected.as_deref(), crate::now())
}
fn entries(db: &Connection, parent: &str, child: Option<&str>) -> Option<Vec<Entry>> {
    let session = child.unwrap_or(parent);
    if child.is_some()
        && !db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM session WHERE id=?1 AND parent_id=?2)",
                [session, parent],
                |r| r.get::<_, bool>(0),
            )
            .ok()?
    {
        return None;
    }
    let mut query = db.prepare("SELECT json_extract(m.data,'$.role'),json_extract(p.data,'$.type'),substr(CASE WHEN json_extract(p.data,'$.type')='tool' THEN json_extract(p.data,'$.tool') ELSE json_extract(p.data,'$.text') END,1,262144) FROM part p JOIN message m ON m.id=p.message_id AND m.session_id=p.session_id WHERE p.session_id=?1 AND json_extract(m.data,'$.role') IN ('user','assistant') AND json_extract(p.data,'$.type') IN ('text','tool') ORDER BY m.time_created DESC,m.id DESC,p.time_created DESC,p.id DESC LIMIT 60").ok()?;
    let rows = query
        .query_map([session], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .ok()?;
    let mut entries: Vec<_> = rows
        .flatten()
        .filter_map(|(role, kind, text)| {
            let text = clean(&text);
            (!text.is_empty()).then_some((
                if kind == "tool" {
                    Role::Tool
                } else if role == "user" {
                    Role::User
                } else {
                    Role::Assistant
                },
                text,
            ))
        })
        .collect();
    entries.reverse();
    Some(entries)
}
pub fn logs(pid: u32, child: Option<&str>) -> Option<Vec<Entry>> {
    let dir = fs::read_link(format!("/proc/{pid}/cwd")).ok()?;
    let args = fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let args: Vec<_> = args
        .split(|b| *b == 0)
        .filter_map(|s| std::str::from_utf8(s).ok())
        .collect();
    let db = database(pid)?;
    let (session, _) = parent(&db, &dir.to_string_lossy(), session_arg(&args).as_deref())?;
    entries(&db, &session, child)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn db() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE session (id TEXT,directory TEXT,parent_id TEXT,title TEXT,time_created INTEGER,time_updated INTEGER,time_archived INTEGER); CREATE TABLE message (id TEXT,session_id TEXT,time_created INTEGER,time_updated INTEGER,data TEXT); CREATE TABLE part (id TEXT,message_id TEXT,session_id TEXT,time_created INTEGER,time_updated INTEGER,data TEXT);").unwrap();
        db
    }
    fn session(db: &Connection, id: &str, parent: Option<&str>, at: u64) {
        db.execute(
            "INSERT INTO session VALUES (?1,'/project',?2,?1,?3,?3,NULL)",
            rusqlite::params![id, parent, at],
        )
        .unwrap();
    }
    fn message(db: &Connection, id: &str, session: &str, at: u64, data: serde_json::Value) {
        db.execute(
            "INSERT INTO message VALUES (?1,?2,?3,?3,?4)",
            rusqlite::params![id, session, at, data.to_string()],
        )
        .unwrap();
    }
    fn part(
        db: &Connection,
        id: &str,
        message: &str,
        session: &str,
        at: u64,
        data: serde_json::Value,
    ) {
        db.execute(
            "INSERT INTO part VALUES (?1,?2,?3,?4,?4,?5)",
            rusqlite::params![id, message, session, at, data.to_string()],
        )
        .unwrap();
    }
    #[test]
    fn commands_and_paths_do_not_mix_kilo_with_its_daemon_or_opencode() {
        assert!(is_session_command(".kilo", &["/bin/.kilo"]));
        assert!(is_session_command("kilo", &["kilo", "run", "prompt"]));
        assert!(is_session_command("kilo", &["kilo", "run", "serve"]));
        assert!(is_session_command("kilo", &["kilo", "--session", "serve"]));
        assert!(!is_session_command(
            "kilo",
            &["kilo", "--port", "4097", "serve"]
        ));
        for command in ["serve", "daemon", "session", "db", "--help", "--version"] {
            assert!(!is_session_command(".kilo", &[".kilo", command]));
        }
        assert!(!is_session_command("node", &["node", "/bin/kilo"]));
        assert_eq!(
            database_path(Path::new("/data"), None),
            Some(PathBuf::from("/data/kilo/kilo.db"))
        );
        assert_eq!(
            database_path(Path::new("/data"), Some("custom.db")),
            Some(PathBuf::from("/data/kilo/custom.db"))
        );
        assert_eq!(
            database_path(Path::new("/data"), Some("/tmp/test.db")),
            Some(PathBuf::from("/tmp/test.db"))
        );
        assert!(database_path(Path::new("/data"), Some(":memory:")).is_none());
        for args in [
            vec!["kilo", "-s", "ses_old"],
            vec!["kilo", "--session", "ses_old"],
            vec!["kilo", "--session=ses_old"],
        ] {
            assert_eq!(session_arg(&args).as_deref(), Some("ses_old"));
        }
    }
    #[test]
    fn parent_selection_ignores_archived_history_and_children() {
        let db = db();
        session(&db, "old", None, 100);
        session(&db, "new", None, 200);
        session(&db, "worker", Some("new"), 300);
        session(&db, "archived", None, 400);
        db.execute(
            "UPDATE session SET time_archived=401 WHERE id='archived'",
            [],
        )
        .unwrap();
        assert_eq!(parent(&db, "/project", None).unwrap().0, "new");
        assert_eq!(parent(&db, "/project", Some("old")).unwrap().0, "old");
        assert!(parent(&db, "/other", None).is_none());
        assert!(parent(&db, "/project", Some("worker")).is_none());
        let a = agent(&db, 42, "/project", None, 1000).unwrap();
        assert_eq!(a.kind, "kilo");
        assert_eq!(a.activity.as_deref(), Some("new"));
        assert_eq!(a.status, "waiting");
        assert_eq!(a.id, "kilo:42");
    }
    #[test]
    fn a_cancelled_turn_is_not_an_error_but_a_real_failure_is() {
        let db = db();
        session(&db, "root", None, 1000);
        message(
            &db,
            "m",
            "root",
            1000_000,
            serde_json::json!({"role":"assistant","error":{"name":"MessageAbortedError"},"time":{"completed":1000_000}}),
        );
        assert_eq!(status(&db, "root", 1010).unwrap().0, "waiting");
        db.execute(
            "UPDATE message SET data=json_set(data,'$.error.name','APIError')",
            [],
        )
        .unwrap();
        assert_eq!(status(&db, "root", 1010).unwrap().0, "error");
    }
    #[test]
    fn a_stalled_tool_in_the_latest_message_is_found() {
        let db = db();
        session(&db, "root", None, 1000);
        message(&db, "m1", "root", 1000, serde_json::json!({}));
        message(&db, "m2", "root", 2000, serde_json::json!({}));
        db.execute("DROP TABLE IF EXISTS part", []).unwrap();
        db.execute_batch("CREATE TABLE part(id TEXT,message_id TEXT,session_id TEXT,time_created INTEGER,time_updated INTEGER,data TEXT)").unwrap();
        let add = |id: &str, m: &str, at: u64, status: &str| {
            let data = serde_json::json!({"type":"tool","tool":"bash","state":{"status":status,"time":{"start":at}}});
            db.execute(
                "INSERT INTO part VALUES(?1,?2,'root',?3,?3,?4)",
                rusqlite::params![id, m, at, data.to_string()],
            )
            .unwrap();
        };
        // Starting a background process can prompt; its other actions just wait.
        let background = |id: &str, at: u64, action: &str| {
            let data = serde_json::json!({"type":"tool","tool":"background_process","state":{"status":"running","input":{"action":action},"time":{"start":at}}});
            db.execute(
                "INSERT INTO part VALUES(?1,'m2','root',?2,?2,?3)",
                rusqlite::params![id, at, data.to_string()],
            )
            .unwrap();
        };
        background("tail", 4000, "tail");
        assert_eq!(unfinished_tool(&db, "root"), None);
        background("start", 5000, "start");
        assert_eq!(
            unfinished_tool(&db, "root"),
            Some(("background_process".into(), 5))
        );
        db.execute("DELETE FROM part", []).unwrap();
        // A tool left unfinished in an older message doesn't count.
        add("old", "m1", 1000, "running");
        assert_eq!(unfinished_tool(&db, "root"), None);
        add("done", "m2", 2000, "completed");
        assert_eq!(unfinished_tool(&db, "root"), None);
        add("now", "m2", 3000, "running");
        assert_eq!(unfinished_tool(&db, "root"), Some(("bash".into(), 3)));
    }
    #[test]
    fn terminals_pair_with_the_most_active_sessions_in_creation_order() {
        let db = db();
        session(&db, "old", None, 1000);
        session(&db, "first", None, 2000);
        session(&db, "second", None, 3000);
        // "first" has no fresh session timestamp, only fresh messages.
        message(&db, "a", "first", 9000, serde_json::json!({}));
        message(&db, "b", "second", 8000, serde_json::json!({}));
        message(&db, "c", "old", 1500, serde_json::json!({}));
        assert_eq!(active_sessions(&db, "/project", 2), vec!["first", "second"]);
        assert_eq!(
            active_sessions(&db, "/project", 3),
            vec!["old", "first", "second"]
        );
        assert!(active_sessions(&db, "/other", 2).is_empty());
    }
    #[test]
    fn todo_list_counts_open_and_finished_items_of_that_session_only() {
        let db = db();
        // Older databases have no todo table: no progress, and no failure.
        assert_eq!(tasks(&db, "root"), None);
        db.execute_batch("CREATE TABLE todo(session_id TEXT,status TEXT)")
            .unwrap();
        assert_eq!(tasks(&db, "root"), None);
        for (session, status) in [
            ("root", "completed"),
            ("root", "in_progress"),
            ("root", "pending"),
            ("root", "cancelled"),
            ("other", "completed"),
        ] {
            db.execute("INSERT INTO todo VALUES(?1,?2)", [session, status])
                .unwrap();
        }
        assert_eq!(
            tasks(&db, "root"),
            Some(TaskProgress {
                completed: 1,
                total: 3
            })
        );
        // A fully finished list stays in the table but is no longer shown.
        assert_eq!(tasks(&db, "other"), None);
    }
    #[test]
    fn lifecycle_and_confirmed_dependencies_use_current_parent_only() {
        let db = db();
        session(&db, "root", None, 1000);
        message(
            &db,
            "m",
            "root",
            1000000,
            json!({"role":"assistant","time":{"completed":1000000},"finish":"tool-calls"}),
        );
        assert_eq!(status(&db, "root", 1120).unwrap().0, "working");
        part(
            &db,
            "p",
            "m",
            "root",
            1000000,
            json!({"type":"tool","callID":"call","tool":"task","state":{"status":"running","input":{"subagent_type":"explore"},"time":{"start":1000000}}}),
        );
        assert_eq!(
            status(&db, "root", 1120).unwrap(),
            ("waiting_task".into(), 120)
        );
        db.execute(
            "UPDATE part SET data=json_set(data,'$.state.status','completed')",
            [],
        )
        .unwrap();
        assert_eq!(status(&db, "root", 1120).unwrap().0, "working");
        db.execute(
            "UPDATE message SET data=json_set(data,'$.finish','stop')",
            [],
        )
        .unwrap();
        assert_eq!(status(&db, "root", 1120).unwrap().0, "waiting");
        assert_eq!(status(&db, "root", 8200).unwrap().0, "idle");
        message(
            &db,
            "err",
            "root",
            1100000,
            json!({"role":"assistant","error":{"name":"APIError"}}),
        );
        assert_eq!(status(&db, "root", 1120).unwrap().0, "error");
        message(&db, "user", "root", 1110000, json!({"role":"user"}));
        assert_eq!(status(&db, "root", 1120).unwrap().0, "working");
    }
    #[test]
    fn workers_are_direct_bounded_and_recent_completions_expire() {
        let db = db();
        session(&db, "root", None, 1000);
        for (id, parent, finished) in [
            ("active", "root", false),
            ("done", "root", true),
            ("other", "unrelated", false),
            ("nested", "active", false),
        ] {
            session(&db, id, Some(parent), 1000);
            message(
                &db,
                id,
                id,
                1000000,
                if finished {
                    json!({"role":"assistant","time":{"completed":1000000},"finish":"stop"})
                } else {
                    json!({"role":"assistant"})
                },
            );
        }
        assert_eq!(children(&db, "root", 1120).len(), 2);
        assert_eq!(children(&db, "root", 1121).len(), 1);
        for i in 0..7 {
            let id = format!("worker-{i}");
            session(&db, &id, Some("root"), 1100);
            message(&db, &id, &id, 1100000, json!({"role":"assistant"}));
        }
        let children = children(&db, "root", 1120);
        assert_eq!(children.len(), 6);
        assert!(children.iter().all(|c| c.status == "working"));
    }
    #[test]
    fn logs_and_context_exclude_reasoning_tool_payloads_and_unrelated_workers() {
        let db = db();
        session(&db, "root", None, 1000);
        session(&db, "child", Some("root"), 1000);
        session(&db, "other", Some("elsewhere"), 1000);
        message(&db, "u", "child", 1000000, json!({"role":"user"}));
        message(
            &db,
            "a",
            "child",
            1001000,
            json!({"role":"assistant","tokens":{"input":100,"cache":{"read":50,"write":10}}}),
        );
        part(
            &db,
            "p1",
            "u",
            "child",
            1000000,
            json!({"type":"text","text":"Inspect the repo"}),
        );
        part(
            &db,
            "p2",
            "a",
            "child",
            1001000,
            json!({"type":"reasoning","text":"private thought"}),
        );
        part(
            &db,
            "p3",
            "a",
            "child",
            1001001,
            json!({"type":"tool","tool":"bash","state":{"input":{"command":"private command"},"output":"private output"}}),
        );
        part(
            &db,
            "p4",
            "a",
            "child",
            1001002,
            json!({"type":"text","text":"It’s done"}),
        );
        assert_eq!(context(&db, "child").unwrap().used, 160);
        assert!(context(&db, "child").unwrap().limit.is_none());
        assert_eq!(
            entries(&db, "root", Some("child")).unwrap(),
            vec![
                (Role::User, "Inspect the repo".into()),
                (Role::Tool, "bash".into()),
                (Role::Assistant, "It's done".into())
            ]
        );
        assert!(entries(&db, "root", Some("other")).is_none());
        assert!(entries(&db, "root", Some("../child")).is_none());
        assert!(parent(&Connection::open_in_memory().unwrap(), "/project", None).is_none());
    }
}
