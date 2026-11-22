//! Antigravity accounts: a Gemini API key, Google Cloud application-default credentials
//! with their project and region, or a Google account signed in through `agy`. Secrets
//! are encrypted on the manager and reach a run only through its broker. A Google
//! account's refresh token stays on the manager: runs receive access tokens only.
use super::{Account, Driver, Lease, Login, broker, remove_file};
use crate::{
    antigravity::{ADC_FILE, GEMINI, GOOGLE, GOOGLE_CLOUD, TOKEN_FILE, kind_name},
    auth::hex_digest,
    config::now,
    error::{Error, Result},
    provider::Provider,
    service::Service,
    skills::private_dir,
    validation::text,
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub(super) struct Antigravity;

fn secret_id(id: &str) -> String {
    format!("antigravity-account:{id}")
}

async fn stored_credentials(s: &Service, id: &str) -> Result<Value> {
    s.vault
        .get(&secret_id(id))
        .await?
        .ok_or_else(|| Error::bad("Replace this Antigravity account's credentials in Connections."))
}

/// A Google account token saved with less than this left is renewed before use.
const RENEW_BEFORE_MS: i64 = 15 * 60_000;

fn expiry(token: &Value) -> i64 {
    chrono::DateTime::parse_from_rfc3339(text(token, "expiry"))
        .map_or(0, |at| at.timestamp_millis())
}

fn token_home(s: &Service, id: &str) -> PathBuf {
    s.config.data_dir.join("antigravity-accounts").join(id)
}

/// Lets `agy` renew a Google account's access token on the manager: it refreshes a
/// saved token it considers expired and rewrites its token file. Cairn never needs
/// the CLI's OAuth client secret.
async fn renew(s: &Service, id: &str) -> Result<Value> {
    static RENEWING: std::sync::LazyLock<
        std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<tokio::sync::Mutex<()>>>>,
    > = std::sync::LazyLock::new(Default::default);
    let lock = RENEWING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entry(id.to_owned())
        .or_default()
        .clone();
    let _renewing = lock.lock().await;
    // Another request may have renewed the token while this one waited.
    let saved = stored_credentials(s, id).await?;
    if expiry(&saved["token"]["token"]) - now() >= RENEW_BEFORE_MS {
        return Ok(saved);
    }
    let saved = &saved;
    let home = token_home(s, id);
    let mut file = saved["token"].clone();
    file["token"]["expiry"] = "2000-01-01T00:00:00Z".into();
    crate::antigravity::prepare_native_home(&s.config, &home, None).await?;
    crate::antigravity::write_token(&home, &file).await?;
    let env = crate::antigravity::environment(&s.config, &home, None);
    let renewed = async {
        crate::process::bounded_output(
            crate::process::command(
                &s.config.antigravity_bin,
                &["models".into()],
                &env,
                Some(&home),
            ),
            Duration::from_secs(60),
            1_000_000,
        )
        .await?;
        let bytes = tokio::fs::read(home.join(TOKEN_FILE)).await?;
        Ok::<_, Error>(serde_json::from_slice::<Value>(&bytes)?)
    }
    .await;
    let _ = crate::accounts::remove_directory(&home).await;
    let renewed = renewed.map_err(|_| {
        Error::conflict("Reconnect this Antigravity Google account in Connections.")
    })?;
    if text(&renewed["token"], "access_token").is_empty() || expiry(&renewed["token"]) <= now() {
        return Err(Error::conflict(
            "Reconnect this Antigravity Google account in Connections.",
        ));
    }
    let mut credential = saved.clone();
    let mut token = renewed;
    // Google keeps the refresh token; keep ours if the CLI omitted it.
    if text(&token["token"], "refresh_token").is_empty() {
        token["token"]["refresh_token"] = saved["token"]["token"]["refresh_token"].clone();
    }
    credential["token"] = token;
    s.vault.set(&secret_id(id), &credential).await?;
    Ok(credential)
}

/// What a run or the manager's catalog uses: a Google account's current access token
/// without its refresh token, or the saved API key or Google Cloud credentials.
pub(crate) async fn credentials(s: &Service, id: &str) -> Result<Value> {
    let mut credential = stored_credentials(s, id).await?;
    if credential["kind"] != GOOGLE {
        return Ok(credential);
    }
    if expiry(&credential["token"]["token"]) - now() < RENEW_BEFORE_MS {
        credential = renew(s, id).await?;
    }
    if let Some(token) = credential["token"]["token"].as_object_mut() {
        token.remove("refresh_token");
    }
    Ok(credential)
}

/// Reads the token `agy` saved after signing in within `home`.
async fn signed_in_token(home: &Path) -> Result<Value> {
    let bytes = tokio::fs::read(home.join(TOKEN_FILE))
        .await
        .map_err(|_| Error::bad(SIGN_IN_FAILED))?;
    let token: Value = serde_json::from_slice(&bytes).map_err(|_| Error::bad(SIGN_IN_FAILED))?;
    if text(&token["token"], "access_token").is_empty()
        || text(&token["token"], "refresh_token").is_empty()
        || text(&token, "id_token_email").is_empty()
    {
        return Err(Error::bad(SIGN_IN_FAILED));
    }
    Ok(token)
}

const SIGN_IN_FAILED: &str = "Google sign-in for Antigravity did not complete. Start again.";

fn login_url(output: &str) -> Option<String> {
    output
        .split_whitespace()
        .filter_map(|s| url::Url::parse(s).ok())
        .find(|u| {
            u.scheme() == "https"
                && u.host_str() == Some("accounts.google.com")
                && u.path().starts_with("/o/oauth2/")
                && u.username().is_empty()
        })
        .map(String::from)
}

fn region(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn project(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-.:_".contains(c))
}

/// Validates what the user pasted for an account kind and returns the vault credential
/// with the identity that detects the same credential connected twice.
pub(crate) fn credential(kind: &str, secret: &str) -> Result<(Value, String)> {
    let secret = secret.trim();
    match kind {
        GEMINI => {
            if secret.is_empty()
                || secret.len() > 400
                || secret.chars().any(|c| c.is_whitespace() || c.is_control())
            {
                return Err(Error::bad("Enter a valid Gemini API key."));
            }
            Ok((
                json!({ "kind": GEMINI, "key": secret }),
                hex_digest(&format!("{GEMINI}:{secret}")),
            ))
        }
        GOOGLE_CLOUD => {
            let invalid = || {
                Error::bad(
                    "Enter Google Cloud credentials as a JSON object with `credentials` (a service account or authorized_user file), `project` and `location`.",
                )
            };
            if secret.len() > 64_000 {
                return Err(invalid());
            }
            let value: Value = serde_json::from_str(secret).map_err(|_| invalid())?;
            let file = &value["credentials"];
            let identity = match text(file, "type") {
                "service_account"
                    if !text(file, "private_key").is_empty()
                        && !text(file, "client_email").is_empty() =>
                {
                    text(file, "client_email")
                }
                "authorized_user"
                    if !text(file, "refresh_token").is_empty()
                        && !text(file, "client_id").is_empty() =>
                {
                    text(file, "refresh_token")
                }
                _ => return Err(invalid()),
            };
            let project_id = match text(&value, "project") {
                "" if text(file, "project_id").is_empty() => text(file, "quota_project_id"),
                "" => text(file, "project_id"),
                project => project,
            };
            let location = match text(&value, "location") {
                "" => "global",
                location => location,
            };
            if !project(project_id) || !region(location) {
                return Err(invalid());
            }
            Ok((
                json!({
                    "kind": GOOGLE_CLOUD,
                    "credentials": file,
                    "project": project_id,
                    "location": location,
                }),
                hex_digest(&format!("{GOOGLE_CLOUD}:{identity}:{project_id}")),
            ))
        }
        _ => Err(Error::bad("Choose a supported Antigravity account type.")),
    }
}

/// Values that must never appear in run output.
fn secrets(credential: &Value) -> Vec<String> {
    let file = &credential["credentials"];
    let token = &credential["token"]["token"];
    let mut values = Vec::new();
    for value in [
        text(credential, "key"),
        text(token, "access_token"),
        text(token, "refresh_token"),
        text(file, "private_key"),
        text(file, "refresh_token"),
        text(file, "client_secret"),
    ] {
        if value.is_empty() {
            continue;
        }
        values.push(value.to_owned());
        // A multi-line key also appears JSON-escaped, as in the credential file.
        let escaped = serde_json::to_string(value).unwrap_or_default();
        let escaped = escaped.trim_matches('"');
        if escaped != value {
            values.push(escaped.to_owned());
        }
        // Each line of a private key is secret on its own when output is wrapped.
        if value.contains('\n') {
            values.extend(
                value
                    .lines()
                    .map(str::trim)
                    .filter(|line| line.len() >= 16 && !line.starts_with("-----"))
                    .map(str::to_owned),
            );
        }
    }
    values
}

/// The credentials a broker response hands to a run, as the run can print them.
pub(crate) fn delivered_redactions(response: &Value) -> Vec<String> {
    if !matches!(text(response, "kind"), GEMINI | GOOGLE_CLOUD | GOOGLE) {
        return Vec::new();
    }
    secrets(response)
}

#[async_trait::async_trait]
impl Driver for Antigravity {
    fn accepts_api_keys(&self) -> bool {
        true
    }

    async fn api_key_providers(&self, _s: &Service) -> Result<Value> {
        Ok(json!([
            { "id": GEMINI, "name": kind_name(GEMINI) },
            { "id": GOOGLE_CLOUD, "name": kind_name(GOOGLE_CLOUD) },
        ]))
    }

    async fn connect_api_key(
        &self,
        s: &Service,
        account: &Account,
        model_provider: &str,
        api_key: &str,
    ) -> Result<()> {
        let (credential, identity) = credential(model_provider, api_key)?;
        if let Some(previous) = account.model_provider.as_deref()
            && previous != model_provider
        {
            return Err(Error::bad(
                "Add a separate account for a different Antigravity account type.",
            ));
        }
        let mut account = account.clone();
        account.identity = Some(identity);
        account.model_provider = Some(model_provider.into());
        account.model_provider_name = Some(kind_name(model_provider).into());
        account.plan = Some(
            match model_provider {
                GOOGLE_CLOUD => "Google Cloud credentials",
                _ => "API key",
            }
            .into(),
        );
        account.usage = None;
        account.exhausted = None;
        account.mark_ready();
        let vault = s.vault.clone();
        s.store
            .transaction(move |db| {
                if super::stored(db)?.iter().any(|a| {
                    a.id != account.id
                        && a.provider == Provider::Antigravity
                        && a.identity == account.identity
                }) {
                    return Err(Error::conflict(
                        "These credentials are already connected. Edit the existing account instead.",
                    ));
                }
                vault.set_in(db, &secret_id(&account.id), &credential)?;
                db.save_coding_account(&account)?;
                db.audit(
                    "account.api-key-saved",
                    &json!({
                        "id": account.id,
                        "provider": account.provider,
                    }),
                )
            })
            .await
    }

    async fn initialize(&self, _s: &Service) -> Result<()> {
        Ok(())
    }

    async fn managed(&self, _s: &Service) -> Result<bool> {
        Ok(true)
    }

    fn signs_in(&self, account: Option<&Account>) -> bool {
        account.is_none_or(|a| {
            a.model_provider
                .as_deref()
                .is_none_or(|kind| kind == GOOGLE)
        })
    }

    /// `agy acp --login` prints Google's link, then reads the pasted code from stdin.
    async fn authorize(&self, s: &Service, home: &Path, login: &Login) -> Result<()> {
        crate::antigravity::prepare_native_home(&s.config, home, None).await?;
        let env = crate::antigravity::environment(&s.config, home, None);
        let mut command = crate::process::command(
            &s.config.antigravity_bin,
            &["acp".into(), "--login".into()],
            &env,
            Some(home),
        );
        command.stdin(std::process::Stdio::piped());
        let mut child = command.spawn().map_err(|_| {
            Error::unavailable("The Antigravity CLI is not installed. Install it on the server.")
        })?;
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let mut output = String::new();
        let (mut a, mut b) = ([0u8; 4096], [0u8; 4096]);
        let (mut stdout_open, mut stderr_open) = (true, true);
        let deadline = tokio::time::sleep(Duration::from_secs(900));
        tokio::pin!(deadline);
        let result = loop {
            tokio::select! {
                () = login.stop.cancelled() => break Err(Error::conflict("Sign-in cancelled.")),
                () = s.shutdown.cancelled() => break Err(Error::conflict("Sign-in cancelled.")),
                () = &mut deadline => {
                    break Err(Error::timeout("Sign-in expired. Start again to get a new link."));
                }
                code = login.code() => {
                    if let Some(code) = code {
                        stdin.write_all(code.trim().as_bytes()).await?;
                        stdin.write_all(b"\n").await?;
                    }
                }
                status = child.wait() => break Ok(status?.success()),
                n = stdout.read(&mut a), if stdout_open => {
                    let n = n?;
                    stdout_open = n > 0;
                    output.push_str(&String::from_utf8_lossy(&a[..n]));
                }
                n = stderr.read(&mut b), if stderr_open => {
                    let n = n?;
                    stderr_open = n > 0;
                    output.push_str(&String::from_utf8_lossy(&b[..n]));
                }
            }
            if output.len() > 64_000 {
                break Err(Error::bad_gateway(
                    "Antigravity sign-in returned too much output.",
                ));
            }
            if let Some(url) = login_url(&output) {
                login.update(|v| {
                    v.phase = super::Phase::Authorizing;
                    v.url = Some(url);
                    v.accepts_code = true;
                });
            }
        };
        let _ = child.kill().await;
        let _ = child.wait().await;
        match result {
            // CLI errors can contain private state. Never forward them.
            Ok(true) => signed_in_token(home).await.map(|_| ()),
            Ok(false) => Err(Error::bad(SIGN_IN_FAILED)),
            Err(error) => Err(error),
        }
    }

    async fn adopt(&self, s: &Service, id: &str, home: &Path) -> Result<()> {
        let token = signed_in_token(home).await?;
        let email = text(&token, "id_token_email").to_owned();
        let mut account = s.accounts.account(s, id).await?;
        let fingerprint = hex_digest(&format!("{GOOGLE}:{}", email.to_lowercase()));
        if account.identity.as_ref().is_some_and(|i| *i != fingerprint) {
            return Err(super::different_account());
        }
        account.identity = Some(fingerprint);
        account.email = Some(email);
        account.model_provider = Some(GOOGLE.into());
        account.model_provider_name = Some(kind_name(GOOGLE).into());
        account.plan = Some("Google account".into());
        account.usage = None;
        account.exhausted = None;
        account.mark_ready();
        super::claim(s, account).await?;
        s.vault
            .set(&secret_id(id), &json!({ "kind": GOOGLE, "token": token }))
            .await?;
        let _ = remove_file(&home.join(TOKEN_FILE)).await;
        Ok(())
    }

    /// Antigravity exposes no readable quota for these credentials.
    async fn refresh(&self, s: &Service, id: &str, _models: &[String]) -> Result<()> {
        credentials(s, id).await?;
        let mut account = s.accounts.account(s, id).await?;
        account.mark_ready();
        super::save(s, &account).await
    }

    /// A Google account is also checked periodically: renewing its token is how a revoked
    /// sign-in is found before a run needs it.
    async fn due(&self, _s: &Service, account: &Account, attempted: i64) -> Result<bool> {
        let google = account.model_provider.as_deref() == Some(GOOGLE);
        Ok((google || account.exhausted.is_some()) && now() - attempted >= self.fresh_for())
    }

    fn fresh_for(&self) -> i64 {
        300_000
    }

    fn requires_usage(&self) -> bool {
        false
    }

    async fn supports(&self, s: &Service, id: &str, model: &str, _speed: &str) -> Result<bool> {
        if model.is_empty() {
            return Ok(true);
        }
        let account = s.accounts.account(s, id).await?;
        let Some(kind) = account.model_provider.as_deref() else {
            return Ok(false);
        };
        Ok(s.antigravity.offers(kind, model))
    }

    async fn warm(&self, s: &Service, _model: &str) -> Result<()> {
        // Without an inventory no account supports a model yet, so the run waits and retries.
        static FAILED_AT: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
        let failed_at = FAILED_AT.load(std::sync::atomic::Ordering::Relaxed);
        if now() - failed_at >= 30_000
            && let Err(error) = s.antigravity.models(s).await
        {
            FAILED_AT.store(now(), std::sync::atomic::Ordering::Relaxed);
            tracing::warn!(error = %error.message, "Antigravity model catalog unavailable");
        }
        Ok(())
    }

    fn ordered(&self) -> bool {
        true
    }

    fn home(&self, s: &Service, run_id: &str) -> PathBuf {
        s.config
            .data_dir
            .join("runs")
            .join(run_id)
            .join("home/.antigravity")
    }

    async fn prepare(&self, _s: &Service, _id: &str, home: &Path) -> Result<()> {
        private_dir(home).await
    }

    async fn clear(&self, home: &Path) -> Result<()> {
        // Side-question copies hold the run's credentials too.
        super::remove_directory(&home.join(crate::antigravity::SIDE_QUESTIONS)).await?;
        remove_file(&home.join(ADC_FILE)).await?;
        remove_file(&home.join(TOKEN_FILE)).await
    }

    async fn recover(&self, s: &Service, run_id: &str) -> Result<()> {
        self.clear(&self.home(s, run_id)).await
    }

    async fn access(
        &self,
        s: &Service,
        lease: &Lease,
        _request: &broker::Request,
    ) -> Result<Value> {
        credentials(s, &lease.account_id).await
    }

    async fn redactions(&self, s: &Service, id: &str) -> Result<Vec<String>> {
        Ok(secrets(&stored_credentials(s, id).await?))
    }

    async fn forget(&self, s: &Service, id: &str) -> Result<()> {
        s.vault.delete(&secret_id(id)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service_account() -> Value {
        json!({
            "type": "service_account",
            "project_id": "synthetic-project",
            "private_key": "-----BEGIN PRIVATE KEY-----\nsynthetic\n-----END PRIVATE KEY-----\n",
            "client_email": "agent@synthetic-project.iam.gserviceaccount.com",
        })
    }

    #[test]
    fn sign_in_links_must_be_google_oauth_pages() {
        let output = "Open this link in a browser to log in with Google:\n\n  https://accounts.google.com/o/oauth2/auth?client_id=x&state=y\n";
        assert_eq!(
            login_url(output).unwrap(),
            "https://accounts.google.com/o/oauth2/auth?client_id=x&state=y"
        );
        assert!(login_url("https://accounts.example.com/o/oauth2/auth").is_none());
        assert!(login_url("https://accounts.google.com/signin").is_none());
    }

    #[test]
    fn google_accounts_sign_in_and_key_accounts_do_not() {
        let driver = Antigravity;
        let mut account = Account::new(
            Provider::Antigravity,
            "a",
            super::super::AccountState::Pending,
        );
        assert!(driver.signs_in(None));
        assert!(driver.signs_in(Some(&account)));
        account.model_provider = Some(GOOGLE.into());
        assert!(driver.signs_in(Some(&account)));
        account.model_provider = Some(GEMINI.into());
        assert!(!driver.signs_in(Some(&account)));
    }

    #[test]
    fn access_tokens_and_refresh_tokens_are_redacted() {
        let google = json!({ "kind": GOOGLE, "token": { "token": { "access_token": "ya29.a", "refresh_token": "1//r" } } });
        assert_eq!(secrets(&google), ["ya29.a", "1//r"]);
        // A delivered Google Cloud key is redacted raw, JSON-escaped and line by line.
        let key = "-----BEGIN PRIVATE KEY-----\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASC\n-----END PRIVATE KEY-----\n";
        let cloud = json!({ "kind": GOOGLE_CLOUD, "credentials": { "private_key": key } });
        let values = delivered_redactions(&cloud);
        assert!(values.contains(&key.to_owned()));
        assert!(values.iter().any(|v| v.contains("\\n")));
        assert!(values.contains(&"MIIEvQIBADANBgkqhkiG9w0BAQEFAASC".to_owned()));
        assert!(delivered_redactions(&json!({ "accessToken": "codex" })).is_empty());
    }

    #[test]
    fn gemini_keys_are_single_tokens() {
        let (stored, identity) = credential(GEMINI, " synthetic-key ").unwrap();
        assert_eq!(stored, json!({ "kind": "gemini", "key": "synthetic-key" }));
        assert_eq!(identity, credential_identity(GEMINI, "synthetic-key"));
        assert!(credential(GEMINI, "").is_err());
        assert!(credential(GEMINI, "two words").is_err());
        assert!(credential("openai", "key").is_err());
    }

    fn credential_identity(kind: &str, secret: &str) -> String {
        hex_digest(&format!("{kind}:{secret}"))
    }

    #[test]
    fn google_cloud_credentials_need_a_credential_file_project_and_region() {
        let pasted = json!({ "credentials": service_account(), "location": "us-central1" });
        let (stored, _) = credential(GOOGLE_CLOUD, &pasted.to_string()).unwrap();
        assert_eq!(stored["project"], "synthetic-project");
        assert_eq!(stored["location"], "us-central1");
        assert_eq!(stored["credentials"]["type"], "service_account");

        let user = json!({
            "credentials": {
                "type": "authorized_user",
                "client_id": "id",
                "client_secret": "secret",
                "refresh_token": "refresh",
                "quota_project_id": "quota-project",
            },
        });
        let (stored, _) = credential(GOOGLE_CLOUD, &user.to_string()).unwrap();
        assert_eq!(stored["project"], "quota-project");
        assert_eq!(stored["location"], "global");
        assert_eq!(secrets(&stored), ["refresh", "secret"]);

        for invalid in [
            "not json".to_owned(),
            json!({ "credentials": { "type": "external_account" } }).to_string(),
            json!({ "credentials": service_account(), "location": "Bad Region" }).to_string(),
            json!({ "credentials": { "type": "authorized_user", "client_id": "id", "refresh_token": "r" } }).to_string(),
        ] {
            assert!(credential(GOOGLE_CLOUD, &invalid).is_err(), "{invalid}");
        }
    }
}
