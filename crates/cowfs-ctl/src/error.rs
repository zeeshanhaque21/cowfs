use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

macro_rules! codes {
    ($($v:ident => $s:literal),* $(,)?) => {
        /// Stable error codes of the control protocol. Unknown codes from a newer peer decode as `Other`.
        #[derive(Clone, Debug, PartialEq, Eq)]
        #[allow(missing_docs)]
        pub enum ErrorCode {
            $($v,)*
            Other(String),
        }

        impl ErrorCode {
            /// Every code this version defines.
            pub const ALL: &'static [ErrorCode] = &[$(ErrorCode::$v),*];

            /// The wire string of this code.
            pub fn as_str(&self) -> &str {
                match self {
                    $(ErrorCode::$v => $s,)*
                    ErrorCode::Other(s) => s,
                }
            }

            /// Parses a wire string. Unknown strings become `Other`.
            pub fn parse(s: &str) -> ErrorCode {
                match s {
                    $($s => ErrorCode::$v,)*
                    other => ErrorCode::Other(other.to_owned()),
                }
            }
        }
    };
}

codes! {
    MalformedFrame => "malformed_frame",
    LineTooLong => "line_too_long",
    HandshakeRequired => "handshake_required",
    UnsupportedVersion => "unsupported_version",
    PermissionDenied => "permission_denied",
    UnknownFrame => "unknown_frame",
    UnknownMethod => "unknown_method",
    InvalidParams => "invalid_params",
    DuplicateId => "duplicate_id",
    Busy => "busy",
    NotFound => "not_found",
    AlreadyExists => "already_exists",
    Unsupported => "unsupported",
    Cancelled => "cancelled",
    ShuttingDown => "shutting_down",
    IoError => "io_error",
    Internal => "internal",
    TooManyConnections => "too_many_connections",
    Timeout => "timeout",
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
