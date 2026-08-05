use crate::PersistedState;
use std::{
    fmt, fs, io,
    path::{Path, PathBuf},
};

#[derive(Debug)]
pub enum StoreError {
    Io(io::Error),
    Json(serde_json::Error),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::Json(error) => write!(f, "{error}"),
        }
    }
}
impl std::error::Error for StoreError {}
impl From<io::Error> for StoreError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}
impl From<serde_json::Error> for StoreError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

#[derive(Debug, Clone)]
pub struct LocalStore {
    path: PathBuf,
}

impl LocalStore {
    pub fn discover() -> Self {
        let base = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        Self::new(base.join("CodeAgent").join("state.json"))
    }
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn load(&self) -> Result<PersistedState, StoreError> {
        match fs::read(&self.path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(PersistedState::default()),
            Err(error) => Err(error.into()),
        }
    }
    pub fn save(&self, state: &PersistedState) -> Result<(), StoreError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temporary = self.path.with_extension("json.tmp");
        fs::write(&temporary, serde_json::to_vec_pretty(state)?)?;
        if self.path.exists() {
            fs::remove_file(&self.path)?;
        }
        fs::rename(temporary, &self.path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_store_loads_defaults_and_save_round_trips() {
        let path =
            std::env::temp_dir().join(format!("codeagent-store-{}.json", std::process::id()));
        let _ = fs::remove_file(&path);
        let store = LocalStore::new(&path);
        let mut state = store.load().unwrap();
        state.preferences.model = "test-model".into();
        state.preferences.effort = "high".into();
        store.save(&state).unwrap();
        let restored = store.load().unwrap();
        assert_eq!(restored.preferences.model, "test-model");
        assert_eq!(restored.preferences.effort, "high");
        let _ = fs::remove_file(path);
    }
}
