//! Direct authorization and signaling over the existing authenticated tunnel.
//! The WebRTC peer consumes this interface; no anonymous local listener is added.
use crate::{
    auth::{InstallationIdentity, InstallationRole},
    error::{Error, Result},
};
use leo_relay_protocol::{
    Frame, Role,
    direct::{
        DirectAuthorization, DirectClaims, DirectRevocation, DirectSignal, DirectVerifier,
        MAX_DIRECT_CONNECTIONS, MAX_DIRECT_MEMBER_CONNECTIONS, MAX_DIRECT_PER_ACCOUNT,
        SignalBudget, unix_time,
    },
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug)]
pub enum DirectEvent {
    Signal { id: String, signal: DirectSignal },
    Revoked(DirectRevocation),
}

#[derive(Clone)]
pub struct DirectLease {
    pub claims: DirectClaims,
    pub closed: CancellationToken,
}

impl DirectLease {
    pub fn identity(&self) -> InstallationIdentity {
        InstallationIdentity::trusted(
            match self.claims.role {
                Role::Owner => InstallationRole::Owner,
                Role::Member => InstallationRole::Member,
            },
            &self.claims.account_id,
        )
    }
}

struct Authorized {
    authorization: DirectAuthorization,
    closed: CancellationToken,
    connected: bool,
}

#[derive(Default)]
struct State {
    verifier: Option<DirectVerifier>,
    authorizations: HashMap<String, Authorized>,
    output: Option<mpsc::Sender<Frame>>,
    signals: SignalBudget,
}

#[derive(Clone)]
pub struct DirectConnections {
    state: Arc<Mutex<State>>,
    events: broadcast::Sender<DirectEvent>,
}

impl Default for DirectConnections {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(State::default())),
            events: broadcast::channel(64).0,
        }
    }
}

impl DirectConnections {
    pub fn subscribe(&self) -> broadcast::Receiver<DirectEvent> {
        self.events.subscribe()
    }

    pub(crate) fn attach(&self, output: mpsc::Sender<Frame>) {
        self.state.lock().unwrap().output = Some(output);
    }

    pub(crate) fn detach(&self) {
        let mut state = self.state.lock().unwrap();
        state.output = None;
        state.verifier = None;
        // Established peers retain only their finite lease while the tunnel is down.
    }

    pub(crate) fn set_key(&self, installation: &str, public_key: &str) -> Result<()> {
        let verifier =
            DirectVerifier::new(installation.to_owned(), public_key).map_err(Error::bad)?;
        let mut state = self.state.lock().unwrap();
        for (_, authorization) in state.authorizations.drain() {
            authorization.closed.cancel();
        }
        state.verifier = Some(verifier);
        Ok(())
    }

    pub(crate) fn authorize(&self, authorization: DirectAuthorization, renewal: bool) -> bool {
        let mut state = self.state.lock().unwrap();
        let Some(verifier) = state.verifier.as_mut() else {
            return false;
        };
        let Ok(claims) = verifier.verify(&authorization) else {
            return false;
        };
        state.authorizations.retain(|_, lease| {
            if lease.authorization.claims.expires_at <= unix_time() {
                lease.closed.cancel();
            }
            !lease.closed.is_cancelled()
        });
        let existing = state.authorizations.get(&claims.connection_id);
        let (closed, connected) = if renewal {
            let Some(existing) = existing else {
                return false;
            };
            let previous = &existing.authorization.claims;
            if previous.account_id != claims.account_id
                || previous.session_id != claims.session_id
                || previous.role != claims.role
                || previous.generation != claims.generation
                || previous.fingerprint != claims.fingerprint
            {
                return false;
            }
            (existing.closed.clone(), existing.connected)
        } else {
            if existing.is_some()
                || state.authorizations.len() >= MAX_DIRECT_CONNECTIONS
                || claims.role == Role::Member
                    && state
                        .authorizations
                        .values()
                        .filter(|lease| lease.authorization.claims.role == Role::Member)
                        .count()
                        >= MAX_DIRECT_MEMBER_CONNECTIONS
                || state
                    .authorizations
                    .values()
                    .filter(|lease| lease.authorization.claims.account_id == claims.account_id)
                    .count()
                    >= MAX_DIRECT_PER_ACCOUNT
            {
                return false;
            }
            (CancellationToken::new(), false)
        };
        let id = claims.connection_id.clone();
        let nonce = claims.nonce.clone();
        let deadline = claims.expires_at;
        state.authorizations.insert(
            id.clone(),
            Authorized {
                authorization,
                closed: closed.clone(),
                connected,
            },
        );
        drop(state);

        let state = Arc::downgrade(&self.state);
        tokio::spawn(async move {
            tokio::select! {
                () = closed.cancelled() => {},
                () = tokio::time::sleep(leo_relay_protocol::direct::until_expiry(deadline)) => {
                    if let Some(state) = state.upgrade() {
                        let mut state = state.lock().unwrap();
                        let current_nonce = state.authorizations.get(&id)
                            .map(|lease| &lease.authorization.claims.nonce);
                        if current_nonce == Some(&nonce) {
                            closed.cancel();
                            state.authorizations.remove(&id);
                        }
                    }
                }
            }
        });
        true
    }

    /// Call once after DTLS using the certificate fingerprint observed by the peer.
    /// Never use a client-supplied HTTP/header identity to dispatch direct frames.
    pub fn accept_peer(
        &self,
        authorization: &DirectAuthorization,
        session: &str,
        fingerprint: &str,
    ) -> Result<DirectLease> {
        let mut state = self.state.lock().unwrap();
        if state.output.is_none() || state.verifier.is_none() {
            return Err(Error::unauthorized("Official tunnel unavailable."));
        }
        let Some(lease) = state
            .authorizations
            .get_mut(&authorization.claims.connection_id)
        else {
            return Err(Error::unauthorized("Unknown direct authorization."));
        };
        if lease.authorization != *authorization
            || lease.connected
            || lease.closed.is_cancelled()
            || lease.authorization.claims.expires_at <= unix_time()
            || lease.authorization.claims.session_id != session
            || lease.authorization.claims.fingerprint != fingerprint
        {
            return Err(Error::unauthorized("Invalid direct peer."));
        }
        lease.connected = true;
        Ok(DirectLease {
            claims: lease.authorization.claims.clone(),
            closed: lease.closed.clone(),
        })
    }

    pub(crate) fn revoke(&self, scope: DirectRevocation) {
        let mut state = self.state.lock().unwrap();
        if let Some(verifier) = state.verifier.as_mut() {
            verifier.revoke(&scope);
        }
        state.authorizations.retain(|_, lease| {
            let claims = &lease.authorization.claims;
            let matches = match &scope {
                DirectRevocation::Account { account_id, .. } => claims.account_id == *account_id,
                DirectRevocation::Session { session_id } => claims.session_id == *session_id,
                DirectRevocation::Installation => true,
            };
            if matches {
                lease.closed.cancel();
            }
            !matches
        });
        let _ = self.events.send(DirectEvent::Revoked(scope));
    }

    pub(crate) fn receive_signal(&self, id: String, signal: DirectSignal) -> bool {
        let mut state = self.state.lock().unwrap();
        let allowed = signal.valid()
            && state.authorizations.get(&id).is_some_and(|lease| {
                signal.matches_client_fingerprint(&lease.authorization.claims.fingerprint)
            })
            && Self::signal_allowed(&mut state, &id);
        drop(state);
        if allowed {
            let _ = self.events.send(DirectEvent::Signal { id, signal });
        }
        allowed
    }

    /// Return connection metadata to the originating official session via the tunnel.
    pub fn send_signal(&self, id: &str, signal: DirectSignal) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        if !signal.valid() || !Self::signal_allowed(&mut state, id) {
            return Err(Error::bad("Invalid direct signal."));
        }
        let output = state
            .output
            .as_ref()
            .ok_or_else(|| Error::unavailable("Official tunnel unavailable."))?;
        output
            .try_send(Frame::DirectSignal {
                id: id.to_owned(),
                signal,
                request_id: None,
            })
            .map_err(|_| Error::unavailable("Signaling busy."))
    }

    fn signal_allowed(state: &mut State, id: &str) -> bool {
        if state.output.is_none() {
            return false;
        }
        let Some(lease) = state.authorizations.get(id) else {
            return false;
        };
        !lease.closed.is_cancelled()
            && lease.authorization.claims.expires_at > unix_time()
            && state.signals.consume(&lease.authorization.claims)
    }
}
