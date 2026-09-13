//! UI-independent domain types and durable local state for Ferro Code.

mod formatting;
mod model;
mod persistence;

pub use formatting::*;
pub use model::*;
pub use persistence::{LocalStore, StoreError, StoreSession};
mod persistence_worker;
pub use persistence_worker::{PersistenceWorker, SaveResult};

/// IDs do not depend on deletions, clock resolution, or process lifetime.
pub fn new_id(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4())
}
pub fn same_path(left: &str, right: &str) -> bool {
    #[cfg(windows)]
    {
        left.replace('/', "\\")
            .eq_ignore_ascii_case(&right.replace('/', "\\"))
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}
