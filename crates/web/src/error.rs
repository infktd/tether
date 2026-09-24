use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

/// A failed request: the status and a message safe to show the user. Details
/// go to the log, never to the response.
#[derive(Debug)]
pub struct AppError {
    status: StatusCode,
    message: &'static str,
}

impl AppError {
    pub fn new(status: StatusCode, message: &'static str) -> Self {
        Self { status, message }
    }

    pub fn unauthorized() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "Log in to continue.")
    }

    /// Logs `err` and returns a generic 500.
    pub fn internal(err: impl std::fmt::Display) -> Self {
        tracing::error!(error = %err, "request failed");
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong.")
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
