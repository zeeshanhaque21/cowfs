use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// Stable error codes of the control protocol. Unknown codes from a newer peer decode as `Other`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ErrorCode {
    MalformedFrame,
    LineTooLong,
    HandshakeRequired,
    UnsupportedVersion,
    PermissionDenied,
    UnknownFrame,
    UnknownMethod,
    InvalidParams,
    DuplicateId,
    Busy,
    NotFound,
    AlreadyExists,
    Unsupported,
    Cancelled,
    ShuttingDown,
    IoError,
    Internal,
    Other(String),
}

const CODES: &[(ErrorCode, &str)] = &[
    (ErrorCode::MalformedFrame, "malformed_frame"),
    (ErrorCode::LineTooLong, "line_too_long"),
    (ErrorCode::HandshakeRequired, "handshake_required"),
    (ErrorCode::UnsupportedVersion, "unsupported_version"),
    (ErrorCode::PermissionDenied, "permission_denied"),
    (ErrorCode::UnknownFrame, "unknown_frame"),
    (ErrorCode::UnknownMethod, "unknown_method"),
    (ErrorCode::InvalidParams, "invalid_params"),
    (ErrorCode::DuplicateId, "duplicate_id"),
    (ErrorCode::Busy, "busy"),
    (ErrorCode::NotFound, "not_found"),
    (ErrorCode::AlreadyExists, "already_exists"),
    (ErrorCode::Unsupported, "unsupported"),
    (ErrorCode::Cancelled, "cancelled"),
    (ErrorCode::ShuttingDown, "shutting_down"),
    (ErrorCode::IoError, "io_error"),
    (ErrorCode::Internal, "internal"),
];

impl ErrorCode {
    /// The wire string of this code.
    pub fn as_str(&self) -> &str {
        match self {
            ErrorCode::Other(s) => s,
            known => CODES
                .iter()
                .find(|(c, _)| c == known)
                .map_or("internal", |(_, s)| s),
        }
    }

    /// Parses a wire string. Unknown strings become `Other`.
    pub fn parse(s: &str) -> ErrorCode {
        CODES
            .iter()
            .find(|(_, name)| *name == s)
            .map_or_else(|| ErrorCode::Other(s.to_owned()), |(c, _)| c.clone())
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for ErrorCode {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ErrorCode {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(ErrorCode::parse(&String::deserialize(d)?))
    }
}

/// A structured error: what the server sends in an `error` frame and what handlers return.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{code}: {message}")]
pub struct CtlError {
    pub code: ErrorCode,
    #[serde(default)]
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl CtlError {
    /// Builds an error with no details.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        CtlError {
            code,
            message: message.into(),
            details: None,
        }
    }

    /// Adds structured details.
    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }

    /// The error a handler returns when its request was cancelled.
    pub fn cancelled() -> Self {
        Self::new(ErrorCode::Cancelled, "operation cancelled")
    }

    /// Shorthand for `invalid_params`.
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidParams, message)
    }

    /// Shorthand for `not_found`.
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::NotFound, message)
    }
}

/// Result alias for handlers.
pub type CtlResult<T> = Result<T, CtlError>;
