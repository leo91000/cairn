//! Owner-managed credentials and live, run-scoped read access to 1Password.
use crate::{
    config::{id, now},
    error::{Error, Result},
    http::Input,
    process::{Environment, bounded_output, command},
    project_workspaces::authorize_in,
    service::Service,
    validation::{text, uuid},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;

const KIND: &str = "onepassword";
pub const AGENT_INSTRUCTIONS: &str = "1Password: When a task needs credentials, use leo_workspace.onepassword, backed by the server-side 1Password CLI. Start with accounts to discover the enabled service accounts explicitly authorized for this agent, then vaults, items, fields and read as needed. Service account tokens stay on the server; do not expect an authenticated op CLI in the task environment. Some website accounts require a passkey. This integration cannot retrieve or use passkeys. If a sign-in requires a passkey, stop that sign-in and ask the user to enable a TOTP authenticator on the website and save its one-time password configuration in the matching 1Password item, if the website supports it. Wait for the user to confirm setup before retrying. Do not try to bypass the passkey requirement or change authentication settings yourself. If TOTP is unavailable, report the blocked sign-in and ask the user how to proceed. For unattended tasks, report the required user action instead of retrying. Never ask the user to paste passwords, TOTP seeds or recovery codes into chat, and never print credentials in messages, logs or artifacts.";

const TOKEN_SIZE_LIMIT: usize = 16384;
const UNAUTHORIZED: &str = "This 1Password account is not authorized.";

fn secret_key(id: &str) -> String {
    format!("onepassword:{id}")
}

/// An owner-managed service account (`onepassword` record). Its token lives in the vault.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Account {
    id: String,
    name: String,
    agent_ids: Vec<String>,
    enabled: bool,
    updated_at: i64,
}

impl Account {
    fn permits(&self, agent_id: &str) -> bool {
        self.enabled && self.agent_ids.iter().any(|id| id == agent_id)
    }
}

#[derive(Serialize, Deserialize)]
struct Credential {
    token: String,
}

fn valid_token(token: &str) -> bool {
    token.starts_with("ops_")
        && token.len() <= TOKEN_SIZE_LIMIT
        && !token.chars().any(|c| c.is_whitespace() || c.is_control())
}

pub async fn save(s: &Service, input: &Value, existing: Option<&str>) -> Result<Value> {
    // Validate manually: schema validator messages must never echo a submitted token.
    let name = text(input, "name").trim();
    let token = input
        .get("token")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(Error::bad("Enter a name of 1–100 characters."));
    }
    let malformed_token = input.get("token").is_some_and(|v| !v.is_string())
        || (!token.is_empty() && !valid_token(token));
    if malformed_token || (existing.is_none() && token.is_empty()) {
        return Err(Error::bad(
            "Enter a valid 1Password service account token (ops_…).",
        ));
    }
    let agents = input["agentIds"]
        .as_array()
        .filter(|a| a.len() <= 100)
        .ok_or_else(|| Error::bad("Select the agents allowed to use this account."))?;
    let mut agent_ids = Vec::with_capacity(agents.len());
    for agent in agents {
        let agent = agent.as_str().unwrap_or("");
        uuid(agent)?;
        agent_ids.push(agent.to_owned());
    }
    let enabled = input["enabled"]
        .as_bool()
        .ok_or_else(|| Error::bad("Choose whether this account is enabled."))?;
    let account_id = existing.map_or_else(id, str::to_owned);
    uuid(&account_id)?;
    let encrypted = if token.is_empty() {
        None
    } else {
        let credential = serde_json::to_value(Credential {
            token: token.to_owned(),
        })?;
        Some(s.vault.encrypt(&secret_key(&account_id), &credential)?)
    };
    let account = Account {
        id: account_id,
        name: name.to_owned(),
        agent_ids,
        enabled,
        updated_at: now(),
    };
    let updating = existing.is_some();
    s.store
        .transaction(move |db| {
            if updating && db.get(KIND, &account.id)?.is_none() {
                return Err(Error::not_found("1Password account not found."));
            }
            for agent in &account.agent_ids {
                if db.get("agents", agent)?.is_none() {
                    return Err(Error::bad("A selected agent no longer exists."));
                }
            }
            if let Some(encrypted) = encrypted {
                let key = format!("mcp-secret:{}", secret_key(&account.id));
                db.set(&key, &encrypted, None)?;
            }
            let record = serde_json::to_value(&account)?;
            db.put(KIND, &record)?;
            db.audit("onepassword.saved", &json!({ "id": account.id }))?;
            Ok(record)
        })
        .await
}

pub async fn remove(s: &Service, account_id: &str) -> Result<Value> {
    uuid(account_id)?;
    let account_id = account_id.to_owned();
    s.store
        .transaction(move |db| {
            db.remove(KIND, &account_id)?;
            db.delete(&format!("mcp-secret:{}", secret_key(&account_id)))?;
            db.audit("onepassword.deleted", &json!({ "id": account_id }))?;
            Ok(json!({ "deleted": true }))
        })
        .await
}

pub async fn routes(s: &Service, input: &Input) -> Result<Value> {
    let parts = input
        .path
        .trim_start_matches("/api/")
        .split('/')
        .collect::<Vec<_>>();
    match (input.method.as_str(), parts.as_slice()) {
        ("GET", ["onepassword"]) => Ok(s.store.list(KIND).await?.into()),
        ("POST", ["onepassword"]) => save(s, &input.body, None).await,
        ("PUT", ["onepassword", id]) => save(s, &input.body, Some(id)).await,
        ("DELETE", ["onepassword", id]) => remove(s, id).await,
        ("POST", ["onepassword", id, "test"]) => {
            uuid(id)?;
            s.get(KIND, id).await?;
            let credential = s
                .vault
                .get(&secret_key(id))
                .await?
                .ok_or_else(|| Error::bad("Token missing."))?;
            let args = ["vault", "list", "--format=json"].map(str::to_owned);
            execute(text(&credential, "token"), &args).await?;
            Ok(json!({ "ok": true }))
        }
        _ => Err(Error::not_found("Not found")),
    }
}

pub fn tool() -> Value {
    let description = format!(
        "Read-only secret access. items requires vault; fields requires vault and item and returns field references without values; read resolves an op://vault/item/field reference. {AGENT_INSTRUCTIONS}"
    );
    json!({
        "name": "onepassword",
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["accounts", "vaults", "items", "fields", "read"],
                },
                "accountId": { "type": "string", "format": "uuid" },
                "vault": { "type": "string", "minLength": 1, "maxLength": 200 },
                "item": { "type": "string", "minLength": 1, "maxLength": 200 },
                "reference": { "type": "string", "pattern": "^op://", "maxLength": 2000 },
            },
            "required": ["operation"],
            "additionalProperties": false,
        },
    })
}

/// Enabled accounts that this run's agent may use.
fn permitted_accounts(db: &crate::store::Db<'_>, run: &Value) -> Result<Vec<Account>> {
    let agent_id = text(&run["snapshot"]["agent"], "id");
    let mut accounts = Vec::new();
    for account in db.list(KIND)? {
        let account = Account::deserialize(&account)?;
        if account.permits(agent_id) {
            accounts.push(account);
        }
    }
    Ok(accounts)
}

// Authorization and credential lookup share a database read. Rechecked after CLI I/O.
async fn grant(s: &Service, bearer: &str, account_id: &str) -> Result<Value> {
    let bearer = bearer.to_owned();
    let key = secret_key(account_id);
    let account_id = account_id.to_owned();
    let encrypted = s
        .store
        .read(move |db| {
            let run = authorize_in(db, &bearer)?;
            let agent_id = text(&run["snapshot"]["agent"], "id");
            let account = match db.get(KIND, &account_id)? {
                Some(account) => Account::deserialize(&account)?,
                None => return Err(Error::forbidden(UNAUTHORIZED)),
            };
            if !account.permits(agent_id) {
                return Err(Error::forbidden(UNAUTHORIZED));
            }
            db.kv(&format!("mcp-secret:{}", secret_key(&account.id)))?
                .ok_or_else(|| Error::forbidden(UNAUTHORIZED))
        })
        .await?;
    s.vault.decrypt(&key, &encrypted)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Operation {
    Accounts,
    Vaults,
    Items,
    Fields,
    Read,
}

impl Operation {
    fn of(input: &Value) -> Option<Self> {
        Some(match text(input, "operation") {
            "accounts" => Self::Accounts,
            "vaults" => Self::Vaults,
            "items" => Self::Items,
            "fields" => Self::Fields,
            "read" => Self::Read,
            _ => return None,
        })
    }
}

/// A vault or item selector: short, present and free of control characters.
fn selector<'a>(input: &'a Value, key: &str, missing: &str) -> Result<&'a str> {
    let value = text(input, key);
    if value.is_empty() || value.len() > 200 || value.chars().any(char::is_control) {
        return Err(Error::bad(missing));
    }
    Ok(value)
}

fn arguments(input: &Value) -> Result<Vec<String>> {
    let operation =
        Operation::of(input).ok_or_else(|| Error::bad("Unknown 1Password operation."))?;
    let args = match operation {
        Operation::Vaults => vec!["vault".into(), "list".into(), "--format=json".into()],
        Operation::Items => {
            let vault = selector(input, "vault", "A vault name or ID is required.")?;
            vec![
                "item".into(),
                "list".into(),
                format!("--vault={vault}"),
                "--format=json".into(),
            ]
        }
        Operation::Fields => {
            let vault = selector(input, "vault", "A vault name or ID is required.")?;
            let item = selector(input, "item", "An item name or ID is required.")?;
            vec![
                "item".into(),
                "get".into(),
                format!("--vault={vault}"),
                "--format=json".into(),
                "--".into(),
                item.into(),
            ]
        }
        Operation::Read => {
            let reference = text(input, "reference");
            if !reference.starts_with("op://")
                || reference.len() > 2000
                || reference.chars().any(char::is_control)
            {
                return Err(Error::bad("Use a valid op://vault/item/field reference."));
            }
            vec!["read".into(), "--no-newline".into(), reference.into()]
        }
        Operation::Accounts => return Err(Error::bad("Unknown 1Password operation.")),
    };
    Ok(args)
}

pub async fn call(s: &Service, bearer: &str, input: &Value) -> Result<Value> {
    call_with_binary(s, bearer, input, "/usr/local/bin/op").await
}

#[derive(Serialize)]
struct AccountSummary {
    id: String,
    name: String,
}

async fn list_accounts(s: &Service, bearer: &str) -> Result<Value> {
    let bearer = bearer.to_owned();
    s.store
        .read(move |db| {
            let run = authorize_in(db, &bearer)?;
            let accounts = permitted_accounts(db, &run)?
                .into_iter()
                .map(|account| AccountSummary {
                    id: account.id,
                    name: account.name,
                })
                .collect::<Vec<_>>();
            Ok(json!({ "accounts": accounts }))
        })
        .await
}

async fn call_with_binary(s: &Service, bearer: &str, input: &Value, binary: &str) -> Result<Value> {
    let operation = Operation::of(input);
    if operation == Some(Operation::Accounts) {
        return list_accounts(s, bearer).await;
    }
    let args = arguments(input)?;
    let account_id = text(input, "accountId");
    uuid(account_id)?;
    let credential = grant(s, bearer, account_id).await?;
    let output = execute_with_binary(text(&credential, "token"), &args, binary).await?;
    let current = grant(s, bearer, account_id).await?;
    if current != credential {
        return Err(Error::forbidden(
            "1Password token changed. Retry the operation.",
        ));
    }
    if operation == Some(Operation::Read) {
        return Ok(json!({ "value": output }));
    }
    let value: Value = serde_json::from_str(&output)
        .map_err(|_| Error::bad_gateway("Invalid 1Password response."))?;
    if operation == Some(Operation::Fields) {
        return Ok(field_references(&value));
    }
    Ok(json!({ "items": value }))
}

/// Field metadata of a CLI item, without its values.
#[derive(Serialize)]
struct FieldReference<'a> {
    id: &'a Value,
    label: &'a Value,
    #[serde(rename = "type")]
    kind: &'a Value,
    reference: &'a Value,
    section: &'a Value,
}

fn field_references(item: &Value) -> Value {
    let fields = item["fields"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|field| FieldReference {
            id: &field["id"],
            label: &field["label"],
            kind: &field["type"],
            reference: &field["reference"],
            section: &field["section"]["label"],
        })
        .collect::<Vec<_>>();
    json!({ "fields": fields })
}

async fn execute(token: &str, args: &[String]) -> Result<String> {
    execute_with_binary(token, args, "/usr/local/bin/op").await
}

async fn execute_with_binary(token: &str, args: &[String], binary: &str) -> Result<String> {
    let home = tempfile::tempdir()?;
    // No inherited Connect tokens, desktop sessions, agent env or persistent cache.
    let env = Environment::from([
        ("HOME".into(), home.path().to_string_lossy().into_owned()),
        ("OP_SERVICE_ACCOUNT_TOKEN".into(), token.into()),
        ("OP_CACHE".into(), "false".into()),
    ]);
    let output = bounded_output(
        command(binary, args, &env, Some(home.path())),
        Duration::from_secs(30),
        1024 * 1024,
    )
    .await
    .map_err(|_| {
        Error::bad_gateway(
            "1Password is unavailable. Check the server CLI, token, and vault permissions.",
        )
    })?;
    if !output.success {
        return Err(Error::bad_gateway(
            "1Password request failed. Check the token, vault permissions, reference, and service account limits.",
        ));
    }
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{Config, MAIN_AGENT_ID},
        service::Service,
    };
    use std::{os::unix::fs::PermissionsExt, sync::Arc};
    use tempfile::TempDir;

    async fn fixture() -> (TempDir, Arc<Service>, String, String) {
        let root = TempDir::new().unwrap();
        std::fs::create_dir_all(root.path().join("home/.codex")).unwrap();
        std::fs::write(root.path().join("home/.codex/leo-managed-auth"), "1").unwrap();
        let s = Service::new(Config {
            data_dir: root.path().join("data"),
            home: root.path().join("home"),
            workspace_roots: vec![root.path().to_owned()],
            public_url: "http://localhost:4310".into(),
            host: "127.0.0.1".into(),
            port: 0,
            setup_token: "fixture".into(),
            codex_bin: "codex".into(),
            claude_bin: "claude".into(),
            gh_bin: "gh".into(),
            concurrency: 1,
            logger: false,
            worker_enabled: false,
            runner_url: "http://runner:4311".into(),
        })
        .await
        .unwrap();
        let task = s
            .task(
                json!({
                    "name": "Probe",
                    "agentId": MAIN_AGENT_ID,
                    "prompt": "Hello"
                }),
                None,
            )
            .await
            .unwrap();
        let run = s.enqueue(text(&task, "id"), "manual", None).await.unwrap();
        s.store
            .patch_run(text(&run, "id"), json!({ "status": "running" }))
            .await
            .unwrap();
        let config = s.mcps.run_configuration(&s, &run).await.unwrap();
        let bearer = text(&config["env"], "LEO_MCP_RUN_TOKEN").to_owned();
        let binary = root.path().join("op");
        std::fs::write(&binary, "#!/bin/sh\n[ \"$OP_SERVICE_ACCOUNT_TOKEN\" = ops_fixture ] || exit 1\n[ -z \"$OP_CONNECT_TOKEN\" ] || exit 2\n[ \"$OP_CACHE\" = false ] || exit 3\ncase \"$1\" in\nread) printf 'secret-with-no-newline' ;;\n*) printf '[{\"id\":\"vault\"}]' ;;\nesac\n").unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        (root, s, bearer, binary.to_string_lossy().into_owned())
    }

    fn account() -> Value {
        json!({
            "name": "Production",
            "token": "ops_fixture",
            "enabled": true,
            "agentIds": []
        })
    }

    #[tokio::test]
    async fn encrypted_credentials_live_grants_rotation_and_deletion() {
        let (_root, s, bearer, binary) = fixture().await;
        let saved = save(&s, &account(), None).await.unwrap();
        let id = text(&saved, "id");
        assert!(!saved.to_string().contains("ops_fixture"));
        let encrypted = s
            .store
            .kv(&format!("mcp-secret:{}", secret_key(id)))
            .await
            .unwrap()
            .unwrap();
        assert!(!encrypted.to_string().contains("ops_fixture"));
        assert_eq!(
            s.vault.get(&secret_key(id)).await.unwrap().unwrap()["token"],
            "ops_fixture"
        );
        let args = json!({
            "operation": "read",
            "accountId": id,
            "reference": "op://vault/item/password",
        });
        assert_eq!(
            call_with_binary(&s, &bearer, &args, &binary)
                .await
                .unwrap_err()
                .status,
            403
        );
        assert_eq!(
            call(&s, &bearer, &json!({ "operation": "accounts" }))
                .await
                .unwrap()["accounts"],
            json!([])
        );
        let mut update = saved.clone();
        update["agentIds"] = json!([MAIN_AGENT_ID]);
        save(&s, &update, Some(id)).await.unwrap(); // omitted token preserves it
        assert_eq!(
            call(&s, &bearer, &json!({ "operation": "accounts" }))
                .await
                .unwrap()["accounts"][0]["id"],
            id
        );
        assert_eq!(
            call_with_binary(&s, &bearer, &args, &binary).await.unwrap()["value"],
            "secret-with-no-newline"
        );
        for operation in ["items", "vaults"] {
            assert_eq!(
                call_with_binary(
                    &s,
                    &bearer,
                    &json!({
                        "operation": operation,
                        "accountId": id,
                        "vault": "vault"
                    }),
                    &binary
                )
                .await
                .unwrap()["items"][0]["id"],
                "vault"
            );
        }
        update["enabled"] = false.into();
        save(&s, &update, Some(id)).await.unwrap();
        assert_eq!(
            call_with_binary(&s, &bearer, &args, &binary)
                .await
                .unwrap_err()
                .status,
            403
        );
        update["enabled"] = true.into();
        update["agentIds"] = json!([]);
        save(&s, &update, Some(id)).await.unwrap();
        assert_eq!(
            call_with_binary(&s, &bearer, &args, &binary)
                .await
                .unwrap_err()
                .status,
            403
        );
        update["agentIds"] = json!([MAIN_AGENT_ID]);
        update["token"] = "ops_rotated".into();
        save(&s, &update, Some(id)).await.unwrap();
        assert_eq!(
            s.vault.get(&secret_key(id)).await.unwrap().unwrap()["token"],
            "ops_rotated"
        );
        let temporary = s.agent(json!({ "name": "Temporary" }), None).await.unwrap();
        update["agentIds"] = json!([temporary["id"]]);
        save(&s, &update, Some(id)).await.unwrap();
        s.remove("agents", text(&temporary, "id")).await.unwrap();
        assert_eq!(s.get(KIND, id).await.unwrap()["agentIds"], json!([]));
        remove(&s, id).await.unwrap();
        assert!(s.vault.get(&secret_key(id)).await.unwrap().is_none());
        assert_eq!(
            call_with_binary(&s, &bearer, &args, &binary)
                .await
                .unwrap_err()
                .status,
            403
        );
    }

    #[tokio::test]
    async fn errors_never_echo_tokens_and_revoked_runs_cannot_read() {
        let (root, s, bearer, binary) = fixture().await;
        let mut input = account();
        input["token"] = "private-invalid-token".into();
        let error = save(&s, &input, None).await.unwrap_err();
        assert!(!error.message.contains("private-invalid-token"));
        input = account();
        input["agentIds"] = json!([id()]);
        assert!(save(&s, &input, None).await.is_err());
        assert!(s.store.list(KIND).await.unwrap().is_empty());
        input["agentIds"] = json!([MAIN_AGENT_ID]);
        let saved = save(&s, &input, None).await.unwrap();
        let args = json!({
            "operation": "read",
            "accountId": saved["id"],
            "reference": "op://vault/item/password",
        });
        std::fs::write(
            &binary,
            "#!/bin/sh\necho ops_fixture >&2\necho ops_fixture\nexit 1\n",
        )
        .unwrap();
        let error = call_with_binary(&s, &bearer, &args, &binary)
            .await
            .unwrap_err();
        assert!(!error.message.contains("ops_fixture"));
        let slow = root.path().join("slow");
        let started = root.path().join("started");
        let released = root.path().join("released");
        std::fs::write(
            &slow,
            format!(
                "#!/bin/sh\n: > '{}'\nwhile [ ! -f '{}' ]; do sleep 0.01; done\nprintf secret\n",
                started.display(),
                released.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&slow, std::fs::Permissions::from_mode(0o700)).unwrap();
        let revoke = async {
            tokio::time::timeout(Duration::from_secs(10), async {
                while !started.exists() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            remove(&s, text(&saved, "id")).await.unwrap();
            std::fs::write(&released, "").unwrap();
        };
        let slow = slow.to_string_lossy();
        let (result, ()) = tokio::join!(call_with_binary(&s, &bearer, &args, &slow), revoke);
        assert_eq!(result.unwrap_err().status, 403);
        let run = crate::project_workspaces::authorize(&s, &bearer)
            .await
            .unwrap();
        s.mcps.revoke_run(&s, text(&run, "id")).await.unwrap();
        assert_eq!(
            call(&s, &bearer, &json!({ "operation": "accounts" }))
                .await
                .unwrap_err()
                .status,
            401
        );
    }

    #[test]
    fn cli_arguments_cannot_inject_options_or_write_operations() {
        let fields = field_references(&json!({
            "fields": [{
                "id": "password",
                "value": "hidden-secret",
                "reference": "op://vault/item/password",
            }],
        }));
        assert!(!fields.to_string().contains("hidden-secret"));
        assert_eq!(fields["fields"][0]["reference"], "op://vault/item/password");
        let args = arguments(&json!({
            "operation": "fields",
            "vault": "vault",
            "item": "--help"
        }))
        .unwrap();
        assert_eq!(&args[4..], &["--", "--help"]);
        assert_eq!(
            crate::run_output::redact("ops_abcdefghijklmnopq", &[]),
            "[redacted]"
        );
        assert_eq!(
            crate::run_output::payload(&json!({ "OP_SERVICE_ACCOUNT_TOKEN": "short" }), &[])["OP_SERVICE_ACCOUNT_TOKEN"],
            "[redacted]"
        );
        for input in [
            json!({ "operation": "write" }),
            json!({
                "operation": "read",
                "reference": "--out-file=/tmp/leak"
            }),
            json!({ "operation": "items" }),
        ] {
            assert!(arguments(&input).is_err());
        }
        assert_eq!(
            arguments(&json!({
                "operation": "items",
                "vault": "--help"
            }))
            .unwrap()[2],
            "--vault=--help"
        );
    }
}
