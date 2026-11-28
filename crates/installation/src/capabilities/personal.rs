//! Read marks and snoozed conversations of one Cairn account, on HTTP and both tools.
//! An execution never acts as a person: without a delegation from an account, it is refused.
use super::{ResumeContext, decode};
use crate::{
    error::{Error, Result},
    service::Service,
};
use cairn_contracts::*;
use serde_json::{Value, json};

pub(crate) struct PersonalOperation {
    pub name: &'static str,
    description: &'static str,
}

pub(crate) const PERSONAL_OPERATIONS: &[PersonalOperation] = &[
    PersonalOperation {
        name: "mark_read",
        description: "Acknowledge conversation questions, finished turns (including errors), or blocked outcomes for the Cairn account that authorized this connection. Pass the conversation readAt or finishedAt; later work stays unread. Reading does not answer a question or change execution status. Executions have no account delegation and are refused.",
    },
    PersonalOperation {
        name: "snooze_chat",
        description: "Snooze any conversation until a later time (up to 30 days) or restore it early, for the Cairn account that authorized this connection. For a new snooze, pass the later of lastActivityAt and readAt (or updatedAt if readAt is absent) as finishedAt. Newer activity cancels the snooze. Executions have no account delegation and are refused.",
    },
];

pub(crate) fn personal_operation(name: &str) -> Option<&'static PersonalOperation> {
    PERSONAL_OPERATIONS.iter().find(|op| op.name == name)
}

/// The verified account whose personal state a request changes. A public grant
/// is bound to the account that authorized it; an execution is bound to none.
pub(crate) async fn reader(s: &Service, context: ResumeContext, scope: &str) -> Result<String> {
    match context {
        ResumeContext::Human(account) if !account.is_empty() => Ok(account),
        ResumeContext::Human(_) => Err(Error::unauthorized("Access through the Beacon.")),
        ResumeContext::Public(scopes, _) => {
            if !scopes.iter().any(|granted| granted == scope) {
                return Err(Error::forbidden(format!("The {scope} scope is required.")));
            }
            crate::auth::public_account()
                .or_else(crate::native::policy::caller)
                .ok_or_else(|| Error::forbidden("This connection is not bound to a Cairn account."))
        }
        ResumeContext::Installation(bearer) => {
            crate::installation_mcp::authorize(s, &bearer).await?;
            Err(Error::forbidden(
                "Read marks and snoozed conversations belong to a Cairn account; this execution has no delegation from one. Ask the person to do it in the Cairn apps or through their own MCP connection.",
            ))
        }
    }
}

/// The account whose personal state a listing shows, if any. Executions see none.
pub(crate) fn listing_reader(context: &ResumeContext) -> Option<String> {
    match context {
        ResumeContext::Human(account) => Some(account.clone()).filter(|a| !a.is_empty()),
        ResumeContext::Public(..) => {
            crate::auth::public_account().or_else(crate::native::policy::caller)
        }
        ResumeContext::Installation(_) => None,
    }
}

impl PersonalOperation {
    pub(crate) fn descriptor(&self) -> Value {
        let (input, output) = match self.name {
            "mark_read" => (
                schema_for::<MarkReadInput>(Contract::Deserialize),
                schema_for::<MarkReadOutput>(Contract::Serialize),
            ),
            _ => (
                schema_for::<SetConversationSnoozeInput>(Contract::Deserialize),
                schema_for::<SetConversationSnoozeOutput>(Contract::Serialize),
            ),
        };
        json!({
            "name": self.name,
            "description": self.description,
            "inputSchema": input,
            "outputSchema": output,
            "_meta": { "securitySchemes": [{ "type": "oauth2", "scopes": ["manage"] }] },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false },
        })
    }

    pub(crate) async fn invoke(
        &self,
        s: &Service,
        input: Value,
        context: ResumeContext,
    ) -> Result<Value> {
        let account = reader(s, context, "manage").await?;
        match self.name {
            "mark_read" => Ok(serde_json::to_value(
                crate::reads::mark(s, &account, decode(input)?).await?,
            )?),
            _ => Ok(serde_json::to_value(
                crate::reads::snooze(s, &account, decode(input)?).await?,
            )?),
        }
    }
}
