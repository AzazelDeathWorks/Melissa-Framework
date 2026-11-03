//! CODE-11 source-bound review and preliminary quality gates.
//!
//! A located finding is a candidate, not a proven defect. Developer check
//! claims are untrusted. Neither can mint protected acceptance or publish.
pub mod candidate;
pub mod package;
pub mod profiles;
pub mod protected;
pub mod review;
pub mod security;
pub mod source;
pub mod supply_chain;
pub mod tools;
pub mod vault;
pub mod verification_tools;

pub enum Error {
    Invalid(&'static str),
    Denied(&'static str),
    Stale(&'static str),
    Cancelled,
    Repository(melissa_repository::Error),
    Store(melissa_local_store::StoreError),
    Json(serde_json::Error),
    AfterAppend,
}
impl std::fmt::Debug for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}
pub type Result<T = ()> = std::result::Result<T, Error>;
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(s) => write!(f, "invalid quality input: {s}"),
            Self::Denied(s) => write!(f, "quality authority denied: {s}"),
            Self::Stale(s) => write!(f, "quality source is stale: {s}"),
            Self::Cancelled => f.write_str("quality operation cancelled"),
            Self::Repository(_) => f.write_str("quality repository validation failed"),
            Self::Store(_) => f.write_str("quality evidence persistence failed"),
            Self::Json(_) => f.write_str("quality JSON violates its exact schema"),
            Self::AfterAppend => {
                f.write_str("evidence append may have completed; inspect before retry")
            }
        }
    }
}
impl std::error::Error for Error {}
impl From<melissa_repository::Error> for Error {
    fn from(e: melissa_repository::Error) -> Self {
        Self::Repository(e)
    }
}
impl From<melissa_local_store::StoreError> for Error {
    fn from(e: melissa_local_store::StoreError) -> Self {
        Self::Store(e)
    }
}
impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}
pub fn digest(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes))
}
pub(crate) fn digest_valid(s: &str) -> bool {
    s.len() == 71
        && s.starts_with("blake3:")
        && s[7..]
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
pub(crate) fn bounded(s: &str, maximum: usize) -> bool {
    !s.trim().is_empty() && s.len() <= maximum && !s.contains('\0')
}
