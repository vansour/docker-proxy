use thiserror::Error;

/// Custom error types for the Docker proxy
#[derive(Error, Debug)]
pub enum ProxyError {
    #[error("Network request failed: {0}")]
    Network(reqwest::Error),

    #[error("Manifest not found: {status}")]
    ManifestNotFound { status: reqwest::StatusCode },

    #[error("Blob not found: {status}")]
    BlobNotFound { status: reqwest::StatusCode },

    #[error("Failed to read response body: {0}")]
    ResponseReadError(String),

    #[error("Blob upload not supported")]
    BlobUploadNotSupported,

    #[allow(dead_code)]
    #[error("Invalid registry URL: {0}")]
    InvalidRegistryUrl(String),

    #[allow(dead_code)]
    #[error("Authentication failed: {0}")]
    AuthenticationFailed(String),

    #[allow(dead_code)]
    #[error("Internal error: {0}")]
    InternalError(String),
}

/// Type alias for Result with ProxyError
pub type ProxyResult<T> = Result<T, ProxyError>;

impl From<reqwest::Error> for ProxyError {
    fn from(err: reqwest::Error) -> Self {
        ProxyError::Network(err)
    }
}
