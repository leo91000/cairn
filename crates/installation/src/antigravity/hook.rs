//! Cairn's `PreToolUse` hook for Antigravity. `agy` runs it before every tool call; a
//! hook that fails, times out or answers anything but an allow decision refuses the
//! tool (measured on 1.5.14), so the policy below is the turn's boundary.
use crate::{
    error::{Error, Result},
    skills::atomic_write,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, io::Read, path::Path};

/// The policy file, beside the native state of one execution.
pub(crate) const POLICY: &str = "cairn-hook-policy.json";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Mode {
    /// An ordinary turn: every native tool except its own questions.
    #[default]
    Turn,
    /// A plan turn: reading and searching only. `agy` has no read-only sandbox.
    Plan,
    /// A side question: no tool at all.
    Side,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct Policy {
    pub mode: Mode,
    /// Configured MCP servers, each with its allowed tools, or `None` for all of them.
    pub mcp: BTreeMap<String, Option<Vec<String>>>,
}

/// Tools that only read the workspace, the conversation or MCP resources.
const READ_ONLY: &[&str] = &[
    "read_file",
    "view_file",
    "list_dir",
    "find_by_name",
    "grep_search",
    "list_resources",
    "read_resource",
    "list_permissions",
    "finish",
];

const NATIVE_QUESTION: &str = "Antigravity's own question tool is unavailable in Cairn. Use the cairn_workspace ask_questions tool to ask the user.";
const PLAN_REFUSAL: &str = "This is a Cairn plan turn: only reading and searching are allowed. Propose the plan; nothing may be modified or run.";

const SIDE_REFUSAL: &str =
    "Tools are unavailable while answering a side question. Answer from the conversation.";

const MCP_REFUSAL: &str = "This MCP tool is outside the agent's access policy.";

impl Policy {
    pub fn decide(&self, call: &Value) -> std::result::Result<(), &'static str> {
        let name = call["name"].as_str().unwrap_or_default();
        if self.mode == Mode::Side {
            return Err(SIDE_REFUSAL);
        }
        if name == "call_mcp_tool" {
            let server = call["args"]["ServerName"].as_str().unwrap_or_default();
            let tool = call["args"]["ToolName"].as_str().unwrap_or_default();
            return match self.mcp.get(server) {
                Some(None) => Ok(()),
                Some(Some(tools)) if tools.iter().any(|t| t == tool) => Ok(()),
                _ => Err(MCP_REFUSAL),
            };
        }
        match self.mode {
            Mode::Turn if name == "ask_question" => Err(NATIVE_QUESTION),
            Mode::Turn => Ok(()),
            Mode::Plan if READ_ONLY.contains(&name) => Ok(()),
            Mode::Plan | Mode::Side => Err(PLAN_REFUSAL),
        }
    }

    pub async fn save(&self, home: &Path) -> Result<()> {
        atomic_write(&home.join(POLICY), &serde_json::to_vec(self)?).await
    }
}

/// The native hook configuration that runs this executable as the hook.
pub(crate) fn hooks_config(home: &Path) -> Result<Value> {
    let executable = std::env::current_exe()?;
    let quote = |value: &str| format!("'{}'", value.replace('\'', "'\\''"));
    let command = format!(
        "{} antigravity-hook {}",
        quote(&executable.to_string_lossy()),
        quote(&home.join(POLICY).to_string_lossy()),
    );
    Ok(json!({
        "cairn": {
            "PreToolUse": [{
                "matcher": ".*",
                "hooks": [{ "type": "command", "command": command, "timeout": 10 }],
            }],
        },
    }))
}

/// The `antigravity-hook <policy>` entry point: reads the tool call on stdin and
/// prints the decision. Any error leaves the output empty of an allow decision.
pub fn main(args: &[String]) -> Result<i32> {
    let path = args
        .first()
        .ok_or_else(|| Error::bad("Missing Antigravity hook policy."))?;
    let policy: Policy = serde_json::from_slice(&std::fs::read(path)?)?;
    let mut input = Vec::new();
    std::io::stdin().take(4_000_000).read_to_end(&mut input)?;
    let event: Value = serde_json::from_slice(&input)?;
    let decision = match policy.decide(&event["toolCall"]) {
        Ok(()) => json!({ "decision": "allow" }),
        Err(reason) => json!({ "decision": "deny", "reason": reason }),
    };
    println!("{decision}");
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str) -> Value {
        json!({ "name": name, "args": {} })
    }

    fn mcp(server: &str, tool: &str) -> Value {
        json!({ "name": "call_mcp_tool", "args": { "ServerName": server, "ToolName": tool, "Arguments": {} } })
    }

    fn policy(mode: Mode) -> Policy {
        Policy {
            mode,
            mcp: BTreeMap::from([
                ("cairn_workspace".into(), None),
                ("files".into(), Some(vec!["read".into()])),
            ]),
        }
    }

    #[test]
    fn ordinary_turns_allow_native_tools_but_not_native_questions() {
        let turn = policy(Mode::Turn);
        assert!(turn.decide(&call("run_command")).is_ok());
        assert!(turn.decide(&call("write_file")).is_ok());
        assert!(turn.decide(&call("invoke_subagent")).is_ok());
        assert!(turn.decide(&call("ask_question")).is_err());
    }

    #[test]
    fn mcp_calls_follow_the_configured_allowlists_in_every_mode() {
        for mode in [Mode::Turn, Mode::Plan] {
            let policy = policy(mode);
            assert!(policy.decide(&mcp("cairn_workspace", "anything")).is_ok());
            assert!(policy.decide(&mcp("files", "read")).is_ok());
            assert!(policy.decide(&mcp("files", "write")).is_err());
            assert!(policy.decide(&mcp("unknown", "read")).is_err());
        }
    }

    #[test]
    fn plan_turns_allow_only_reading_and_refuse_unknown_tools() {
        let plan = policy(Mode::Plan);
        for allowed in [
            "read_file",
            "view_file",
            "list_dir",
            "grep_search",
            "find_by_name",
        ] {
            assert!(plan.decide(&call(allowed)).is_ok(), "{allowed}");
        }
        for refused in [
            "run_command",
            "write_file",
            "edit_file",
            "invoke_subagent",
            "define_subagent",
            "schedule",
            "run_workflow",
            "swarm",
            "send_message",
            "manage_task",
            "search_web",
            "ask_question",
            "a_future_tool",
            "",
        ] {
            assert!(plan.decide(&call(refused)).is_err(), "{refused}");
        }
    }

    #[test]
    fn side_questions_refuse_every_tool() {
        let side = policy(Mode::Side);
        for name in ["read_file", "run_command", "call_mcp_tool", "finish"] {
            assert!(side.decide(&call(name)).is_err(), "{name}");
        }
        assert!(side.decide(&mcp("cairn_workspace", "list_nodes")).is_err());
    }
}
