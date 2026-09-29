use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;

#[derive(Clone, Debug)]
pub struct Error {
    pub status: u16,
    pub message: String,
    pub oauth: Option<String>,
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn new(status: u16, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            oauth: None,
        }
    }

    pub fn bad(message: impl Into<String>) -> Self {
        Self::new(400, message)
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(401, message)
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(403, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(404, message)
    }

    pub fn method_not_allowed(message: impl Into<String>) -> Self {
        Self::new(405, message)
    }

    pub fn timeout(message: impl Into<String>) -> Self {
        Self::new(408, message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(409, message)
    }

    pub fn too_large(message: impl Into<String>) -> Self {
        Self::new(413, message)
    }

    pub fn too_many_requests(message: impl Into<String>) -> Self {
        Self::new(429, message)
    }

    pub fn bad_gateway(message: impl Into<String>) -> Self {
        Self::new(502, message)
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(503, message)
    }

    pub fn gateway_timeout(message: impl Into<String>) -> Self {
        Self::new(504, message)
    }

    pub const fn is_unauthorized(&self) -> bool {
        self.status == 401
    }

    pub const fn is_not_found(&self) -> bool {
        self.status == 404
    }

    pub const fn is_conflict(&self) -> bool {
        self.status == 409
    }

    pub const fn is_unavailable(&self) -> bool {
        self.status == 503
    }

    pub fn oauth(code: &str, message: &str) -> Self {
        Self {
            status: 400,
            message: message.into(),
            oauth: Some(code.into()),
        }
    }

    pub fn internal(error: impl std::fmt::Display) -> Self {
        tracing::error!(error = %error, "backend operation failed");
        Self::new(500, "An internal error occurred. Check the server logs.")
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for Error {}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let body = match self.oauth {
            Some(code) => json!({ "error": code, "error_description": self.message }),
            None => json!({ "error": self.message }),
        };
        (
            StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(body),
        )
            .into_response()
    }
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        if let rusqlite::Error::SqliteFailure(code, _) = &e
            && code.code == rusqlite::ErrorCode::ConstraintViolation
        {
            return Self::conflict(
                "This operation conflicts with an existing record or active run.",
            );
        }
        Self::internal(e)
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::internal(e)
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Self::internal(e)
    }
}

pub fn required<T>(value: Option<T>, message: &str) -> Result<T> {
    value.ok_or_else(|| Error::not_found(message))
}
