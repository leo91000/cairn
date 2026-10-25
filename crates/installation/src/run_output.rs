use crate::{service::policy, validation::text};
use serde_json::{Value, json};
use std::{path::Path, sync::LazyLock};

pub const ACTIVITY_DETAIL_LIMIT: usize = 16 * 1024;
const NOTICE: &str = "Details shortened: arguments and output were omitted.";

/// Serialize UI activity before it crosses the guest transport. The native
/// session retains full tool results; the manager already stores these previews.
pub fn event_line(event: &Value) -> Vec<u8> {
    let mut bytes = event.to_string().into_bytes();
    let activity = text(event, "type").starts_with("item.")
        && event["item"].is_object()
        && event["item"]["type"] != "agent_message";
    if activity && bytes.len() > ACTIVITY_DETAIL_LIMIT {
        bytes = compact_activity_preview(event, None)
            .to_string()
            .into_bytes();
    }
    bytes.push(b'\n');
    bytes
}

/// A controller can fail after writing only part of a guest stdout line.
/// Establish a fresh line so the failure remains independently parseable.
pub fn controller_failure_line(message: &str) -> Vec<u8> {
    let failure = json!({
        "type": "runner.failed",
        "error": { "message": message },
    });
    let mut bytes = vec![b'\n'];
    bytes.extend(event_line(&failure));
    bytes
}

static TOKEN: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\b(?:ops_[\w.-]{15,}|sk-[\w-]{12,})\b").unwrap());

/// Token formats active plugins declare (`secretPatterns`), masked like the above.
static PLUGIN_TOKENS: LazyLock<std::sync::RwLock<Vec<regex::Regex>>> =
    LazyLock::new(std::sync::RwLock::default);

/// Replaces the token formats of active plugins.
pub fn set_plugin_secret_patterns(patterns: Vec<regex::Regex>) {
    *PLUGIN_TOKENS.write().unwrap() = patterns;
}

static BEARER: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(?i)(Bearer\s+)[\w.~-]+").unwrap());

static CREDENTIAL: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r#"(?i)("?(?:access_token|refresh_token|id_token|OPENAI_API_KEY|CODEX_API_KEY|ANTHROPIC_API_KEY|ANTHROPIC_AUTH_TOKEN|CLAUDE_CODE_OAUTH_TOKEN|accessToken|refreshToken|OP_SERVICE_ACCOUNT_TOKEN)"?\s*[:=]\s*"?)[^"\s,}]+"#).unwrap()
});

// Codex and Claude Code subscription limits. Claude Code reports them as the turn's result.
static EXHAUSTED: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?i)^(?:you['’]ve hit your (?:usage |session |weekly |(?:individual )?spend )?limit|you have hit your (?:usage )?limit|usage limit (?:has been )?(?:reached|exceeded)|claude ai usage limit reached|(?:5-hour|session|weekly|opus) limit reached)\b").unwrap()
});

#[derive(Default)]
enum TerminalState {
    #[default]
    Text,
    Escape,
    Csi,
    Osc,
    OscEscape,
}

/// Removes CSI, OSC and two-byte escapes without buffering control-sequence bodies.
#[derive(Default)]
pub(crate) struct TerminalStyling {
    state: TerminalState,
}

impl TerminalStyling {
    pub(crate) fn push(&mut self, bytes: &[u8], plain: &mut Vec<u8>) {
        use TerminalState::*;

        for &byte in bytes {
            match self.state {
                Text if byte == 0x1b => self.state = Escape,
                Text => plain.push(byte),
                Escape => match byte {
                    b'[' => self.state = Csi,
                    b']' => self.state = Osc,
                    0x1b => {}
                    b'@'..=b'Z' | b'\\'..=b'_' => self.state = Text,
                    _ => {
                        plain.extend_from_slice(&[0x1b, byte]);
                        self.state = Text;
                    }
                },
                Csi => match byte {
                    b'@'..=b'~' => self.state = Text,
                    0x1b => self.state = Escape,
                    b'\r' | b'\n' => {
                        plain.push(byte);
                        self.state = Text;
                    }
                    _ => {}
                },
                Osc => match byte {
                    0x07 => self.state = Text,
                    0x1b => self.state = OscEscape,
                    _ => {}
                },
                OscEscape => match byte {
                    b'\\' | 0x07 => self.state = Text,
                    0x1b => {}
                    _ => self.state = Osc,
                },
            }
        }
    }
}

/// Removes terminal styling that agent processes write to stderr.
pub fn strip_terminal_styling(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains('\x1b') {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut plain = Vec::with_capacity(text.len());
    TerminalStyling::default().push(text.as_bytes(), &mut plain);
    std::borrow::Cow::Owned(String::from_utf8_lossy(&plain).into_owned())
}

pub fn redact(text: &str, secrets: &[String]) -> String {
    let mut text = TOKEN.replace_all(text, "[redacted]").into_owned();
    for pattern in PLUGIN_TOKENS.read().unwrap().iter() {
        text = pattern.replace_all(&text, "[redacted]").into_owned();
    }
    let text = BEARER.replace_all(&text, "${1}[redacted]");
    let mut text = CREDENTIAL.replace_all(&text, "${1}[redacted]").into_owned();
    for secret in secrets {
        if !secret.is_empty() {
            text = text.replace(secret, "[redacted]");
        }
    }
    text
}

/// Keys whose values are credentials, compared case-insensitively.
const CREDENTIAL_KEYS: [&str; 11] = [
    "access_token",
    "refresh_token",
    "id_token",
    "openai_api_key",
    "codex_api_key",
    "op_service_account_token",
    "anthropic_api_key",
    "anthropic_auth_token",
    "claude_code_oauth_token",
    "accesstoken",
    "refreshtoken",
];

pub fn payload(value: &Value, secrets: &[String]) -> Value {
    match value {
        Value::String(s) => redact(s, secrets).into(),
        Value::Array(a) => a.iter().map(|v| payload(v, secrets)).collect(),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(key, value)| {
                    let value = if CREDENTIAL_KEYS.contains(&key.to_lowercase().as_str()) {
                        "[redacted]".into()
                    } else {
                        payload(value, secrets)
                    };
                    (key.clone(), value)
                })
                .collect(),
        ),
        _ => value.clone(),
    }
}

/// Keep an inspectable activity and its outcome without retaining large tool data.
/// Redact complete fields before shortening them, so a cut cannot expose part of a secret.
pub fn compact_activity(event: &Value, secrets: &[String]) -> Value {
    compact_activity_preview(event, Some(secrets))
}

/// Unknown configured secrets prohibit cutting fields: omit oversized values
/// whole, and let the manager redact every complete value that remains.
fn compact_activity_preview(event: &Value, secrets: Option<&[String]>) -> Value {
    let source = &event["item"];
    let mut item = serde_json::Map::new();
    for key in [
        "status",
        "tool",
        "server",
        "cwd",
        "working_directory",
        "query",
        "exit_code",
        "exitCode",
        "duration_ms",
        "durationMs",
        "parent_id",
        "replaces_id",
        "task_id",
        "name",
        "agent_type",
        "background",
    ] {
        if let Some(value) = source
            .get(key)
            .filter(|value| !value.is_object() && !value.is_array())
        {
            item.insert(key.to_owned(), activity_preview(value, 256, secrets));
        }
    }
    for key in ["id", "type"] {
        if let Some(value) = source.get(key) {
            item.insert(key.to_owned(), payload(value, secrets.unwrap_or_default()));
        }
    }
    if let Some(command) = source.get("command") {
        item.insert("command".into(), activity_preview(command, 1024, secrets));
    }
    if let Some(error) = source.get("error") {
        let error = if error.is_object() {
            json!({ "message": activity_preview(&error["message"], 512, secrets) })
        } else if error.is_array() {
            json!({ "message": NOTICE })
        } else {
            activity_preview(error, 512, secrets)
        };
        item.insert("error".into(), error);
    }
    item.insert("details_truncated".into(), true.into());
    match text(source, "type") {
        "command_execution" => {
            item.insert("aggregated_output".into(), NOTICE.into());
        }
        "file_change" => {
            let changes: Vec<_> = source["changes"]
                .as_array()
                .into_iter()
                .flatten()
                .take(16)
                .map(|change| {
                    json!({
                        "path": activity_preview(&change["path"], 256, secrets),
                        "kind": activity_preview(&change["kind"], 64, secrets),
                        "diff": NOTICE,
                    })
                })
                .collect();
            item.insert("changes".into(), changes.into());
        }
        "mcp_tool_call" | "web_search" => {
            item.insert("arguments".into(), json!({ "notice": NOTICE }));
            item.insert(
                "result".into(),
                json!({
                    "isError": source["result"]["isError"] == true,
                    "content": [{ "type": "text", "text": NOTICE }],
                }),
            );
        }
        "todo_list" => {
            let tasks: Vec<_> = source["items"]
                .as_array()
                .into_iter()
                .flatten()
                .take(16)
                .map(|task| {
                    let mut step = json!({
                        "text": activity_preview(&task["text"], 256, secrets),
                        "completed": task["completed"] == true,
                    });
                    for (key, limit) in [("status", 32), ("active_text", 256)] {
                        if let Some(value) = task.get(key) {
                            step[key] = activity_preview(value, limit, secrets);
                        }
                    }
                    step
                })
                .collect();
            item.insert("items".into(), tasks.into());
            if let Some(explanation) = source.get("explanation") {
                item.insert(
                    "explanation".into(),
                    activity_preview(explanation, 1024, secrets),
                );
            }
        }
        "subagent" => {
            for (key, limit) in [
                ("prompt", 4096),
                ("result", 4096),
                ("activity", 1024),
                ("model", 256),
                ("tokens", 0),
                ("tool_uses", 0),
            ] {
                if let Some(value) = source.get(key) {
                    item.insert(key.into(), activity_preview(value, limit, secrets));
                }
            }
        }
        _ => {
            item.insert("text".into(), NOTICE.into());
        }
    }
    json!({ "type": event["type"], "item": item })
}

fn activity_preview(value: &Value, limit: usize, secrets: Option<&[String]>) -> Value {
    if value.is_object() || value.is_array() {
        return Value::Null;
    }
    let redacted = payload(value, secrets.unwrap_or_default());
    let Some(text) = redacted.as_str() else {
        return redacted;
    };
    if text.len() <= limit {
        return redacted;
    }
    if secrets.is_none() {
        return NOTICE.into();
    }

    format!("{}…", &text[..text.floor_char_boundary(limit)]).into()
}

pub fn exhausted(event: &Value) -> bool {
    if event["willRetry"] == true || !["turn.failed", "error"].contains(&text(event, "type")) {
        return false;
    }
    let error = if event["type"] == "turn.failed" {
        &event["error"]
    } else {
        event
    };
    error["code"] == "usage_limit_reached"
        || error["codexErrorInfo"] == "usageLimitExceeded"
        || EXHAUSTED.is_match(text(error, "message"))
}

/// Compatibility entrypoint for the legacy Codex exec protocol.
pub fn args(run: &Value, output: &str, session: Option<&str>) -> Vec<String> {
    crate::coding_agent::codex_args(run, output, session)
}

/// The rule that sets chat turns apart from scheduled work.
pub const INTERACTIVE: &str = "This is an interactive chat. Use native user-input questions when \
    clarification is useful. Nonblocking questions let you continue independent \
    work while the user considers the options; a suggested answer is never user \
    approval. Follow the latest user instructions and do not treat a question \
    as authorization to publish changes. Use cairn_workspace.pending_questions and \
    cairn_workspace.withdraw_question to retire a question that became unnecessary \
    or was answered in ordinary chat. Withdrawal never supplies approval. A completed \
    outcome retires unanswered questions at the end of the turn; report needs_input \
    when an answer is still required.";

pub fn prompt(run: &Value, chat: bool) -> String {
    // Set at launch from the plugins granting this run's agent (`agents::with_instructions`).
    let plugins = if run["isolated"] == true {
        text(run, "pluginInstructions")
    } else {
        ""
    };
    let deliverables = if run["isolated"] == true {
        "When the user requests files, screenshots, videos or documents, publish \
            each finished deliverable with cairn_workspace.publish_artifact. Use a stable \
            key for revisions and a shared group for related screenshots. Artifacts are \
            private by default. When the user requests public sharing, set visibility \
            to public or use cairn_workspace.set_artifact_visibility on an existing \
            artifact, and share its returned publicUrl. Never make an artifact public \
            merely because it is a deliverable. Store export files in the current run \
            workspace or /tmp. Wait for successful publication and include the returned \
            durable URL in your reply. Before your final response, unless ending a \
            confirmed deferred wait with cairn_workspace.wait_for, call \
            cairn_workspace.report_outcome with completed, blocked, or needs_input, a \
            concrete reason, and validation/delivery evidence. A successful process or \
            local commit is not a completed request when an authorized push, CI check, \
            or release remains blocked. Never include credentials in evidence. Do not \
            present VM-local paths as downloadable links."
    } else {
        ""
    };
    let interaction = if chat {
        INTERACTIVE
    } else {
        "This unattended task cannot answer clarification questions; report a \
            concrete blocker if required information is missing."
    };
    let projects = crate::project_workspaces::catalog(run)
        .iter()
        .map(|project| project_line(run, project))
        .collect::<Vec<_>>()
        .join("\n");
    let skills = skill_index(&run["snapshot"]["skills"]);
    let storage = if crate::native::policy::native(&run["snapshot"]["agent"]) {
        "The managed /tmp is cleared on every execution attempt. Keep SDKs and build caches \
            (including Cargo target directories) in the managed home or workspace, never in /tmp. \
            Moving a large directory across filesystems copies it before removing the source \
            and can temporarily exceed the disk budget; managed /tmp and workspace share that budget."
    } else {
        ""
    };
    let tooling = if crate::native::policy::native(&run["snapshot"]["agent"])
        && run["nativeMode"]["mode"] == "administrator"
    {
        "Tooling: This execution uses Native administrator on the real Linux host as an ordinary user with unrestricted passwordless sudo. The owner entrusted this machine to admitted conversations. Keep the managed HOME/workspace for durable work. System changes persist on this node and may affect other work. Respect the user's scope; this mode does not grant new remote Cairn accounts or resources. Root can read credentials present on this host and alter or escape its supervisor, cooperative budgets, leases and cancellation. Do not expose credentials. Returning to protected does not clean or restore this machine."
    } else if crate::native::policy::native(&run["snapshot"]["agent"]) {
        "Tooling: This execution runs on a native Linux node using its installed GPU driver. \
            Cairn supplies the pinned coding engines, Node.js and GitHub CLI. Other tools and \
            project SDKs depend on this node; check availability before using them. Respect \
            the repository's runtime and package-manager pins. Install user tools under HOME \
            when authorized; system directories and the owner's desktop are unavailable. \
            Do not change drivers, GPU ownership or host services. Files in the managed home \
            and workspace persist between turns; /tmp is temporary. Run GPU jobs with care \
            because this GPU also serves the owner's desktop."
    } else {
        "Tooling: mise manages \
            project runtimes and global tools. Prefer rg and fd for search. Respect \
            mise.toml, .tool-versions, .nvmrc, .node-version, .python-version, \
            .java-version, rust-toolchain.toml, and package.json packageManager pins. \
            For Android use cairn-android setup --accept-licenses followed by the \
            project-required SDK packages (for example platforms;android-36 and \
            build-tools;36.0.0). The JDK is managed by mise; use the project Gradle \
            Wrapper. ANDROID_HOME, ANDROID_USER_HOME and GRADLE_USER_HOME are \
            persistent. Never install SDKs or large caches in /tmp or publish them as \
            deliverables. For real device tests use cairn-android emulator start <API> \
            --accept-licenses, then adb -s emulator-5580 or the project \
            connectedAndroidTest task. Stop it with cairn-android emulator stop when \
            finished. Use --aosp for UI tests that do not require Google Play services; \
            it keeps a separate device from the default Google APIs image. The emulator \
            status reports kvm or software acceleration. Nested Android can take \
            several minutes to boot and consumes run memory. Report actual device \
            evidence; Robolectric and JVM tests are not emulator tests. Use mise exec \
            -- <command> when project environment variables are needed; use uv for \
            Python environments. Do not upgrade project pins unless the task requests \
            it."
    };
    format!(
        "{}\n\n{}\n\nAuthorized projects (open only those needed for the task; \
            unopened repositories are not on disk):\n{}\n\n{tooling}\n{storage}\n\nSelected skills (read a skill's SKILL.md before using it; its \
            scripts and references sit beside it):\n{skills}\n\n{plugins}\n\nRun this task to completion within \
            its stated scope. Preserve unrelated files. Do not expose credentials. \
            {interaction}\n{deliverables}{requested}\n{waiting}{delegation} All task-authorized effects such as creating \
            PRs or releasing must follow their checks. Use .agents/skills for skills. \
            Summarize actual changes, validation, external links and remaining blockers \
            at the end.",
        text(&run["snapshot"]["agent"], "instructions"),
        text(&run["snapshot"]["task"], "prompt"),
        if projects.is_empty() {
            "No projects assigned; use the task workspace."
        } else {
            &projects
        },
        requested = if run["requestedBy"].is_object() && run["isolated"] == true {
            " Another agent's work started this run. When the steps left belong to that work, \
                for example because it asked you to stop before them, report the outcome with \
                waitingOn requester so it goes back to that work rather than to the user."
        } else {
            ""
        },
        waiting = if run["isolated"] == true {
            crate::waits::INSTRUCTIONS
        } else {
            ""
        },
        delegation = match (
            run["isolated"] == true,
            text(&run["snapshot"], "delegation")
        ) {
            (true, "requester") => format!(" {}", crate::delegation::INSTRUCTIONS),
            (true, "delegated") => format!(" {}", crate::delegation::DELEGATED_INSTRUCTIONS),
            _ => String::new(),
        },
    )
}

fn project_line(run: &Value, project: &Value) -> String {
    let (name, id) = (text(project, "name"), text(project, "id"));
    let path = run["workspaces"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|w| w["projectId"] == project["id"])
        .map_or("", |w| text(w, "path"));
    if !path.is_empty() {
        format!("- {name} ({id}): {path}")
    } else if run["isolated"] == true {
        format!(
            "- {name} ({id}): not loaded; call cairn_workspace.open_project \
            with this projectId when needed."
        )
    } else {
        format!("- {name}: {}", text(project, "path"))
    }
}

pub fn chat_plan(
    run: &Value,
    prepared: &Value,
    directory: &Path,
    mcp: &Value,
    session: Option<&str>,
) -> crate::error::Result<Value> {
    let mut context = run.clone();
    context["snapshot"]["task"]["prompt"] = "".into();
    if prepared["isolated"] == true {
        context["isolated"] = true.into();
        context["snapshot"]["skills"] = prepared["skills"].clone();
    }
    let mut roots = vec![
        Path::new(text(prepared, "output"))
            .parent()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
    ];
    if let Some(root) = prepared["projectRoot"].as_str() {
        roots.push(root.to_owned());
    }
    roots.extend(
        prepared["workspaces"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|w| text(w, "path").into()),
    );
    let input_directory = if prepared["isolated"] == true {
        Path::new("/run/cairn-chat").to_owned()
    } else {
        directory.join("chat-input")
    };
    let agent = &run["snapshot"]["agent"];
    let provider = crate::provider::Provider::of_run(run)?;
    let mut plan = json!({
        "provider": provider,
        "execution": run["chatExecution"],
        "instructions": prompt(&context, interactive(run)),
        "inputDirectory": input_directory,
        "output": prepared["output"],
        "cwd": prepared["cwd"],
        "model": agent["model"],
        "reasoning": agent["reasoning"],
        "sandbox": policy(agent)["sandbox"],
        "writableRoots": roots,
    });
    crate::store::merge(&mut plan, &provider.adapter().chat_mcp(mcp));
    if let Some(session) = session {
        plan["sessionId"] = session.into();
    }
    Ok(plan)
}

/// Only chat turns can answer questions. Scheduled work also uses the chat
/// protocol, without a conversation to deliver them to.
fn interactive(run: &Value) -> bool {
    run["chatExecution"].is_object()
}

/// The first turn of scheduled work sent through the chat protocol. The
/// instructions already carry the Cairn frame, so a fresh turn sends only the
/// task brief; a resumed turn sends its resume prompt. The brief follows a
/// fixed lead-in: Claude Code runs a turn that starts with a command such as
/// `/clear` as that command, and unattended work has nobody to notice.
pub fn scheduled_turn(run: &Value, resume_prompt: Option<&str>) -> Value {
    let turn = resume_prompt.map_or_else(
        || format!("Task brief:\n{}", text(&run["snapshot"]["task"], "prompt")),
        str::to_owned,
    );
    json!({
        "messageId": run["missionTurnId"].as_str().unwrap_or_else(|| text(run, "id")),
        "text": turn,
        "recovery": resume_prompt.is_some(),
        "attachments": [],
    })
}

/// One line per selected skill: name, description and `SKILL.md` path. The
/// agent reads the file itself, so supporting files load only when used.
fn skill_index(skills: &Value) -> String {
    skills
        .as_array()
        .into_iter()
        .flatten()
        .map(|skill| {
            let description = match text(skill, "description") {
                "" => crate::skills::parse(text(skill, "content"))
                    .map(|parsed| text(&parsed, "description").to_owned())
                    .unwrap_or_default(),
                description => description.to_owned(),
            };
            let description = description.split_whitespace().collect::<Vec<_>>().join(" ");
            format!(
                "- {}: {description}\n  {}",
                text(skill, "name"),
                text(skill, "path")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Chat runners consume their chat payload instead of RunnerPlan.prompt. Both
/// fresh instructions and the actual turn carry the confirmed mode on resumption.
pub(crate) fn native_chat_privileges(chat: &mut Value, run: &Value, mode: &Value) {
    let mut context = run.clone();
    context["nativeMode"] = mode.clone();
    context["snapshot"]["task"]["prompt"] = "".into();
    let notice = native_privileges(mode);
    chat["instructions"] = format!("{notice}\n\n{}", prompt(&context, interactive(run))).into();
    chat["execution"]["text"] = format!("{notice}\n\n{}", text(&chat["execution"], "text")).into();
}

/// Always delivered on a native attempt, including an engine session resumed after a switch.
pub(crate) fn native_privileges(mode: &Value) -> &'static str {
    if mode["mode"] == "administrator" {
        "Effective node mode: Native administrator. You run as an ordinary user with unrestricted passwordless sudo on the real host. The owner confirmed this access. Root can alter the supervisor and local credentials; budgets, leases and cancellation are cooperative. Existing remote Cairn authorizations remain unchanged. Stay within the user's instructions."
    } else {
        "Effective node mode: Native protected. Host sudo and personal host paths are unavailable. Use the managed HOME/workspace. Previous administrator changes, if any, persist: this mode change did not clean or restore the machine."
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn native_chat_turns_and_resumed_instructions_use_the_confirmed_mode() {
        for provider in ["codex", "claude", "opencode"] {
            let run = serde_json::json!({
                "id": "chat", "trigger": "chat", "isolated": true,
                "nativeMode": { "mode": "administrator" },
                "snapshot": { "agent": { "provider": provider, "access": { "backend": "native" } }, "task": {} },
            });
            for mode in ["protected", "administrator"] {
                let mut chat = serde_json::json!({ "instructions": "stale mode instructions", "execution": { "text": "continue the task", "recovery": true } });
                super::native_chat_privileges(
                    &mut chat,
                    &run,
                    &serde_json::json!({ "mode": mode }),
                );
                let expected = if mode == "administrator" {
                    "Effective node mode: Native administrator."
                } else {
                    "Effective node mode: Native protected."
                };
                assert!(chat["instructions"].as_str().unwrap().starts_with(expected));
                assert!(
                    chat["execution"]["text"]
                        .as_str()
                        .unwrap()
                        .starts_with(expected)
                );
                assert!(
                    chat["execution"]["text"]
                        .as_str()
                        .unwrap()
                        .ends_with("continue the task")
                );
                assert!(
                    !chat["instructions"]
                        .as_str()
                        .unwrap()
                        .contains("stale mode instructions")
                );
                assert_eq!(chat["execution"]["recovery"], true);
                if mode == "administrator" {
                    assert!(
                        !chat["instructions"]
                            .as_str()
                            .unwrap()
                            .contains("system directories and the owner's desktop are unavailable")
                    );
                }
            }
        }
    }

    use super::*;

    #[test]
    fn transport_preserves_large_messages_control_events_and_small_activity() {
        let text = "Long message é\n".repeat(3000);
        for event in [
            json!({ "type": "item.completed", "item": { "id": "message", "type": "agent_message", "text": text } }),
            json!({ "type": "chat.question", "question": { "title": text } }),
            json!({ "type": "turn.failed", "error": { "message": text } }),
            json!({ "type": "item.completed", "item": { "id": "tool", "type": "mcp_tool_call", "result": { "isError": true } } }),
        ] {
            let line = event_line(&event);
            assert_eq!(line.last(), Some(&b'\n'));
            assert_eq!(serde_json::from_slice::<Value>(&line).unwrap(), event);
        }
    }

    #[test]
    fn terminal_styling_is_removed_from_diagnostics() {
        let styled = "\x1b[2m2026-10-22T12:08:49Z\x1b[0m \x1b[32m INFO\x1b[0m \x1b[1magent_attempt\x1b[0m\x1b[1m{\x1b[0m\x1b[3mrun_id\x1b[0m\x1b[2m=\x1b[0m\"run\"\x1b[1m}\x1b[0m: \x1b]8;;https://example.com\x07link\x1b]8;;\x07 done\x1bM";
        assert_eq!(
            strip_terminal_styling(styled),
            "2026-10-22T12:08:49Z  INFO agent_attempt{run_id=\"run\"}: link done"
        );
        assert_eq!(
            strip_terminal_styling("Reconnecting... 2/5 [é]"),
            "Reconnecting... 2/5 [é]"
        );
    }

    #[test]
    fn selected_skills_list_paths_instead_of_contents() {
        let skills = json!([
            { "name": "review", "description": "Review a\n  change.", "path": "/home/node/.agents/skills/0/review/SKILL.md", "content": "body" },
            { "name": "legacy", "path": "/skills/1/legacy/SKILL.md", "content": "---\nname: legacy\ndescription: Older snapshot\n---\nBody" },
        ]);
        assert_eq!(
            skill_index(&skills),
            "- review: Review a change.\n  /home/node/.agents/skills/0/review/SKILL.md\n- legacy: Older snapshot\n  /skills/1/legacy/SKILL.md"
        );
    }

    #[test]
    fn chat_protocol_runs_receive_one_frame_matching_their_interaction() {
        let guest = "/home/node/.agents/skills/0/review/SKILL.md";
        let host = "/installation/skills/review/SKILL.md";
        for provider in ["codex", "claude", "opencode"] {
            let mut run = json!({
                "id": "run", "isolated": true, "missionTurnId": "turn",
                "pluginInstructions": "Mail: never print credentials.",
                "snapshot": {
                    "agent": { "provider": provider, "instructions": "Agent rules." },
                    "task": { "prompt": "Check the deployment." },
                    "skills": [{ "name": "review", "description": "Review a change.", "path": host }],
                },
            });
            let prepared = json!({
                "isolated": true,
                "output": "/run/output/result.md",
                "skills": [{ "name": "review", "description": "Review a change.", "path": guest }],
            });
            let plan = chat_plan(&run, &prepared, Path::new("/tmp"), &json!({}), None).unwrap();
            let instructions = text(&plan, "instructions");
            for expected in [
                "Agent rules.",
                "Mail: never print credentials.",
                guest,
                "This unattended task",
            ] {
                assert!(instructions.contains(expected), "{provider}: {expected}");
            }
            assert!(!instructions.contains(host));
            assert!(!instructions.contains("This is an interactive chat"));

            let turn = scheduled_turn(&run, None);
            assert_eq!(turn["messageId"], "turn");
            assert_eq!(turn["text"], "Task brief:\nCheck the deployment.");
            assert_eq!(turn["recovery"], false);
            let resumed = scheduled_turn(&run, Some("Continue."));
            assert_eq!(resumed["text"], "Continue.");
            assert_eq!(resumed["recovery"], true);

            run["chatExecution"] = json!({ "messageId": "message", "text": "Hello" });
            let plan = chat_plan(&run, &prepared, Path::new("/tmp"), &json!({}), None).unwrap();
            let instructions = text(&plan, "instructions");
            assert!(instructions.contains("This is an interactive chat"));
            assert!(instructions.contains("Mail: never print credentials."));
            assert!(!instructions.contains("This unattended task"));
            assert_eq!(plan["execution"]["text"], "Hello");
        }
    }

    #[test]
    fn native_chat_protocol_runs_keep_the_frame_of_their_interaction() {
        for provider in ["codex", "claude", "opencode"] {
            let mut run = json!({
                "id": "run",
                "isolated": true,
                "pluginInstructions": "Mail: never print credentials.",
                "snapshot": {
                    "agent": { "provider": provider, "access": { "backend": "native" } },
                    "task": { "prompt": "Check the deployment." },
                },
            });
            let mode = json!({ "mode": "protected" });
            let mut chat = json!({
                "execution": { "text": "Task brief:\nCheck the deployment." },
            });
            native_chat_privileges(&mut chat, &run, &mode);
            let instructions = text(&chat, "instructions");
            assert!(instructions.contains("Mail: never print credentials."));
            assert!(instructions.contains("This unattended task"), "{provider}");
            assert!(!instructions.contains("This is an interactive chat"));
            assert!(!instructions.contains("Check the deployment."));

            run["chatExecution"] = json!({ "messageId": "message", "text": "Hello" });
            let mut chat = json!({ "execution": { "text": "Hello" } });
            native_chat_privileges(&mut chat, &run, &mode);
            let instructions = text(&chat, "instructions");
            assert!(instructions.contains("Mail: never print credentials."));
            assert!(
                instructions.contains("This is an interactive chat"),
                "{provider}"
            );
            assert!(!instructions.contains("This unattended task"));
        }
    }

    #[test]
    fn subscription_limits_of_both_coding_agents_are_exhaustion() {
        for message in [
            "You've hit your usage limit. Upgrade or try again later.",
            "You've hit your individual spend limit · run /usage-credits to ask your admin for a higher limit · your weekly limit resets 3pm (UTC)",
            "You’ve hit your individual spend limit",
            "You've hit your spend limit",
            "Usage limit reached for this plan",
            "Claude AI usage limit reached|1790000000",
            "You've hit your limit · resets 3pm (Europe/Paris)",
            "5-hour limit reached ∙ resets 5pm",
        ] {
            assert!(
                exhausted(&json!({"type":"turn.failed","error":{"message":message}})),
                "{message}"
            );
        }
        assert!(exhausted(
            &json!({"type":"error","code":"usage_limit_reached"})
        ));
        assert!(!exhausted(&json!({
            "type": "error",
            "code": "usage_limit_reached",
            "message": "Usage limit reached",
            "willRetry": true,
        })));
        for message in ["Rate limit exceeded, retrying", "The usage limit docs say"] {
            assert!(
                !exhausted(&json!({"type":"turn.failed","error":{"message":message}})),
                "{message}"
            );
        }
    }
}
