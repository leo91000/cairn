//! A sign-in in progress: what its driver publishes and what the user sees of it.
use super::{Driver, SIGN_IN_MS, remove_directory};
use crate::{
    config::now,
    error::{Error, Result},
    provider::Provider,
    service::Service,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};
use tokio::sync::{Mutex, mpsc, watch};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SignInState {
    Pending,
    Complete,
    Failed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Starting,
    Authorizing,
    Verifying,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub state: SignInState,
    pub phase: Phase,
    pub url: Option<String>,
    /// A code to enter on the sign-in page (Codex).
    pub code: Option<String>,
    /// Whether the sign-in page shows a code to paste back here (Claude Code).
    pub accepts_code: bool,
    pub expires_at: Option<i64>,
    pub error: Option<String>,
}

impl Progress {
    fn new() -> Self {
        Self {
            state: SignInState::Pending,
            phase: Phase::Starting,
            url: None,
            code: None,
            accepts_code: false,
            expires_at: Some(now() + SIGN_IN_MS),
            error: None,
        }
    }

    /// The link and code are only shown while the CLI waits for them.
    fn hide_challenge(&mut self) {
        self.url = None;
        self.code = None;
        self.accepts_code = false;
    }

    fn finish(&mut self, result: &Result<()>, stopped: bool) {
        self.hide_challenge();
        self.expires_at = None;
        self.state = match result {
            Ok(()) => SignInState::Complete,
            Err(_) if stopped => SignInState::Cancelled,
            Err(_) => SignInState::Failed,
        };
        if let Err(error) = result
            && !stopped
        {
            self.error = Some(error.message.clone());
        }
    }
}

/// A sign-in in progress, as seen by a driver.
pub struct Login {
    pub(crate) view: watch::Sender<Progress>,
    pub stop: CancellationToken,
    codes: Mutex<mpsc::Receiver<String>>,
}

impl Login {
    pub fn update(&self, change: impl FnOnce(&mut Progress)) {
        self.view.send_modify(change);
    }

    /// The next authorization code the user pastes, for CLIs that ask for one.
    pub async fn code(&self) -> Option<String> {
        self.codes.lock().await.recv().await
    }

    fn accepts_code(&self) -> bool {
        self.view.borrow().accepts_code
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct View<'a> {
    account_id: &'a str,
    provider: Provider,
    #[serde(flatten)]
    progress: Progress,
}

pub(super) struct SignIn {
    pub account_id: String,
    pub provider: Provider,
    pub login: Arc<Login>,
    input: mpsc::Sender<String>,
    pub complete: watch::Receiver<bool>,
    /// An account that never finished signing in is removed if the user gives up.
    pub created: bool,
}

impl SignIn {
    pub fn busy(&self) -> bool {
        !*self.complete.borrow()
    }

    pub fn view(&self) -> Value {
        let view = View {
            account_id: &self.account_id,
            provider: self.provider,
            progress: self.login.view.borrow().clone(),
        };
        serde_json::to_value(view).unwrap_or(Value::Null)
    }

    pub fn waits_for_code(&self) -> bool {
        self.busy() && self.login.accepts_code()
    }

    pub fn submit(&self, code: &str) -> Result<()> {
        self.input
            .try_send(code.into())
            .map_err(|_| Error::conflict("A code is already being checked."))
    }
}

/// Starts signing in to account `id` inside `home` in the background.
pub(super) fn start(
    s: &Arc<Service>,
    id: String,
    provider: Provider,
    home: PathBuf,
    created: bool,
) -> SignIn {
    let (input, codes) = mpsc::channel(1);
    let (view, _) = watch::channel(Progress::new());
    let login = Arc::new(Login {
        view,
        stop: CancellationToken::new(),
        codes: Mutex::new(codes),
    });
    let (done, complete) = watch::channel(false);
    let sign_in = SignIn {
        account_id: id.clone(),
        provider,
        login: login.clone(),
        input,
        complete,
        created,
    };
    let s = s.clone();
    tokio::spawn(async move {
        let result = authorize(&s, provider.driver(), &id, &home, &login).await;
        let stopped = login.stop.is_cancelled() || s.shutdown.is_cancelled();
        if result.is_ok()
            && let Err(error) = s
                .store
                .audit(
                    "account.connected",
                    json!({ "id": id, "provider": provider }),
                )
                .await
        {
            tracing::warn!(%error, "could not audit account sign-in");
        }
        if let Err(error) = remove_directory(&home).await {
            tracing::warn!(%error, "could not remove sign-in directory");
        }
        *s.accounts.connecting.lock().await = None;

        // A terminal view lets clients immediately select, reconnect or remove the
        // account. Finish cleanup and release its fence before publishing it. Nobody
        // may be waiting for the sign-in anymore.
        let _ = done.send(true);
        login.update(|v| v.finish(&result, stopped));
    });
    sign_in
}

async fn authorize(
    s: &Service,
    driver: &dyn Driver,
    id: &str,
    home: &std::path::Path,
    login: &Login,
) -> Result<()> {
    driver.authorize(s, home, login).await?;
    login.update(|v| {
        v.phase = Phase::Verifying;
        v.hide_challenge();
    });
    // Keep selection fenced until the identity is verified and credentials saved.
    let _selection = s.accounts.selection.lock().await;
    let _guard = s.accounts.lock(id).await;
    // Drivers report sign-in errors without provider details; verification can fail for
    // internal reasons worth hiding.
    driver.adopt(s, id, home).await.map_err(|error| {
        if error.status < 500 {
            error
        } else {
            Error::new(error.status, "Unable to complete sign-in. Try again.")
        }
    })
}
