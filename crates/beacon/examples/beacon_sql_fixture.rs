//! Test-only SQL transport for browser fixtures; never included in production images.
use serde::Deserialize;
use sqlx_core::{connection::Connection, raw_sql::raw_sql};
use sqlx_postgres::{PgConnectOptions, PgConnection};
use std::{
    io::{self, Read},
    process::ExitCode,
    str::FromStr,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    database_url: String,
    sql: String,
}

enum Failure {
    Input,
    Connection,
    Query,
}

impl Failure {
    fn report(self) -> ExitCode {
        // Never print SQLx/serde errors: they may contain the URL, password or SQL.
        let (code, message) = match self {
            Self::Input => (1, "BEACON_SQL_FIXTURE_INVALID_INPUT"),
            Self::Connection => (2, "BEACON_SQL_FIXTURE_CONNECTION_FAILED"),
            Self::Query => (3, "BEACON_SQL_FIXTURE_QUERY_FAILED"),
        };
        eprintln!("{message}");
        ExitCode::from(code)
    }
}

fn read_request() -> Result<Request, Failure> {
    let mut input = String::new();
    io::stdin()
        .read_to_string(&mut input)
        .map_err(|_| Failure::Input)?;
    serde_json::from_str(&input).map_err(|_| Failure::Input)
}

async fn execute(request: Request) -> Result<(), Failure> {
    let database = url::Url::parse(&request.database_url).map_err(|_| Failure::Input)?;
    if !matches!(database.scheme(), "postgres" | "postgresql") {
        return Err(Failure::Input);
    }

    let options = PgConnectOptions::from_str(database.as_str()).map_err(|_| Failure::Input)?;
    let mut connection = PgConnection::connect_with(&options)
        .await
        .map_err(|_| Failure::Connection)?;
    // Like psql -c, send the complete batch in the simple-query protocol. A
    // prepared query cannot preserve multi-statement and implicit rollback behavior.
    let result = raw_sql(&request.sql)
        .execute(&mut connection)
        .await
        .map_err(|_| Failure::Query);
    let closed = connection.close().await;
    result?;
    closed.map_err(|_| Failure::Connection)
}

#[tokio::main]
async fn main() -> ExitCode {
    let result = match read_request() {
        Ok(request) => execute(request).await,
        Err(error) => Err(error),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => error.report(),
    }
}
