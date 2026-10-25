//! Converted account capabilities execute only at Beacon, against its Postgres authority.
use super::*;
use cairn_contracts::beacon::{
    AccountSignInMethod, ListAccountMethodsInput, ListAccountMethodsOutput,
};

pub(super) struct ListAccountMethods;

pub(super) const LIST_ACCOUNT_METHODS: ListAccountMethods = ListAccountMethods;

impl ListAccountMethods {
    pub(super) async fn invoke(
        &self,
        service: &Service,
        headers: &HeaderMap,
        _: ListAccountMethodsInput,
    ) -> Result<ListAccountMethodsOutput, ApiError> {
        let mut transaction = service.pool.begin().await?;
        // Authentication and data use one snapshot. No identity comes from input JSON,
        // an installation grant or the installation's direct/relay connection.
        query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *transaction)
            .await?;
        let (account, _) = methods::authenticated_on(&mut transaction, headers, false).await?;
        let rows: Vec<(String, String, String)> = query_as(
            "SELECT id, kind, label FROM sign_in_methods WHERE account_id = $1 AND NOT removed ORDER BY kind, id",
        )
        .bind(account)
        .fetch_all(&mut *transaction)
        .await?;
        let methods = rows
            .into_iter()
            .map(|(id, kind, label)| AccountSignInMethod { id, kind, label })
            .collect();
        transaction.commit().await?;

        Ok(ListAccountMethodsOutput::Listed { methods })
    }
}
