use crate::{
    accounts::{Phase, Progress},
    config::{Config, now},
    error::{Error, Result},
    rpc::Session,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{path::Path, time::Duration};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

const LOGIN_LIFETIME: Duration = Duration::from_secs(15 * 60);

// Keep the CLI's auth lifecycle, but consume its protocol instead of terminal text.
pub(crate) async fn run(
    config: &Config,
    home: &Path,
    flow: &watch::Sender<Progress>,
    stop: &CancellationToken,
) -> Result<()> {
    let codex_home = home.join(".codex");
    let mut session = tokio::select! {
        () = stop.cancelled() => return Err(cancelled()),
        result = Session::codex(config, &codex_home, &[], None) => result?,
    };
    let mut login_id = None;
    let result = sign_in(&mut session, flow, stop, &mut login_id).await;
    if result.is_err()
        && let Some(id) = login_id
    {
        // Best effort: the app-server forgets the login when it exits anyway.
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            session
                .rpc
                .request("account/login/cancel", json!({ "loginId": id })),
        )
        .await;
    }
    session.close().await;
    result
}

async fn sign_in(
    session: &mut Session,
    flow: &watch::Sender<Progress>,
    stop: &CancellationToken,
    login_id: &mut Option<String>,
) -> Result<()> {
    // Session::request discards notifications while awaiting the reply.
    // Keep them queued here so an immediately completed login is not lost.
    let start = session.rpc.request(
        "account/login/start",
        json!({ "type": "chatgptDeviceCode" }),
    );
    let started = tokio::select! {
        () = stop.cancelled() => return Err(cancelled()),
        result = start => result?,
    };
    let challenge = Challenge::parse(&started)?;
    *login_id = Some(challenge.login_id.clone());
    flow.send_modify(|progress| {
        progress.phase = Phase::Authorizing;
        progress.code = Some(challenge.user_code.clone());
        progress.url = Some(challenge.verification_url.clone());
        progress.expires_at = Some(now() + LOGIN_LIFETIME.as_millis() as i64);
    });
    let deadline = tokio::time::sleep(LOGIN_LIFETIME);
    tokio::pin!(deadline);
    loop {
        let incoming = tokio::select! {
            () = stop.cancelled() => return Err(cancelled()),
            () = &mut deadline => {
                return Err(Error::timeout("Sign-in expired. Try again to get a new code."));
            }
            incoming = session.incoming.recv() => incoming,
        };
        let Some(incoming) = incoming else {
            return Err(Error::unavailable(
                "Codex disconnected during sign-in. Try again.",
            ));
        };
        if let Some(id) = incoming.id {
            session.rpc.reject(id).await?;
            continue;
        }
        let completed = incoming.method == "account/login/completed"
            && incoming.params["loginId"] == challenge.login_id.as_str();
        if !completed {
            continue;
        }
        if incoming.params["success"] == true {
            return Ok(());
        }
        // Provider errors can contain credentials or callback URLs.
        return Err(Error::bad(
            "Sign-in was not completed. Try again and approve access on the verification page.",
        ));
    }
}

fn cancelled() -> Error {
    Error::bad("Sign-in cancelled.")
}

/// The device code `account/login/start` answers with.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Challenge {
    #[serde(
        default,
        rename = "type",
        deserialize_with = "crate::accounts::lenient::string"
    )]
    kind: String,
    #[serde(default, deserialize_with = "crate::accounts::lenient::string")]
    login_id: String,
    #[serde(default, deserialize_with = "crate::accounts::lenient::string")]
    user_code: String,
    #[serde(default, deserialize_with = "crate::accounts::lenient::string")]
    verification_url: String,
}

impl Challenge {
    fn parse(value: &Value) -> Result<Self> {
        let challenge = Self::deserialize(value).unwrap_or_default();
        if !challenge.valid() {
            return Err(Error::bad_gateway(
                "Codex did not provide a valid sign-in code. Update Codex and try again.",
            ));
        }
        Ok(challenge)
    }

    fn valid(&self) -> bool {
        let code = &self.user_code;
        self.kind == "chatgptDeviceCode"
            && !self.login_id.is_empty()
            && self.login_id.len() <= 256
            && (8..=64).contains(&code.len())
            && code.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            && trusted_url(&self.verification_url)
    }
}

fn trusted_url(url: &str) -> bool {
    url::Url::parse(url).is_ok_and(|url| {
        url.scheme() == "https"
            && url.host_str() == Some("auth.openai.com")
            && url.port_or_known_default() == Some(443)
            && url.username().is_empty()
            && url.password().is_none()
            && url.path() == "/codex/device"
            && url.query().is_none()
            && url.fragment().is_none()
    })
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
            "verificationUrl": "https://auth.openai.com/codex/device",
        });
        for code in ["ABCD-1234", "ABCD-12345", "abcd-12345"] {
            value["userCode"] = code.into();
            assert_eq!(Challenge::parse(&value).unwrap().user_code, code);
        }
        for url in [
            "https://example.test/codex/device",
            "http://auth.openai.com/codex/device",
            "https://auth.openai.com@evil.test/codex/device",
            "https://auth.openai.com/codex/device?redirect=evil",
        ] {
            value["verificationUrl"] = url.into();
            assert!(Challenge::parse(&value).is_err());
        }
    }
}
