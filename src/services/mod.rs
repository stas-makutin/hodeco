pub mod catalog;
pub mod discovery;
pub mod registry;
pub mod serial;
#[cfg(test)]
pub(crate) mod test_support;
pub mod zwave;

use hodeco_contract::domain::ParamError;

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("service not found")]
    NotFound,
    #[error("service already exists")]
    AlreadyExists,
    #[error("service alias already exists")]
    AliasConflict,
    #[error("the service key is not valid")]
    InvalidKey,
    #[error("the service alias is not valid")]
    InvalidAlias,
    #[error("service is not ready")]
    NotReady,
    #[error("service send failed")]
    SendFailed,
    #[error("the payload is not valid")]
    InvalidPayload,
    #[error("unknown protocol '{0}'")]
    UnknownProtocol(String),
    #[error("unknown transport '{0}'")]
    UnknownTransport(String),
    #[error("transport '{transport}' is not supported for protocol '{protocol}'")]
    UnsupportedTransport { protocol: String, transport: String },
    #[error("transport error: {0}")]
    Transport(String),
    #[error(transparent)]
    InvalidParams(#[from] ParamError),
    #[error("discovery not found")]
    DiscoveryNotFound,
    #[error("discovery service is busy")]
    DiscoveryBusy,
}
