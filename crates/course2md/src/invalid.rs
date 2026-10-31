//! Typed markers for input the program refused before starting any work.

/// The command line or task input was rejected before any work started. It
/// shows the wrapped error unchanged; the type only names the cause.
#[derive(Debug, thiserror::Error)]
#[error("{0:#}")]
pub struct InvalidInput(pub anyhow::Error);

/// The configuration file could not be used as written.
#[derive(Debug, thiserror::Error)]
#[error("{0:#}")]
pub struct InvalidConfiguration(pub anyhow::Error);

pub fn invalid_input(error: impl Into<anyhow::Error>) -> anyhow::Error {
    anyhow::Error::new(InvalidInput(error.into()))
}

pub fn invalid_configuration(error: impl Into<anyhow::Error>) -> anyhow::Error {
    anyhow::Error::new(InvalidConfiguration(error.into()))
}
