//! Coding-agent account policies and transport adapters.
use super::{ResumeContext, decode};
use crate::{
    error::{Error, Result},
    http::Input,
    service::Service,
    validation::uuid,
};
use cairn_contracts::*;
use serde_json::{Value, json};
use std::sync::Arc;

pub(crate) struct AccountOperation {
    pub name: &'static str,
    scope: &'static str,
}

pub(crate) const ACCOUNT_OPERATIONS: &[AccountOperation] = &[
    AccountOperation {
        name: "list_models",
        scope: "read",
    },
    AccountOperation {
        name: "get_coding_agent_account_sign_in",
        scope: "read",
    },
    AccountOperation {
        name: "list_coding_agent_account_providers",
        scope: "read",
    },
    AccountOperation {
        name: "list_coding_agent_accounts",
        scope: "read",
    },
    AccountOperation {
        name: "replace_coding_agent_account",
        scope: "manage",
    },
    AccountOperation {
        name: "order_coding_agent_accounts",
        scope: "manage",
    },
    AccountOperation {
        name: "refresh_coding_agent_accounts",
        scope: "manage",
    },
    AccountOperation {
        name: "connect_coding_agent_account",
        scope: "manage",
    },
    AccountOperation {
        name: "reconnect_coding_agent_account",
        scope: "manage",
    },
    AccountOperation {
        name: "replace_coding_agent_account_key",
        scope: "manage",
    },
    AccountOperation {
        name: "submit_coding_agent_account_code",
        scope: "manage",
    },
    AccountOperation {
        name: "cancel_coding_agent_account_sign_in",
        scope: "manage",
    },
    AccountOperation {
        name: "delete_coding_agent_account",
        scope: "manage",
    },
];

pub(crate) fn account_operation(name: &str) -> Option<&'static AccountOperation> {
    ACCOUNT_OPERATIONS.iter().find(|op| op.name == name)
}

impl AccountOperation {
    pub(crate) fn descriptor(&self) -> Value {
        let input = match self.name {
            "list_models" => schema_for::<ListModelsInput>(Contract::Deserialize),
            "list_coding_agent_account_providers" => {
                schema_for::<AccountProviderInput>(Contract::Deserialize)
            }
            "replace_coding_agent_account" => {
                schema_for::<ReplaceAccountInput>(Contract::Deserialize)
            }
            "order_coding_agent_accounts" => {
                schema_for::<OrderAccountsInput>(Contract::Deserialize)
            }
            "connect_coding_agent_account" => {
                schema_for::<ConnectAccountInput>(Contract::Deserialize)
            }
            "replace_coding_agent_account_key" => {
                schema_for::<ReplaceAccountKeyInput>(Contract::Deserialize)
            }
            "submit_coding_agent_account_code" => {
                schema_for::<AccountCodeInput>(Contract::Deserialize)
            }
            "reconnect_coding_agent_account" | "delete_coding_agent_account" => {
                schema_for::<AccountInput>(Contract::Deserialize)
            }
            _ => schema_for::<ListAccountsInput>(Contract::Deserialize),
        };
        let output = match self.name {
            "list_models" => schema_for::<ModelCatalog>(Contract::Serialize),
            "get_coding_agent_account_sign_in" => {
                schema_for::<AccountSignInProgressOutput>(Contract::Serialize)
            }
            "refresh_coding_agent_accounts" => {
                schema_for::<RefreshAccountsOutput>(Contract::Serialize)
            }
            "order_coding_agent_accounts" => schema_for::<OrderAccountsOutput>(Contract::Serialize),
            "list_coding_agent_account_providers" => {
                schema_for::<AccountProvidersOutput>(Contract::Serialize)
            }
            "replace_coding_agent_account" => schema_for::<CodingAgentAccount>(Contract::Serialize),
            "connect_coding_agent_account"
            | "reconnect_coding_agent_account"
            | "replace_coding_agent_account_key" => {
                schema_for::<ConnectAccountOutput>(Contract::Serialize)
            }
            "submit_coding_agent_account_code"
            | "cancel_coding_agent_account_sign_in"
            | "delete_coding_agent_account" => {
                schema_for::<AccountMutationOutput>(Contract::Serialize)
            }
            _ => schema_for::<CodingAgentAccounts>(Contract::Serialize),
        };
        let description = match self.name {
            "list_models" => {
                "List the models a coding agent offers on its enabled accounts: reasoning levels and, for Codex, the speeds (service tiers) each model offers with Codex's own description of their usage. Use a serviceTiers id, or \"standard\", as an agent or message speed. A tier marked limited is not confirmed by every account able to run the model; runs that ask for it prefer an account that confirms it. Before each turn attempt the run checks the tier against its Codex process model list and records a status event when the outcome changes: speed.applied is standard when the turn runs at Standard, speed.confirmed when the Codex process and the account's recent catalog both list the tier and the turn asks for it, speed.unconfirmed when it cannot be confirmed for the account and model (a model list could not be read or does not name the model exactly, or the account's catalog is unknown or older than 30 minutes)."
            }
            "get_coding_agent_account_sign_in" => {
                "Read account identity, provider, state and phase of a sign-in already started by the human owner. Never returns its challenge URL, code, credentials or errors containing provider details."
            }
            _ => {
                "Read model catalogs (including reasoning levels, routes and Codex service tiers), connection progress, account metadata and usage, or manage account settings/order with account policies. Refreshing credentials/usage, connecting, replacing credentials, submitting codes, cancelling sign-in and deleting credentials require an authenticated human owner; an installation grant is not human proof. Agents never receive sign-in challenges or credentials."
            }
        };
        json!({
            "name": self.name,
            "description": description,
            "inputSchema": input,
            "outputSchema": output,
            "_meta": {
                "securitySchemes": [{ "type": "oauth2", "scopes": [self.scope] }]
            },
            "annotations": {
                "readOnlyHint": self.scope == "read",
                "destructiveHint": self.scope != "read",
                "openWorldHint": self.scope != "read"
            }
        })
    }

    pub(crate) async fn invoke(
        &self,
        s: &Arc<Service>,
        input: Value,
        context: ResumeContext,
    ) -> Result<Value> {
        let (human, bearer, public_credential) = match context {
            ResumeContext::Human(_) => (true, None, None),
            ResumeContext::Public(scopes, credential) => {
                if !scopes.iter().any(|scope| scope == self.scope) {
                    return Err(Error::forbidden(format!(
                        "The {} scope is required.",
                        self.scope
                    )));
                }
                if matches!(
                    self.name,
                    "replace_coding_agent_account" | "order_coding_agent_accounts"
                ) && credential.is_none()
                {
                    return Err(Error::unauthorized(
                        "A current public MCP authorization is required.",
                    ));
                }
                (false, None, credential)
            }
            ResumeContext::Installation(bearer) => {
                crate::installation_mcp::authorize(s, &bearer).await?;
                (false, Some(bearer), None)
            }
        };
        let authorize =
            crate::installation_mcp::management_authorization(s, bearer.as_deref()).await?;
        let before = authorize.clone();
        s.store.read(move |db| before.authorize(db)).await?;
        // A manual poll may refresh credentials and redeem a banked reset.
        // Preserve the human owner policy before entering any Driver or lock wait.
        if !human && self.name == "refresh_coding_agent_accounts" {
            let _: ListAccountsInput = decode(input)?;
            return Ok(serde_json::to_value(RefreshAccountsOutput::HumanRequired {
                message: "An authenticated installation owner must refresh accounts in Connections. Agents can read the automatically refreshed usage.".into(),
                path: "/connections".into(),
            })?);
        }
        s.accounts.initialize(s).await?;
        authorize.recheck().await?;
        let result = match self.name {
            "list_models" => {
                let input: ListModelsInput = decode(input)?;
                let provider: crate::provider::Provider = input.provider.into();
                let catalog: ModelCatalog = serde_json::from_value(
                    provider.adapter().model_catalog(s).await?,
                )
                .map_err(|_| {
                    Error::bad_gateway("The coding agent returned an invalid model catalog.")
                })?;
                serde_json::to_value(catalog)?
            }
            "get_coding_agent_account_sign_in" => {
                let _: ListAccountsInput = decode(input)?;
                let sign_in = s
                    .accounts
                    .sign_in()
                    .await
                    .map(|sign_in| AccountSignInProgress {
                        account_id: sign_in.account_id,
                        provider: sign_in.provider,
                        state: sign_in.state,
                        phase: sign_in.phase,
                    });
                serde_json::to_value(AccountSignInProgressOutput { sign_in })?
            }
            "list_coding_agent_account_providers" => {
                let input: AccountProviderInput = decode(input)?;
                let provider: crate::provider::Provider = input.provider.into();
                let output = if provider.driver().accepts_api_keys() {
                    AccountProvidersOutput::Available {
                        providers: decode(provider.driver().api_key_providers(s).await?)?,
                    }
                } else {
                    AccountProvidersOutput::Unsupported {
                        provider: input.provider,
                        capability: "apiKey".into(),
                        message: "This coding agent does not accept API keys.".into(),
                    }
                };
                serde_json::to_value(output)?
            }
            "list_coding_agent_accounts" | "refresh_coding_agent_accounts" => {
                let _: ListAccountsInput = decode(input)?;
                if self.name == "refresh_coding_agent_accounts" {
                    s.accounts.poll(s, false).await?;
                }
                let mut result = s.accounts.overview(s).await?;
                if !human {
                    result.sign_in = None;
                }
                if self.name == "refresh_coding_agent_accounts" {
                    serde_json::to_value(RefreshAccountsOutput::Refreshed { overview: result })?
                } else {
                    serde_json::to_value(result)?
                }
            }
            "replace_coding_agent_account" => {
                let input: ReplaceAccountInput = decode(input)?;
                uuid(&input.account_id)?;
                serde_json::to_value(
                    s.accounts
                        .update_live(
                            s,
                            &input.account_id,
                            &input.revision,
                            &input.settings,
                            public_credential.as_deref(),
                            authorize.clone(),
                        )
                        .await?,
                )?
            }
            "order_coding_agent_accounts" => {
                let input: OrderAccountsInput = decode(input)?;
                for id in &input.ids {
                    uuid(id)?;
                }
                let provider: crate::provider::Provider = input.provider.into();
                if !provider.driver().ordered() {
                    return Ok(serde_json::to_value(OrderAccountsOutput::Unsupported {
                        provider: input.provider,
                        capability: "order".into(),
                        message: "This coding agent does not order its accounts.".into(),
                    })?);
                }
                let mut result = s
                    .accounts
                    .reorder_live(
                        s,
                        input.provider.into(),
                        &input.ids,
                        public_credential.as_deref(),
                        authorize.clone(),
                    )
                    .await?;
                if !human {
                    result.sign_in = None;
                }
                serde_json::to_value(OrderAccountsOutput::Ordered { overview: result })?
            }
            "connect_coding_agent_account"
            | "reconnect_coding_agent_account"
            | "replace_coding_agent_account_key" => {
                // No CLI, vault, account creation or challenge read before the human proof.
                if !human {
                    match self.name {
                        "connect_coding_agent_account" => {
                            let _: ConnectAccountInput = decode(input)?;
                        }
                        "reconnect_coding_agent_account" => {
                            let input: AccountInput = decode(input)?;
                            uuid(&input.account_id)?;
                        }
                        _ => {
                            let input: ReplaceAccountKeyInput = decode(input)?;
                            uuid(&input.account_id)?;
                        }
                    }
                    serde_json::to_value(ConnectAccountOutput::HumanRequired { message:"An authenticated installation owner must connect this account in Connections.".into(),path:"/connections".into() })?
                } else {
                    let output = match self.name {
                        "connect_coding_agent_account" => {
                            let input: ConnectAccountInput = decode(input)?;
                            let provider: crate::provider::Provider = input.provider.into();
                            if !provider.driver().accepts_api_keys()
                                && (input.api_key.is_some() || input.model_provider.is_some())
                            {
                                ConnectAccountOutput::Unsupported {
                                    provider: input.provider,
                                    capability: "apiKey".into(),
                                    message: "This coding agent does not accept API keys.".into(),
                                }
                            } else if provider.driver().accepts_api_keys() {
                                ConnectAccountOutput::Connected {
                                    account: Box::new(
                                        s.accounts
                                            .connect_api_key(
                                                s,
                                                provider,
                                                &input.name,
                                                input.model_provider.as_deref().unwrap_or(""),
                                                input.api_key.as_deref().unwrap_or(""),
                                                None,
                                            )
                                            .await?,
                                    ),
                                }
                            } else {
                                ConnectAccountOutput::SignIn {
                                    sign_in: s.accounts.add(s, provider, &input.name).await?,
                                }
                            }
                        }
                        "reconnect_coding_agent_account" => {
                            let input: AccountInput = decode(input)?;
                            uuid(&input.account_id)?;
                            let account = s.accounts.account(s, &input.account_id).await?;
                            if account.provider.driver().accepts_api_keys() {
                                ConnectAccountOutput::Unsupported {
                                    provider: account.provider.into(), capability: "signIn".into(),
                                    message: "Reconnect this coding agent by replacing its provider API key.".into(),
                                }
                            } else {
                                ConnectAccountOutput::SignIn {
                                    sign_in: s.accounts.reconnect(s, &input.account_id).await?,
                                }
                            }
                        }
                        _ => {
                            let input: ReplaceAccountKeyInput = decode(input)?;
                            uuid(&input.account_id)?;
                            let account = s.accounts.account(s, &input.account_id).await?;
                            if !account.provider.driver().accepts_api_keys() {
                                ConnectAccountOutput::Unsupported {
                                    provider: account.provider.into(),
                                    capability: "apiKey".into(),
                                    message: "This coding agent does not accept API keys.".into(),
                                }
                            } else {
                                ConnectAccountOutput::Connected {
                                    account: Box::new(
                                        s.accounts
                                            .connect_api_key(
                                                s,
                                                account.provider,
                                                &account.name,
                                                &input.model_provider,
                                                &input.api_key,
                                                Some(&input.account_id),
                                            )
                                            .await?,
                                    ),
                                }
                            }
                        }
                    };
                    serde_json::to_value(output)?
                }
            }
            _ => {
                if !human {
                    match self.name {
                        "submit_coding_agent_account_code" => {
                            let _: AccountCodeInput = decode(input)?;
                        }
                        "cancel_coding_agent_account_sign_in" => {
                            let _: ListAccountsInput = decode(input)?;
                        }
                        _ => {
                            let input: AccountInput = decode(input)?;
                            uuid(&input.account_id)?;
                        }
                    }
                    return Ok(serde_json::to_value(AccountMutationOutput::HumanRequired {
                        message: "An authenticated installation owner is required to manage account credentials or sign-in.".into(),
                        path: "/connections".into(),
                    })?);
                }
                match self.name {
                    "submit_coding_agent_account_code" => {
                        let input: AccountCodeInput = decode(input)?;
                        let code = input.code.trim();
                        if code.is_empty()
                            || code.len() > 4096
                            || code.chars().any(char::is_whitespace)
                        {
                            return Err(Error::bad(
                                "Paste the authorization code shown by the sign-in page.",
                            ));
                        }
                        s.accounts.submit_code(code).await?;
                    }
                    "cancel_coding_agent_account_sign_in" => {
                        let _: ListAccountsInput = decode(input)?;
                        s.accounts.cancel(s).await?;
                    }
                    _ => {
                        let input: AccountInput = decode(input)?;
                        uuid(&input.account_id)?;
                        s.accounts.remove(s, &input.account_id).await?;
                    }
                }
                serde_json::to_value(AccountMutationOutput::Completed { done: true })?
            }
        };
        authorize.recheck().await?;
        s.store
            .read(move |db| {
                authorize.authorize(db)?;
                Ok(result)
            })
            .await
    }
}

pub(crate) async fn routes(s: &Arc<Service>, input: &Input) -> Result<Value> {
    let parts = input
        .path
        .trim_start_matches("/api/accounts")
        .trim_start_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>();
    if let ("GET", ["providers", provider]) = (input.method.as_str(), parts.as_slice()) {
        let provider = crate::provider::Provider::parse(provider)?;
        return account_operation("list_coding_agent_account_providers")
            .unwrap()
            .invoke(
                s,
                json!({ "provider": provider }),
                ResumeContext::http(input)?,
            )
            .await;
    }
    let mut body = input.body.clone();
    let name = match (input.method.as_str(), parts.as_slice()) {
        ("GET", []) => "list_coding_agent_accounts",
        ("GET", ["sign-in"]) => "get_coding_agent_account_sign_in",
        ("POST", []) => "connect_coding_agent_account",
        ("POST", ["order"]) => "order_coding_agent_accounts",
        ("POST", ["refresh"]) => "refresh_coding_agent_accounts",
        ("POST", ["sign-in", "code"]) => "submit_coding_agent_account_code",
        ("DELETE", ["sign-in"]) => "cancel_coding_agent_account_sign_in",
        (method, [id, ..]) => {
            uuid(id)?;
            if body.is_null() {
                body = json!({});
            }
            if body.get("accountId").is_some_and(|value| value != id) {
                return Err(Error::bad("The path and account identifier disagree."));
            }
            body.as_object_mut()
                .ok_or_else(|| Error::bad("Expected an account command object."))?
                .insert("accountId".into(), (*id).into());
            match (method, parts.as_slice()) {
                ("PUT", [_]) => "replace_coding_agent_account",
                ("DELETE", [_]) => "delete_coding_agent_account",
                ("POST", [_, "sign-in"]) => "reconnect_coding_agent_account",
                ("POST", [_, "api-key"]) => "replace_coding_agent_account_key",
                _ => return Err(Error::not_found("Unknown account operation.")),
            }
        }
        _ => return Err(Error::not_found("Unknown account operation.")),
    };
    if body.is_null() {
        body = json!({});
    }
    account_operation(name)
        .expect("known account operation")
        .invoke(s, body, ResumeContext::http(input)?)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        accounts::{Account, AccountState},
        provider::Provider,
    };
    use std::sync::atomic::{AtomicBool, Ordering};

    #[tokio::test]
    async fn revoked_authorization_after_account_lock_wait_cannot_publish_settings() {
        let root = tempfile::tempdir().unwrap();
        let s = Service::new(crate::config::Config {
            data_dir: root.path().join("data"),
            home: root.path().join("home"),
            workspace_roots: vec![root.path().to_owned()],
            public_url: "http://localhost:4310".into(),
            host: "127.0.0.1".into(),
            port: 0,
            codex_bin: "/nonexistent-codex".into(),
            claude_bin: "/nonexistent-claude".into(),
            opencode_bin: "/nonexistent-opencode".into(),
            opencode_go_usage_url: crate::accounts::opencode::USAGE_URL.into(),
            concurrency: 1,
            logger: false,
            worker_enabled: false,
            runner_url: String::new(),
        })
        .await
        .unwrap();
        let account = Account::new(Provider::Claude, "Unchanged", AccountState::Pending);
        s.store.save_coding_account(&account).await.unwrap();
        let guard = s.accounts.lock(&account.id).await;
        let revoked = Arc::new(AtomicBool::new(false));
        let denied = revoked.clone();
        let settings = AccountSettings {
            name: "Revoked edit".into(),
            enabled: false,
            max_concurrent_runs: 1,
        };
        let mut waiting = Box::pin(s.accounts.update_authorized(
            &s,
            &account.id,
            &account.revision,
            &settings,
            None,
            move |_| {
                if denied.load(Ordering::SeqCst) {
                    Err(Error::forbidden("Revoked"))
                } else {
                    Ok(())
                }
            },
        ));
        assert!(futures_util::poll!(&mut waiting).is_pending());
        revoked.store(true, Ordering::SeqCst);
        drop(guard);
        assert_eq!(waiting.await.unwrap_err().status, 403);
        assert_eq!(s.accounts.account(&s, &account.id).await.unwrap(), account);
    }
}
