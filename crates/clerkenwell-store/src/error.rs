use serde_json::Value;

/// Why a store operation failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreErrorKind {
    NotFound,
    InvalidRequest,
    Conflict,
    Internal,
}

/// A store failure. `data` carries structured detail a caller can branch on,
/// such as the accepted state a conflicting write lost to.
#[derive(Debug, Clone)]
pub struct StoreError {
    pub kind: StoreErrorKind,
    pub message: String,
    pub data: Option<Value>,
}

impl StoreError {
    fn new(kind: StoreErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            data: None,
        }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StoreErrorKind::NotFound, message)
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(StoreErrorKind::InvalidRequest, message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(StoreErrorKind::Conflict, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(StoreErrorKind::Internal, message)
    }

    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{:?}: {}", self.kind, self.message)
    }
}

impl std::error::Error for StoreError {}

pub type StoreResult<T> = Result<T, StoreError>;
