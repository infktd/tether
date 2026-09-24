use std::borrow::Cow;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

/// A failed request: the status and a message safe to show the user. Details
/// go to the log, never to the response.
#[derive(Debug)]
pub struct AppError {
    status: StatusCode,
    message: Cow<'static, str>,
}

impl AppError {
    pub fn new(status: StatusCode, message: impl Into<Cow<'static, str>>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    pub fn unauthorized() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "Log in to continue.")
    }

    pub fn forbidden() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            "You don't have permission to do that.",
        )
    }

    pub fn not_found(what: &'static str) -> Self {
        Self::new(StatusCode::NOT_FOUND, what)
    }

    pub fn bad_request(message: impl Into<Cow<'static, str>>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    /// Logs `err` and returns a generic 500.
    pub fn internal(err: impl std::fmt::Display) -> Self {
        tracing::error!(error = %err, "request failed");
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong.")
    }

    pub fn status(&self) -> StatusCode {
        self.status
    }
}

impl From<sqlx::Error> for AppError {
    fn from(err: sqlx::Error) -> Self {
        Self::internal(err)
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (self.status, self.message).into_response()
    }
}

/// True for a unique-constraint violation (SQLSTATE 23505).
pub fn is_unique_violation(err: &sqlx::Error) -> bool {
    sqlstate(err).is_some_and(|code| code == "23505")
}

/// True for a foreign-key violation (SQLSTATE 23503).
pub fn is_foreign_key_violation(err: &sqlx::Error) -> bool {
    sqlstate(err).is_some_and(|code| code == "23503")
}

fn sqlstate(err: &sqlx::Error) -> Option<std::borrow::Cow<'_, str>> {
    err.as_database_error().and_then(|e| e.code())
}
