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
            "list_coding_agent_account_providers" => {
                schema_for::<Vec<ApiKeyProvider>>(Contract::Serialize)
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
        json!({
            "name": self.name,
            "description": "Manage coding-agent account metadata and usage with account policies. Connecting, replacing credentials, submitting codes, cancelling sign-in and deleting credentials require an authenticated human owner; an installation grant is not human proof. Agents never receive sign-in challenges or credentials.",
            "inputSchema": input,
            "outputSchema": output,
            "_meta": { "securitySchemes": [{ "type":"oauth2", "scopes":[self.scope] }] },
            "annotations": { "readOnlyHint": self.scope=="read", "destructiveHint": self.scope!="read", "openWorldHint": self.scope!="read" }
        })
    }

    pub(crate) async fn invoke(
        &self,
        s: &Arc<Service>,
        input: Value,
        context: ResumeContext,
    ) -> Result<Value> {
        let (human, bearer) = match context {
            ResumeContext::Human(_) => (true, None),
            ResumeContext::Public(scopes, _) => {
                if !scopes.iter().any(|scope| scope == self.scope) {
                    return Err(Error::forbidden(format!(
                        "The {} scope is required.",
                        self.scope
                    )));
                }
                (false, None)
            }
            ResumeContext::Installation(bearer) => {
                crate::installation_mcp::authorize(s, &bearer).await?;
                (false, Some(bearer))
            }
        };
        let authorize = crate::installation_mcp::management_authorization(bearer.as_deref());
        let before = authorize.clone();
        s.store.read(move |db| before(db)).await?;
        s.accounts.initialize(s).await?;
        let result = match self.name {
            "list_coding_agent_account_providers" => {
                let input: AccountProviderInput = decode(input)?;
                let provider: crate::provider::Provider = input.provider.into();
                let values: Vec<ApiKeyProvider> =
                    decode(provider.driver().api_key_providers(s).await?)?;
                serde_json::to_value(values)?
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
                serde_json::to_value(result)?
            }
            "replace_coding_agent_account" => {
                let input: ReplaceAccountInput = decode(input)?;
                uuid(&input.account_id)?;
                serde_json::to_value(
                    s.accounts
                        .update_authorized(
                            s,
                            &input.account_id,
                            &input.revision,
                            &input.settings,
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
                let mut result = s
                    .accounts
                    .reorder_authorized(s, input.provider.into(), &input.ids, authorize.clone())
                    .await?;
                if !human {
                    result.sign_in = None;
                }
                serde_json::to_value(result)?
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
                            if provider.driver().accepts_api_keys() {
                                ConnectAccountOutput::Connected {
                                    account: s
                                        .accounts
                                        .connect_api_key(
                                            s,
                                            provider,
                                            &input.name,
                                            input.model_provider.as_deref().unwrap_or(""),
                                            input.api_key.as_deref().unwrap_or(""),
                                            None,
                                        )
                                        .await?,
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
                            ConnectAccountOutput::SignIn {
                                sign_in: s.accounts.reconnect(s, &input.account_id).await?,
                            }
                        }
                        _ => {
                            let input: ReplaceAccountKeyInput = decode(input)?;
                            uuid(&input.account_id)?;
                            let account = s.accounts.account(s, &input.account_id).await?;
                            ConnectAccountOutput::Connected {
                                account: s
                                    .accounts
                                    .connect_api_key(
                                        s,
                                        account.provider,
                                        &account.name,
                                        &input.model_provider,
                                        &input.api_key,
                                        Some(&input.account_id),
                                    )
                                    .await?,
                            }
                        }
                    };
                    serde_json::to_value(output)?
                }
            }
            _ => {
                if !human {
                    return Err(Error::forbidden(
                        "An authenticated installation owner is required to manage account credentials or sign-in.",
                    ));
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
                serde_json::to_value(AccountMutationOutput { done: true })?
            }
        };
        s.store
            .read(move |db| {
                authorize(db)?;
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
            .invoke(s, json!({"provider":provider}), ResumeContext::http(input)?)
            .await;
    }
    let mut body = input.body.clone();
    let name = match (input.method.as_str(), parts.as_slice()) {
        ("GET", []) => "list_coding_agent_accounts",
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
            body["accountId"] = (*id).into();
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
