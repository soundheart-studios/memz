//! Durable, scoped memory. Product modules are introduced in subsequent tasks.
pub mod adapters;
pub mod auth;
pub mod backup;
pub mod cache;
pub mod capture;
pub mod catalog;
pub mod daemon;
pub mod export;
pub mod graph;
pub mod health;
pub mod import;
pub mod inspect;
pub mod jobs;
pub mod lifecycle;
pub mod providers;
pub mod retrieval;
pub mod setup;
pub mod state;
pub mod store;
pub mod vector;

#[derive(Debug)]
pub struct Error {
    pub code: &'static str,
    pub message: String,
}
impl Error {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::new("io", e.to_string())
    }
}
impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Self::new("storage", e.to_string())
    }
}
pub type Result<T> = std::result::Result<T, Error>;
