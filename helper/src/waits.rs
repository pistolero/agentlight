//! Confirmed blocking dependencies, shared by the provider adapters.
use serde_json::Value;

#[derive(Default, Debug)]
pub struct Evidence {
    calls: Vec<(String, &'static str, u64)>,
}
impl Evidence {
    pub fn call(&mut self, id: &str, name: &str, args: &Value, at: u64) {
        let name = name.rsplit('.').next().unwrap_or(name);
        let has = |key: &str| args[key].as_str().is_some_and(|s| !s.is_empty());
        let list = |key: &str| {
            args[key].as_array().is_some_and(|a| {
                !a.is_empty()
                    && a.iter().all(|v| {
                        v.as_str().is_some_and(|s| !s.is_empty())
                            || v["threadId"].as_str().is_some_and(|s| !s.is_empty())
                            || v["target"].as_str().is_some_and(|s| !s.is_empty())
                    })
            })
        };
        let reason = match name {
            "request_user_input" | "request_permissions" | "AskUserQuestion" | "question" => {
                "waiting"
            }
            "TaskOutput"
                if has("task_id") && (args["block"].is_null() || args["block"] == true) =>
            {
                "waiting_task"
            }
            "background_output" if has("task_id") && args["block"] == true => "waiting_task",
            "task"
                if has("subagent_type")
                    && args["run_in_background"] != true
                    && args["block"] != false =>
            {
                "waiting_task"
            }
            "wait_agent" if list("targets") || has("target") => "waiting_task",
            "wait" if has("cell_id") && args["terminate"] != true => "waiting_task",
            "wait_threads" if list("targets") && args["timeoutMs"] != 0 => "waiting_task",
            "write_stdin"
                if args["session_id"].is_number()
                    && args["chars"].as_str().unwrap_or("").is_empty()
                    && args["yield_time_ms"] != 0 =>
            {
                "waiting_task"
            }
            _ => return,
        };
        self.calls.retain(|(key, ..)| key != id);
        self.calls.push((id.into(), reason, at));
    }
    pub fn result(&mut self, id: &str) {
        self.calls.retain(|(key, ..)| key != id);
    }
    pub fn clear(&mut self) {
        self.calls.clear();
    }
    pub fn resolve(&self, base: (&str, u64), at: u64) -> (String, u64) {
        let evidence = self
            .calls
            .iter()
            .rev()
            .find(|(_, reason, _)| *reason == "waiting")
            .or_else(|| self.calls.last());
        evidence
            .map(|(_, reason, since)| ((*reason).into(), at.saturating_sub(*since)))
            .unwrap_or_else(|| (base.0.into(), base.1))
    }
}
fn timestamp(v: &Value) -> Option<u64> {
    Some(
        chrono::DateTime::parse_from_rfc3339(v["timestamp"].as_str()?)
            .ok()?
            .timestamp()
            .max(0) as u64,
    )
}
pub fn codex(values: &[Value]) -> Evidence {
    let mut evidence = Evidence::default();
    for v in values {
        let p = &v["payload"];
        match p["type"].as_str().unwrap_or("") {
            "task_started" | "task_complete" | "turn_aborted" => evidence.clear(),
            "function_call" => {
                if let (Some(id), Some(name), Some(at)) =
                    (p["call_id"].as_str(), p["name"].as_str(), timestamp(v))
                {
                    let args = p["arguments"]
                        .as_str()
                        .and_then(|s| serde_json::from_str(s).ok())
                        .unwrap_or_else(|| p["arguments"].clone());
                    evidence.call(id, name, &args, at);
                }
            }
            "function_call_output" | "custom_tool_call_output" => {
                if let Some(id) = p["call_id"].as_str() {
                    evidence.result(id);
                }
            }
            _ => (),
        }
    }
    evidence
}
pub fn claude(values: &[Value]) -> Evidence {
    let mut evidence = Evidence::default();
    for v in values {
        if v["isSidechain"] == true {
            continue;
        }
        let kind = v["type"].as_str().unwrap_or("");
        if kind != "assistant" && kind != "user" {
            continue;
        }
        let parts = v["message"]["content"].as_array();
        if kind == "user" && !parts.is_some_and(|p| p.iter().any(|b| b["type"] == "tool_result")) {
            evidence.clear();
        }
        if let Some(parts) = parts {
            for b in parts {
                if b["type"] == "tool_use" {
                    if let (Some(id), Some(name), Some(at)) =
                        (b["id"].as_str(), b["name"].as_str(), timestamp(v))
                    {
                        evidence.call(id, name, &b["input"], at);
                    }
                } else if b["type"] == "tool_result" {
                    if let Some(id) = b["tool_use_id"].as_str() {
                        evidence.result(id);
                    }
                }
            }
        }
        if v["message"]["stop_reason"] == "end_turn" {
            evidence.clear();
        }
    }
    evidence
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn only_blocking_dependencies_change_status() {
        for (name, args) in [
            ("TaskOutput", json!({"task_id":"task","block":true})),
            ("functions.wait", json!({"cell_id":"cell"})),
            (
                "write_stdin",
                json!({"session_id":12,"chars":"","yield_time_ms":1000}),
            ),
            ("wait_agent", json!({"targets":["worker"]})),
            ("task", json!({"subagent_type":"explore"})),
            ("background_output", json!({"task_id":"task","block":true})),
        ] {
            let mut e = Evidence::default();
            e.call("call", name, &args, 100);
            assert_eq!(
                e.resolve(("working", 0), 150),
                ("waiting_task".into(), 50),
                "{name}"
            );
            e.result("call");
            assert_eq!(e.resolve(("working", 0), 150).0, "working");
        }
        for (name, args) in [
            (
                "Bash",
                json!({"command":"agentlight --wifi","run_in_background":true}),
            ),
            ("TaskOutput", json!({"task_id":"task","block":false})),
            ("TaskOutput", json!({})),
            ("wait_agent", json!({"targets":[{}]})),
            (
                "task",
                json!({"subagent_type":"explore","run_in_background":true}),
            ),
            ("wait_agent", json!({"targets":[]})),
            ("wait", json!({"cell_id":"cell","terminate":true})),
            ("write_stdin", json!({"session_id":12,"chars":"hello"})),
            ("write_stdin", json!({"session_id":12,"yield_time_ms":0})),
            ("wait_threads", json!({"targets":["worker"],"timeoutMs":0})),
        ] {
            let mut e = Evidence::default();
            e.call("call", name, &args, 100);
            assert_eq!(
                e.resolve(("waiting", 5), 150),
                ("waiting".into(), 5),
                "{name}"
            );
        }
    }
    #[test]
    fn human_requests_win_over_background_dependencies() {
        let mut e = Evidence::default();
        e.call("ask", "request_user_input", &json!({}), 100);
        e.call("wait", "TaskOutput", &json!({"task_id":"task"}), 110);
        assert_eq!(e.resolve(("working", 0), 150), ("waiting".into(), 50));
        e.result("ask");
        assert_eq!(e.resolve(("working", 0), 150).0, "waiting_task");
    }
    #[test]
    fn codex_matches_results_and_discards_previous_turns() {
        let call = json!({"timestamp":"1970-01-01T00:01:40Z","payload":{"type":"function_call","call_id":"a","name":"wait_agent","arguments":"{\"targets\":[\"worker\"]}"}});
        let result = json!({"payload":{"type":"function_call_output","call_id":"a"}});
        assert_eq!(
            codex(&[call.clone()]).resolve(("working", 0), 150).0,
            "waiting_task"
        );
        for end in [
            result,
            json!({"payload":{"type":"task_complete"}}),
            json!({"payload":{"type":"turn_aborted"}}),
            json!({"payload":{"type":"task_started"}}),
        ] {
            assert_eq!(
                codex(&[call.clone(), end]).resolve(("working", 0), 150).0,
                "working"
            );
        }
    }
    #[test]
    fn claude_launch_is_not_a_wait_and_results_clear_dependencies() {
        let call = |name: &str, input: Value| json!({"timestamp":"1970-01-01T00:01:40Z","type":"assistant","message":{"content":[{"type":"tool_use","id":"a","name":name,"input":input}],"stop_reason":"tool_use"}});
        let launch = call("Bash", json!({"run_in_background":true}));
        assert_eq!(claude(&[launch]).resolve(("waiting", 0), 150).0, "waiting");
        let wait = call("TaskOutput", json!({"task_id":"test","block":true}));
        assert_eq!(
            claude(&[wait.clone()]).resolve(("working", 0), 150).0,
            "waiting_task"
        );
        for end in [
            json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"a"}]}}),
            json!({"type":"assistant","message":{"stop_reason":"end_turn"}}),
            json!({"type":"user","message":{"content":"new request"}}),
        ] {
            assert_eq!(
                claude(&[wait.clone(), end]).resolve(("working", 0), 150).0,
                "working"
            );
        }
    }
}
