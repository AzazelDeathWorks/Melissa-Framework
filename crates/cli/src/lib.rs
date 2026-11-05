//! Local CLI host. Acknowledging a plan does not mint a process/model permit,
//! execute a worker, or replace independent protected acceptance.
pub mod app;
pub mod args;
#[cfg(all(test, windows))]
mod capabilities_tests;
pub mod context_bridge;
pub mod control;
pub mod development;
pub mod display;
pub mod domain;
pub mod engine;
pub mod preparation;
pub mod project_memory;
pub mod recipe;
mod repository_signals;
pub mod resources;
pub mod settings;
pub mod views;
pub type Result<T = ()> = std::result::Result<T, Error>;
#[derive(Debug)]
pub enum Error {
    Usage(&'static str),
    Denied(&'static str),
    Conflict(&'static str),
    Unavailable(&'static str),
    Reconciliation(&'static str),
    Store,
    Repository,
    Io,
}
impl Error {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Usage(_) => "usage",
            Self::Denied(_) => "denied",
            Self::Conflict(_) => "conflict",
            Self::Unavailable(_) => "unavailable",
            Self::Reconciliation(_) => "requires_reconciliation",
            Self::Store => "store_integrity",
            Self::Repository => "repository_boundary",
            Self::Io => "io",
        }
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Usage(s)
            | Self::Denied(s)
            | Self::Conflict(s)
            | Self::Unavailable(s)
            | Self::Reconciliation(s) => s,
            Self::Store => "Şifreli görev kaydı doğrulanamadı; mevcut dosyaları koruyup inceleyin.",
            Self::Repository => {
                "Çalışma klasörü güvenli dosya sınırlarını veya kaynak sürümü denetimini geçmedi."
            }
            Self::Io => "Yerel dosya işlemi tamamlanamadı; mevcut dosyalar korunur.",
        })
    }
}
impl std::error::Error for Error {}
impl From<melissa_local_store::StoreError> for Error {
    fn from(_: melissa_local_store::StoreError) -> Self {
        Self::Store
    }
}
impl From<melissa_repository::Error> for Error {
    fn from(_: melissa_repository::Error) -> Self {
        Self::Repository
    }
}
impl From<std::io::Error> for Error {
    fn from(_: std::io::Error) -> Self {
        Self::Io
    }
}
pub(crate) fn digest(b: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(b))
}

mod model_pool;
pub mod model_tools;
