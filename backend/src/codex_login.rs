use crate::{
    config::{Config, now},
    error::{Error, Result},
    rpc::Session,
};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

const LOGIN_LIFETIME: Duration = Duration::from_secs(15 * 60);

// Keep the CLI's auth lifecycle, but consume its protocol instead of terminal text.
pub(crate) async fn run(
    config: &Config,
    home: &Path,
    flow: &watch::Sender<Value>,
    stop: &CancellationToken,
) -> Result<()> {
    let codex_home = home.join(".codex");
    let mut session = tokio::select! {
        _ = stop.cancelled() => return Err(cancelled()),
        result = Session::codex(config, &codex_home, &[], None) => result?,
    };
    let mut login_id = None;
    let result = async {
        // Session::request discards notifications while awaiting the reply.
        // Keep them queued here so an immediately completed login is not lost.
        let started = tokio::select! {
            _ = stop.cancelled() => return Err(cancelled()),
            result = session.rpc.request("account/login/start", json!({"type": "chatgptDeviceCode"})) => result?,
        };
        let (id, code, url) = challenge(&started)?;
        login_id = Some(id.to_owned());
        flow.send_modify(|value| {
            value["phase"] = "authorizing".into();
            value["code"] = code.into();
            value["url"] = url.into();
            value["expiresAt"] = (now() + LOGIN_LIFETIME.as_millis() as i64).into();
        });
        let deadline = tokio::time::sleep(LOGIN_LIFETIME);
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                _ = stop.cancelled() => return Err(cancelled()),
                _ = &mut deadline => return Err(Error::timeout("Sign-in expired. Try again to get a new code.")),
                incoming = session.incoming.recv() => {
                    let Some(incoming) = incoming else {
                        return Err(Error::unavailable("Codex disconnected during sign-in. Try again."));
                    };
                    if let Some(id) = incoming.id {
                        session.rpc.reject(id).await?;
                        continue;
                    }
                    if incoming.method != "account/login/completed" || incoming.params["loginId"] != id {
                        continue;
                    }
                    if incoming.params["success"] == true {
                        return Ok(());
                    }
                    // Provider errors can contain credentials or callback URLs.
                    return Err(Error::bad("Sign-in was not completed. Try again and approve access on the verification page."));
                }
            }
        }
    }.await;
    if result.is_err()
        && let Some(id) = login_id
    {
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            session.rpc.request(
                "account/login/cancel",
                json!({
                    "loginId": id
                }),
            ),
        )
        .await;
    }
    session.close().await;
    result
}

fn cancelled() -> Error {
    Error::bad("Sign-in cancelled.")
}

fn challenge(value: &Value) -> Result<(&str, &str, &str)> {
    let id = value["loginId"].as_str().unwrap_or_default();
    let code = value["userCode"].as_str().unwrap_or_default();
    let url = value["verificationUrl"].as_str().unwrap_or_default();
    let trusted_url = url::Url::parse(url).is_ok_and(|url| {
        url.scheme() == "https"
            && url.host_str() == Some("auth.openai.com")
            && url.port_or_known_default() == Some(443)
            && url.username().is_empty()
            && url.password().is_none()
            && url.path() == "/codex/device"
            && url.query().is_none()
            && url.fragment().is_none()
    });
    if value["type"] != "chatgptDeviceCode"
        || id.is_empty()
        || id.len() > 256
        || !(8..=64).contains(&code.len())
        || !code.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        || !trusted_url
    {
        return Err(Error::bad_gateway(
            "Codex did not provide a valid sign-in code. Update Codex and try again.",
        ));
    }
    Ok((id, code, url))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_structured_codes_without_assuming_terminal_format() {
        let mut value = json!({
            "type": "chatgptDeviceCode",
            "loginId": "fixture-login",
            "userCode": "ABCD-12345",
            "verificationUrl": "https://auth.openai.com/codex/device"
        });
        for code in ["ABCD-1234", "ABCD-12345", "abcd-12345"] {
            value["userCode"] = code.into();
            assert_eq!(challenge(&value).unwrap().1, code);
        }
        for url in [
            "https://example.test/codex/device",
            "http://auth.openai.com/codex/device",
            "https://auth.openai.com@evil.test/codex/device",
            "https://auth.openai.com/codex/device?redirect=evil",
        ] {
            value["verificationUrl"] = url.into();
            assert!(challenge(&value).is_err());
        }
    }
}
