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
    legacy_path: Option<PathBuf>,
}

impl LocalStore {
    pub fn discover() -> Self {
        let base = state_base_dir();
        Self {
            path: base.join(state_directory_name()).join("state.json"),
            legacy_path: legacy_state_path(&base),
        }
    }
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            legacy_path: None,
        }
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn load(&self) -> Result<PersistedState, StoreError> {
        match fs::read(&self.path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let Some(legacy_path) = &self.legacy_path else {
                    return Ok(PersistedState::default());
                };
                match fs::read(legacy_path) {
                    Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        Ok(PersistedState::default())
                    }
                    Err(error) => Err(error.into()),
                }
            }
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

#[cfg(windows)]
fn state_base_dir() -> PathBuf {
    non_empty_env_path("LOCALAPPDATA")
        .or_else(|| non_empty_env_path("APPDATA"))
        .unwrap_or_else(fallback_state_base_dir)
}

#[cfg(target_os = "macos")]
fn state_base_dir() -> PathBuf {
    non_empty_env_path("HOME")
        .map(|home| home.join("Library").join("Application Support"))
        .unwrap_or_else(fallback_state_base_dir)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn state_base_dir() -> PathBuf {
    non_empty_env_path("XDG_DATA_HOME")
        .or_else(|| non_empty_env_path("HOME").map(|home| home.join(".local").join("share")))
        .unwrap_or_else(fallback_state_base_dir)
}

#[cfg(not(any(windows, unix)))]
fn state_base_dir() -> PathBuf {
    fallback_state_base_dir()
}

fn non_empty_env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn fallback_state_base_dir() -> PathBuf {
    std::env::temp_dir()
}

#[cfg(any(windows, target_os = "macos"))]
fn state_directory_name() -> &'static str {
    "Ferro Code"
}

#[cfg(not(any(windows, target_os = "macos")))]
fn state_directory_name() -> &'static str {
    "ferro-code"
}

#[cfg(windows)]
fn legacy_state_path(base: &Path) -> Option<PathBuf> {
    Some(base.join(concat!("Code", "Agent")).join("state.json"))
}

#[cfg(not(windows))]
fn legacy_state_path(_base: &Path) -> Option<PathBuf> {
    // Earlier builds fell back to the process working directory whenever
    // LOCALAPPDATA was unavailable, which was always the case on Unix.
    Some(
        std::env::current_dir()
            .unwrap_or_else(|_| fallback_state_base_dir())
            .join("Ferro Code")
            .join("state.json"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovered_store_uses_the_platform_data_directory_name() {
        let path = LocalStore::discover().path().to_path_buf();
        #[cfg(any(windows, target_os = "macos"))]
        assert!(path.ends_with(Path::new("Ferro Code").join("state.json")));
        #[cfg(not(any(windows, target_os = "macos")))]
        assert!(path.ends_with(Path::new("ferro-code").join("state.json")));
    }

    #[test]
    fn missing_store_loads_defaults_and_save_round_trips() {
        let path =
            std::env::temp_dir().join(format!("ferro-code-store-{}.json", std::process::id()));
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

    #[test]
    fn legacy_store_is_loaded_until_ferro_code_state_is_saved() {
        let base =
            std::env::temp_dir().join(format!("ferro-code-legacy-store-{}", std::process::id()));
        let legacy_path = base.join("legacy").join("state.json");
        let path = base.join("Ferro Code").join("state.json");
        fs::create_dir_all(legacy_path.parent().unwrap()).unwrap();

        let mut legacy_state = PersistedState::default();
        legacy_state.preferences.model = "legacy-model".into();
        fs::write(
            &legacy_path,
            serde_json::to_vec_pretty(&legacy_state).unwrap(),
        )
        .unwrap();

        let store = LocalStore {
            path: path.clone(),
            legacy_path: Some(legacy_path),
        };
        let restored = store.load().unwrap();
        assert_eq!(restored.preferences.model, "legacy-model");

        store.save(&restored).unwrap();
        assert!(path.exists());
        let _ = fs::remove_dir_all(base);
    }
}
