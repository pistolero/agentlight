//! Context and plan-limit figures from local agent files. Reads only token
//! counts and limits: never message content, credentials or the network.
use crate::{env_path, home, open_db, Ctx, PlanLimit};
use serde_json::Value;
use std::{
    collections::HashMap,
    fs,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::SystemTime,
};

const TAILS: [u64; 2] = [64 * 1024, 256 * 1024];

fn ctx(used: u64, limit: Option<u64>) -> Option<Ctx> {
    (used > 0).then_some(Ctx {
        used,
        // A percent above 100 would be wrong, so show tokens instead.
        limit: limit.filter(|l| *l > 0 && used <= *l),
    })
}

/// Complete JSON lines from the end of a file, oldest first. A cut-off first
/// line is skipped, and lines without `needle` are not parsed at all.
pub(crate) fn tail_values(path: &Path, size: u64, needle: &[u8]) -> Option<(Vec<Value>, bool)> {
    let mut f = fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let start = len.saturating_sub(size);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    f.take(size).read_to_end(&mut bytes).ok()?;
    let values = bytes
        .split(|b| *b == b'\n')
        .skip(usize::from(start > 0))
        .filter(|line| line.windows(needle.len()).any(|w| w == needle))
        .filter_map(|line| serde_json::from_slice(line).ok())
        .collect();
    Some((values, start == 0))
}

// ---------------------------------------------------------------- Claude
fn claude_reading(values: &[Value]) -> Option<(u64, String)> {
    values.iter().rev().find_map(|v| {
        if v["type"] != "assistant" || v["isSidechain"] == true {
            return None;
        }
        let usage = &v["message"]["usage"];
        let field = |name: &str| usage[name].as_u64().unwrap_or(0);
        let used = field("input_tokens")
            + field("cache_read_input_tokens")
            + field("cache_creation_input_tokens");
        // Synthetic error messages carry an all-zero usage block.
        (used > 0).then(|| {
            let model = v["message"]["model"].as_str().unwrap_or("");
            (used, model.to_string())
        })
    })
}
/// Input size of a model, from Claude Code's cached model catalog.
fn claude_model_limit(root: &Path, model: &str) -> Option<u64> {
    fs::read_dir(root.join("cache/model-catalog"))
        .ok()?
        .flatten()
        .find_map(|e| {
            let v: Value = serde_json::from_slice(&fs::read(e.path()).ok()?).ok()?;
            catalog_limit(&v, model)
        })
}
fn catalog_limit(catalog: &Value, model: &str) -> Option<u64> {
    catalog["catalog"]["config"]["models"]
        .as_array()?
        .iter()
        .find(|m| m["id"] == model)?["runtime"]["max_input_tokens"]
        .as_u64()
}
fn stamp(path: &Path) -> Option<(u64, SystemTime)> {
    let m = fs::metadata(path).ok()?;
    Some((m.len(), m.modified().ok()?))
}
type Cached<T> = Mutex<HashMap<PathBuf, ((u64, SystemTime), T)>>;
/// Re-read a file only when it has changed: this runs every 2 s per connection.
fn cached<T: Clone>(cache: &Cached<T>, path: &Path, read: impl Fn() -> T) -> Option<T> {
    let current = stamp(path)?;
    let mut map = cache.lock().unwrap();
    if let Some((seen, value)) = map.get(path) {
        if *seen == current {
            return Some(value.clone());
        }
    }
    let value = read();
    map.insert(path.to_path_buf(), (current, value.clone()));
    Some(value)
}
pub fn claude_context(session: &str) -> Option<Ctx> {
    static CACHE: OnceLock<Cached<Option<Ctx>>> = OnceLock::new();
    let root = env_path("CLAUDE_CONFIG_DIR", home().join(".claude"));
    let relative = format!("{session}.jsonl");
    let path = crate::logs::claude_file(&root, Path::new(&relative))?;
    cached(CACHE.get_or_init(Default::default), &path, || {
        let reading = TAILS.iter().find_map(|size| {
            let (values, whole) = tail_values(&path, *size, b"\"usage\"")?;
            claude_reading(&values).map(Some).or(whole.then_some(None))
        })??;
        ctx(reading.0, claude_model_limit(&root, &reading.1))
    })?
}

// ---------------------------------------------------------------- Codex
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CodexUsage {
    pub ctx: Option<Ctx>,
    /// Plan windows with their reset time, and when they were recorded.
    pub windows: Vec<(PlanLimit, u64)>,
    pub at: String,
}
fn window_label(minutes: u64) -> String {
    if minutes >= 1440 {
        format!("{}d", (minutes + 720) / 1440)
    } else {
        format!("{}h", (minutes + 30) / 60)
    }
}
fn codex_reading(values: &[Value]) -> CodexUsage {
    let mut usage = CodexUsage::default();
    for v in values {
        let p = &v["payload"];
        if p["type"] != "token_count" {
            continue;
        }
        let info = &p["info"];
        if let Some(used) = info["last_token_usage"]["input_tokens"].as_u64() {
            usage.ctx = ctx(used, info["model_context_window"].as_u64());
        }
        let limits = &p["rate_limits"];
        let windows: Vec<_> = [&limits["primary"], &limits["secondary"]]
            .into_iter()
            .filter_map(|w| {
                let limit = PlanLimit {
                    l: window_label(w["window_minutes"].as_u64()?),
                    p: w["used_percent"].as_f64()?.round().clamp(0.0, 100.0) as u8,
                };
                Some((limit, w["resets_at"].as_u64()?))
            })
            .collect();
        if !windows.is_empty() {
            usage.windows = windows;
            usage.at = v["timestamp"].as_str().unwrap_or("").to_string();
        }
    }
    usage
}
pub fn codex_usage(rollout: &Path) -> Option<CodexUsage> {
    static CACHE: OnceLock<Cached<CodexUsage>> = OnceLock::new();
    cached(CACHE.get_or_init(Default::default), rollout, || {
        for size in TAILS {
            let Some((values, whole)) = tail_values(rollout, size, b"\"token_count\"") else {
                break;
            };
            let usage = codex_reading(&values);
            if whole || usage.ctx.is_some() {
                return usage;
            }
        }
        CodexUsage::default()
    })
}
/// Plan windows that haven't reset yet: an expired percent would be misleading.
pub fn current_windows(usage: &CodexUsage, at: u64) -> Vec<PlanLimit> {
    usage
        .windows
        .iter()
        .filter(|(_, resets)| *resets > at)
        .map(|(limit, _)| limit.clone())
        .take(2)
        .collect()
}

// ---------------------------------------------------------------- OpenCode
/// OpenCode keeps token counts but no context size, so this returns tokens only.
pub fn opencode_context(session: &str) -> Option<Ctx> {
    let db = open_db(
        &env_path("XDG_DATA_HOME", home().join(".local/share")).join("opencode/opencode.db"),
    )?;
    let used: i64 = db
        .query_row(
            "SELECT coalesce(json_extract(data,'$.tokens.input'),0)+coalesce(json_extract(data,'$.tokens.cache.read'),0)+coalesce(json_extract(data,'$.tokens.cache.write'),0) AS used FROM session_message WHERE session_id=?1 AND type='assistant' AND used>0 ORDER BY seq DESC LIMIT 1",
            [session],
            |r| r.get(0),
        )
        .ok()?;
    ctx(used.max(0) as u64, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn temp(name: &str, body: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("agentlight-usage-{}-{name}", std::process::id()));
        fs::write(&path, body).unwrap();
        path
    }
    #[test]
    fn claude_uses_newest_real_main_conversation_usage() {
        let values = vec![
            json!({"type":"assistant","message":{"model":"m","usage":{"input_tokens":10,"cache_read_input_tokens":20,"cache_creation_input_tokens":5}}}),
            json!({"type":"assistant","isSidechain":true,"message":{"model":"sub","usage":{"input_tokens":999}}}),
            json!({"type":"assistant","message":{"model":"<synthetic>","usage":{"input_tokens":0}}}),
        ];
        assert_eq!(claude_reading(&values), Some((35, "m".into())));
    }
    #[test]
    fn catalog_gives_the_size_of_the_exact_model_only() {
        let catalog = json!({"catalog":{"config":{"models":[{"id":"a","runtime":{"max_input_tokens":200000}}]}}});
        assert_eq!(catalog_limit(&catalog, "a"), Some(200000));
        assert_eq!(catalog_limit(&catalog, "b"), None);
    }
    #[test]
    fn percent_is_never_above_100() {
        assert_eq!(ctx(5, Some(4)).unwrap().limit, None);
        assert_eq!(ctx(4, Some(4)).unwrap().limit, Some(4));
        assert_eq!(ctx(4, None).unwrap().limit, None);
        assert_eq!(ctx(0, Some(4)), None);
    }
    #[test]
    fn tail_skips_a_cut_off_first_line() {
        let path = temp(
            "tail",
            "{\"token_count\":1,\"pad\":\"xxxxxxxx\"}\n{\"token_count\":2}\n",
        );
        let (values, whole) = tail_values(&path, 20, b"token_count").unwrap();
        assert_eq!((values, whole), (vec![json!({"token_count":2})], false));
        fs::remove_file(path).unwrap();
    }
    #[test]
    fn codex_reads_context_and_labels_windows() {
        let values = vec![
            json!({"timestamp":"t1","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":100},"model_context_window":400},"rate_limits":{"primary":{"used_percent":41.6,"window_minutes":300,"resets_at":50},"secondary":{"used_percent":3.0,"window_minutes":10080,"resets_at":90}}}}),
            // Limits missing from a later event keep the last known ones.
            json!({"timestamp":"t2","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":120},"model_context_window":400},"rate_limits":null}}),
        ];
        let usage = codex_reading(&values);
        assert_eq!(
            usage.ctx,
            Some(Ctx {
                used: 120,
                limit: Some(400)
            })
        );
        assert_eq!(usage.at, "t1");
        let names = |at| {
            current_windows(&usage, at)
                .iter()
                .map(|w| (w.l.clone(), w.p))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(10),
            vec![("5h".to_string(), 42), ("7d".to_string(), 3)]
        );
        // A window past its reset is dropped rather than shown stale.
        assert_eq!(names(60), vec![("7d".to_string(), 3)]);
        assert!(names(100).is_empty());
    }
    #[test]
    fn cache_rereads_only_when_the_file_changes() {
        let cache: Cached<u32> = Default::default();
        let path = temp("cache", "a");
        let calls = std::cell::Cell::new(0);
        let read = || {
            calls.set(calls.get() + 1);
            calls.get()
        };
        assert_eq!(cached(&cache, &path, read), Some(1));
        assert_eq!(cached(&cache, &path, read), Some(1));
        fs::write(&path, "changed").unwrap();
        assert_eq!(cached(&cache, &path, read), Some(2));
        fs::remove_file(path).unwrap();
    }
}
