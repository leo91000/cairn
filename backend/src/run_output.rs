use crate::{service::policy, validation::text};
use serde_json::{Value, json};
use std::{path::Path, sync::LazyLock};

static TOKEN: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"\b(?:gh[pousr]_\w{15,}|github_pat_\w{15,}|ops_[\w.-]{15,}|sk-[\w-]{12,})\b")
        .unwrap()
});

static BEARER: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(?i)(Bearer\s+)[\w.~-]+").unwrap());

static CREDENTIAL: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r#"(?i)("?(?:access_token|refresh_token|id_token|OPENAI_API_KEY|CODEX_API_KEY|ANTHROPIC_API_KEY|ANTHROPIC_AUTH_TOKEN|CLAUDE_CODE_OAUTH_TOKEN|accessToken|refreshToken|OP_SERVICE_ACCOUNT_TOKEN)"?\s*[:=]\s*"?)[^"\s,}]+"#).unwrap()
});

// Codex and Claude Code subscription limits. Claude Code reports them as the turn's result.
static EXHAUSTED: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?i)^(?:you['’]ve hit your (?:usage |session |weekly )?limit|you have hit your (?:usage )?limit|usage limit (?:has been )?(?:reached|exceeded)|claude ai usage limit reached|(?:5-hour|session|weekly|opus) limit reached)\b").unwrap()
});

pub fn redact(text: &str, secrets: &[String]) -> String {
    let text = TOKEN.replace_all(text, "[redacted]");
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

pub fn exhausted(event: &Value) -> bool {
    if !["turn.failed", "error"].contains(&text(event, "type")) {
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

pub fn args(run: &Value, output: &str, session: Option<&str>) -> Vec<String> {
    let agent = &run["snapshot"]["agent"];
    let access = policy(agent);
    let mut args = if access["sandbox"] == "yolo" {
        vec!["--dangerously-bypass-approvals-and-sandbox".into()]
    } else {
        vec![
            "--sandbox".into(),
            text(&access, "sandbox").into(),
            "-a".into(),
            "never".into(),
        ]
    };
    args.extend(
        [
            "-c",
            "forced_login_method=\"chatgpt\"",
            "-c",
            "cli_auth_credentials_store=\"file\"",
        ]
        .map(str::to_owned),
    );
    let roots = || {
        let mut roots = vec![
            Path::new(output)
                .parent()
                .unwrap_or(Path::new("."))
                .to_string_lossy()
                .into_owned(),
        ];
        roots.extend(
            run["workspaces"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|w| text(w, "path").to_owned()),
        );
        roots
            .into_iter()
            .flat_map(|r| vec!["--add-dir".into(), r])
            .collect::<Vec<_>>()
    };
    if session.is_some() && access["sandbox"] == "workspace-write" {
        args.extend(roots());
    }
    args.push("exec".into());
    if let Some(session) = session {
        args.extend(["resume".into(), session.into()]);
    }
    args.extend(["--json", "--skip-git-repo-check"].map(str::to_owned));
    if session.is_none() {
        args.extend(["--color", "never"].map(str::to_owned));
    }
    if !text(agent, "reasoning").is_empty() {
        args.extend([
            "-c".into(),
            format!("model_reasoning_effort={}", agent["reasoning"]),
        ]);
    }
    if !text(agent, "model").is_empty() {
        args.extend(["--model".into(), text(agent, "model").into()]);
    }
    if access["sandbox"] == "workspace-write" {
        args.extend(["-c", "sandbox_workspace_write.network_access=true"].map(str::to_owned));
        if session.is_none() {
            args.extend(roots());
        }
    }
    args.extend(["--output-last-message".into(), output.into(), "-".into()]);
    args
}

pub fn prompt(run: &Value, chat: bool) -> String {
    let onepassword = if run["isolated"] == true {
        crate::onepassword::AGENT_INSTRUCTIONS
    } else {
        ""
    };
    let deliverables = if run["isolated"] == true {
        "When the user requests files, screenshots, videos or documents, publish \
            each finished deliverable with leo_workspace.publish_artifact. Use a stable \
            key for revisions and a shared group for related screenshots. Artifacts are \
            private by default. When the user requests public sharing, set visibility \
            to public or use leo_workspace.set_artifact_visibility on an existing \
            artifact, and share its returned publicUrl. Never make an artifact public \
            merely because it is a deliverable. Store export files in the current run \
            workspace or /tmp. Wait for successful publication and include the returned \
            durable URL in your reply. Before your final response call \
            leo_workspace.report_outcome with completed, blocked, or needs_input, a \
            concrete reason, and validation/delivery evidence. A successful process or \
            local commit is not a completed request when an authorized push, CI check, \
            or release remains blocked. Never include credentials in evidence. Do not \
            present VM-local paths as downloadable links."
    } else {
        ""
    };
    let interaction = if chat {
        "This is an interactive chat. Use native user-input questions when \
            clarification is useful. Nonblocking questions let you continue independent \
            work while the user considers the options; a suggested answer is never user \
            approval. Follow the latest user instructions and do not treat a question \
            as authorization to publish changes."
    } else {
        "This unattended task cannot answer clarification questions; report a \
            concrete blocker if required information is missing."
    };
    let projects = crate::project_workspaces::catalog(run)
        .iter()
        .map(|project| project_line(run, project))
        .collect::<Vec<_>>()
        .join("\n");
    let skills = run["snapshot"]["skills"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|s| format!("\n{}\n{}", text(s, "path"), text(s, "content")))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "{}\n\n{}\n\nAuthorized projects (open only those needed for the task; \
            unopened repositories are not on disk):\n{}\n\nTooling: mise manages \
            project runtimes and global tools. Prefer rg and fd for search. Respect \
            mise.toml, .tool-versions, .nvmrc, .node-version, .python-version, \
            .java-version, rust-toolchain.toml, and package.json packageManager pins. \
            For Android use leo-android setup --accept-licenses followed by the \
            project-required SDK packages (for example platforms;android-36 and \
            build-tools;36.0.0). The JDK is managed by mise; use the project Gradle \
            Wrapper. ANDROID_HOME, ANDROID_USER_HOME and GRADLE_USER_HOME are \
            persistent. Never install SDKs or large caches in /tmp or publish them as \
            deliverables. For real device tests use leo-android emulator start <API> \
            --accept-licenses, then adb -s emulator-5580 or the project \
            connectedAndroidTest task. Stop it with leo-android emulator stop when \
            finished. Use --aosp for UI tests that do not require Google Play services; \
            it keeps a separate device from the default Google APIs image. The emulator \
            status reports kvm or software acceleration. Nested Android can take \
            several minutes to boot and consumes run memory. Report actual device \
            evidence; Robolectric and JVM tests are not emulator tests. Use mise exec \
            -- <command> when project environment variables are needed; use uv for \
            Python environments. Do not upgrade project pins unless the task requests \
            it.\n\nSelected skills (use their supporting resources from the supplied \
            paths):\n{skills}\n\n{onepassword}\n\nRun this task to completion within \
            its stated scope. Preserve unrelated files. Do not expose credentials. \
            {interaction}\n{deliverables} All task-authorized effects such as creating \
            PRs or releasing must follow their checks. Use .agents/skills for skills. \
            Summarize actual changes, validation, external links and remaining blockers \
            at the end.",
        text(&run["snapshot"]["agent"], "instructions"),
        text(&run["snapshot"]["task"], "prompt"),
        if projects.is_empty() {
            "No projects assigned; use the task workspace."
        } else {
            &projects
        }
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
            "- {name} ({id}): not loaded; call leo_workspace.open_project \
            with this projectId when needed."
        )
    } else {
        format!("- {name}: {}", text(project, "path"))
    }
}

/// A thread's complete transport selection. Select the built-in provider explicitly
/// on rollback so a resumed HTTP thread cannot retain its previous provider.
pub fn codex_configuration(servers: &Value, http_version: Option<&str>) -> Value {
    let mut configuration = json!({
        "mcp_servers": servers,
        "model_provider": "openai",
    });
    let Some(version) = http_version else {
        return configuration;
    };
    configuration["model_provider"] = "leo_http".into();
    configuration["model_providers"] = json!({
        "leo_http": {
            // Codex uses this name to retain ChatGPT backend routing and capabilities.
            "name": "OpenAI",
            "wire_api": "responses",
            "requires_openai_auth": true,
            "supports_websockets": false,
            "supports_standalone_web_search": true,
            "include_internal_metadata": true,
            "http_headers": { "version": version },
            "env_http_headers": {
                "OpenAI-Organization": "OPENAI_ORGANIZATION",
                "OpenAI-Project": "OPENAI_PROJECT",
            },
        },
    });
    configuration
}

pub fn chat_plan(
    run: &Value,
    prepared: &Value,
    directory: &Path,
    mcp: &Value,
    session: Option<&str>,
) -> Value {
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
        Path::new("/run/leo-chat").to_owned()
    } else {
        directory.join("chat-input")
    };
    let agent = &run["snapshot"]["agent"];
    let provider = crate::provider::Provider::of_run(run);
    // MCP access belongs to the current thread lease. Duplicating it in CLI
    // options prevents an already initialized Codex process from being reused.
    let args = if provider == crate::provider::Provider::Codex {
        json!([])
    } else {
        mcp["args"].clone()
    };
    let http_requested = std::env::var("LEO_CODEX_TRANSPORT").as_deref() == Ok("http");
    let http_version = http_requested
        .then(|| std::env::var("APP_CODEX_VERSION").ok())
        .flatten()
        .filter(|version| !version.trim().is_empty());
    if http_requested && http_version.is_none() && provider == crate::provider::Provider::Codex {
        tracing::warn!("HTTP Codex transport needs the image's APP_CODEX_VERSION; using WebSocket");
    }
    let codex_config = codex_configuration(&mcp["codexConfig"], http_version.as_deref());
    let mut plan = json!({
        "provider": provider,
        "claudeMcps": mcp["claudeMcps"],
        "claudeDeniedTools": mcp["claudeDeniedTools"],
        "execution": run["chatExecution"],
        "instructions": prompt(&context, true),
        "inputDirectory": input_directory,
        "output": prepared["output"],
        "cwd": prepared["cwd"],
        "model": agent["model"],
        "reasoning": agent["reasoning"],
        "sandbox": policy(agent)["sandbox"],
        "writableRoots": roots,
        "args": args,
        "codexConfig": codex_config,
    });
    if let Some(session) = session {
        plan["sessionId"] = session.into();
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscription_limits_of_both_coding_agents_are_exhaustion() {
        for message in [
            "You've hit your usage limit. Upgrade or try again later.",
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
        for message in ["Rate limit exceeded, retrying", "The usage limit docs say"] {
            assert!(
                !exhausted(&json!({"type":"turn.failed","error":{"message":message}})),
                "{message}"
            );
        }
    }
}
