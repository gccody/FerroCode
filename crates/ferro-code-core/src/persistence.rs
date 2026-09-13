use crate::PersistedState;
use std::{
    fmt, fs, io,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Debug)]
pub enum StoreError {
    Io(io::Error),
    Json(serde_json::Error),
    UnsupportedSchema,
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::Json(error) => write!(f, "{error}"),
            Self::UnsupportedSchema => write!(
                f,
                "History was saved by an unsupported version. Open it with a compatible Ferro Code version."
            ),
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
            Ok(bytes) => decode_snapshot(&bytes),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let Some(legacy_path) = &self.legacy_path else {
                    return Ok(PersistedState::default());
                };
                match fs::read(legacy_path) {
                    Ok(bytes) => decode_snapshot(&bytes),
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
        self.open_session()?.save(state)
    }

    /// A lifetime lock prevents two windows from overwriting each other's state.
    pub fn open_session(&self) -> Result<StoreSession, StoreError> {
        let parent = self.path.parent().unwrap_or(Path::new("."));
        fs::create_dir_all(parent)?;
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.path.with_extension("lock"))?;
        lock.try_lock().map_err(|error| {
            io::Error::other(format!(
                "Local history is already open in another Ferro Code window: {error}"
            ))
        })?;
        Ok(StoreSession {
            store: self.clone(),
            _lock: lock,
        })
    }

    pub fn attachments_dir(&self) -> PathBuf {
        self.path
            .parent()
            .unwrap_or(Path::new("."))
            .join("attachments")
    }

    pub fn import_attachment(&self, source: &Path) -> Result<PathBuf, StoreError> {
        let directory = self.attachments_dir();
        fs::create_dir_all(&directory)?;
        if source.canonicalize().ok().is_some_and(|p| {
            directory
                .canonicalize()
                .ok()
                .is_some_and(|dir| p.starts_with(dir))
        }) {
            return Ok(source.to_owned());
        }
        let filename = source
            .file_name()
            .ok_or_else(|| io::Error::other("Attachment has no filename"))?
            .to_string_lossy();
        let directory = directory.join(crate::new_id("file"));
        fs::create_dir(&directory)?;
        let target = directory.join(filename.as_ref());
        let mut input = fs::File::open(source)?;
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)?;
        io::copy(&mut input, &mut output)?;
        output.sync_all()?;
        Ok(target)
    }

    pub fn export(&self, state: &PersistedState, target: &Path) -> Result<(), StoreError> {
        if let (Ok(target), Ok(primary)) = (target.canonicalize(), self.path.canonicalize()) {
            if target == primary
                || target
                    == self
                        .path
                        .with_extension("json.bak")
                        .canonicalize()
                        .unwrap_or_default()
            {
                return Err(io::Error::other(
                    "Choose an export path outside the active history files",
                )
                .into());
            }
        }
        // Export is portable: all referenced files accompany the JSON snapshot.
        let export_store = LocalStore::new(
            target
                .parent()
                .unwrap_or(Path::new("."))
                .join(format!(
                    "{}.files",
                    target.file_stem().unwrap_or_default().to_string_lossy()
                ))
                .join("state.json"),
        );
        let mut state = state.clone();
        remap_attachments(&mut state, |path| {
            export_store.import_attachment(Path::new(path)).map(|p| {
                p.strip_prefix(target.parent().unwrap_or(Path::new(".")))
                    .unwrap_or(&p)
                    .to_string_lossy()
                    .into_owned()
            })
        })?;
        atomic_write(target, &encode_snapshot(&state)?)
    }

    pub fn import(&self, source: &Path) -> Result<PersistedState, StoreError> {
        let mut state = decode_snapshot(&fs::read(source)?)?;
        remap_attachments(&mut state, |path| {
            let path = Path::new(path);
            let source_path = if path.is_absolute() {
                path.to_owned()
            } else {
                source.parent().unwrap_or(Path::new(".")).join(path)
            };
            self.import_attachment(&source_path)
                .map(|p| p.to_string_lossy().into_owned())
        })?;
        Ok(state)
    }
}

/// A transaction is a synced snapshot atomically replacing the previous one.
/// The previous validated generation remains available for recovery.
pub struct StoreSession {
    store: LocalStore,
    _lock: fs::File,
}
impl StoreSession {
    pub fn load(&self) -> Result<(PersistedState, Option<String>), StoreError> {
        let primary = self.store.load();
        if matches!(&primary, Err(StoreError::UnsupportedSchema)) {
            return primary.map(|state| (state, None));
        }
        if primary.is_ok() && self.store.path.exists() {
            return primary.map(|state| (state, None));
        }
        for candidate in [
            self.store.path.with_extension("json.bak"),
            self.store.path.with_extension("json.tmp"),
        ] {
            if !candidate.exists() {
                continue;
            }
            match fs::read(&candidate)
                .map_err(StoreError::from)
                .and_then(|bytes| decode_snapshot(&bytes))
            {
                Ok(state) => {
                    self.preserve_current()?;
                    atomic_write(&self.store.path, &encode_snapshot(&state)?)?;
                    return Ok((state, Some("Recovered local history from a saved generation. The damaged file was preserved.".into())));
                }
                Err(error) if primary.is_ok() => return Err(error),
                Err(_) => {}
            }
        }
        primary.map(|state| (state, None))
    }
    fn preserve_current(&self) -> Result<(), StoreError> {
        if self.store.path.exists() {
            let preserved = self
                .store
                .path
                .with_extension(format!("{}.json", crate::new_id("preserved")));
            fs::copy(&self.store.path, preserved)?;
        }
        Ok(())
    }
    pub fn restore(&self, state: &PersistedState) -> Result<(), StoreError> {
        self.preserve_current()?;
        atomic_write(&self.store.path, &encode_snapshot(state)?)
    }
    pub fn save(&self, state: &PersistedState) -> Result<(), StoreError> {
        if self.store.path.exists() {
            let previous = fs::read(&self.store.path)?;
            decode_snapshot(&previous)?; // Never replace unreadable history with defaults.
            atomic_write(&self.store.path.with_extension("json.bak"), &previous)?;
        }
        atomic_write(&self.store.path, &encode_snapshot(state)?)
    }
}

fn encode_snapshot(state: &PersistedState) -> Result<Vec<u8>, StoreError> {
    Ok(serde_json::to_vec_pretty(
        &serde_json::json!({"schema_version":1, "state":state}),
    )?)
}
fn decode_snapshot(bytes: &[u8]) -> Result<PersistedState, StoreError> {
    let value: serde_json::Value = serde_json::from_slice(bytes)?;
    let state = if let Some(version) = value.get("schema_version") {
        if version.as_u64() != Some(1) {
            return Err(StoreError::UnsupportedSchema);
        }
        value
            .get("state")
            .cloned()
            .ok_or_else(|| io::Error::other("Missing history snapshot"))?
    } else {
        value
    };
    if !state.is_object() || (state.get("history").is_none() && state.get("preferences").is_none())
    {
        return Err(io::Error::other("This file is not a Ferro Code history snapshot").into());
    }
    Ok(serde_json::from_value(state)?)
}
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    let parent = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    staged.write_all(bytes)?;
    staged.as_file().sync_all()?;
    staged.persist(path).map_err(|error| error.error)?;
    #[cfg(unix)]
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

fn remap_attachments(
    state: &mut PersistedState,
    mut copy: impl FnMut(&str) -> Result<String, StoreError>,
) -> Result<(), StoreError> {
    let mut paths = std::collections::HashMap::<String, String>::new();
    let all = state
        .history
        .threads
        .iter_mut()
        .chain(state.history.archived_threads.iter_mut())
        .flat_map(|thread| thread.messages.iter_mut())
        .flat_map(|message| message.attachments.iter_mut())
        .chain(
            state
                .history
                .drafts
                .values_mut()
                .flat_map(|draft| draft.attachments.iter_mut()),
        );
    for path in all {
        let replacement = if let Some(replacement) = paths.get(path) {
            replacement.clone()
        } else {
            let replacement = copy(path)?;
            paths.insert(path.clone(), replacement.clone());
            replacement
        };
        *path = replacement;
    }
    Ok(())
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

#[cfg(test)]
mod recovery_tests {
    use super::*;
    #[test]
    fn invalid_primary_recovers_backup_and_preserves_original() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStore::new(dir.path().join("state.json"));
        let mut state = PersistedState::default();
        state.preferences.model = "first".into();
        store.save(&state).unwrap();
        state.preferences.model = "second".into();
        store.save(&state).unwrap();
        fs::write(store.path(), b"broken").unwrap();
        let session = store.open_session().unwrap();
        let (state, warning) = session.load().unwrap();
        assert_eq!(state.preferences.model, "first");
        assert!(warning.is_some());
        assert!(
            fs::read_dir(dir.path())
                .unwrap()
                .flatten()
                .any(|e| e.file_name().to_string_lossy().contains("preserved"))
        );
    }
    #[test]
    fn invalid_primary_without_backup_is_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStore::new(dir.path().join("state.json"));
        fs::write(store.path(), b"broken").unwrap();
        assert!(store.save(&PersistedState::default()).is_err());
        assert_eq!(fs::read(store.path()).unwrap(), b"broken");
    }
    #[test]
    fn only_one_writer_can_open_history() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStore::new(dir.path().join("state.json"));
        let session = store.open_session().unwrap();
        assert!(store.open_session().is_err());
        drop(session);
        assert!(store.open_session().is_ok());
    }
    #[test]
    fn attachments_survive_source_removal_and_export() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.txt");
        fs::write(&source, b"contents").unwrap();
        let store = LocalStore::new(dir.path().join("app/state.json"));
        let imported = store.import_attachment(&source).unwrap();
        fs::remove_file(source).unwrap();
        assert_eq!(fs::read(imported).unwrap(), b"contents");
        let export = dir.path().join("export.json");
        store.export(&PersistedState::default(), &export).unwrap();
        assert!(store.import(&export).is_ok());
    }
    #[test]
    fn portable_export_restores_attachments_after_moving_backup() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("original.txt");
        fs::write(&source, "durable").unwrap();
        let store = LocalStore::new(dir.path().join("app/state.json"));
        let mut state = PersistedState::default();
        state.history.drafts.insert(
            "draft".into(),
            crate::Draft {
                text: "remember".into(),
                attachments: vec![source.to_string_lossy().into_owned()],
            },
        );
        let export_dir = dir.path().join("export");
        fs::create_dir(&export_dir).unwrap();
        store
            .export(&state, &export_dir.join("backup.json"))
            .unwrap();
        let moved = dir.path().join("moved");
        fs::rename(&export_dir, &moved).unwrap();
        fs::remove_file(source).unwrap();
        let restored = store.import(&moved.join("backup.json")).unwrap();
        assert_eq!(restored.history.drafts["draft"].text, "remember");
        assert_eq!(
            fs::read_to_string(&restored.history.drafts["draft"].attachments[0]).unwrap(),
            "durable"
        );
    }
    #[test]
    fn future_schema_is_preserved_even_when_backup_exists() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStore::new(dir.path().join("state.json"));
        store.save(&PersistedState::default()).unwrap();
        store.save(&PersistedState::default()).unwrap();
        let future = br#"{"schema_version":99,"state":{}}"#;
        fs::write(store.path(), future).unwrap();
        assert!(matches!(
            store.open_session().unwrap().load(),
            Err(StoreError::UnsupportedSchema)
        ));
        assert_eq!(fs::read(store.path()).unwrap(), future);
    }
    #[test]
    fn missing_primary_recovers_previous_generation() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStore::new(dir.path().join("state.json"));
        let mut state = PersistedState::default();
        state.preferences.model = "saved".into();
        store.save(&state).unwrap();
        store.save(&state).unwrap();
        fs::remove_file(store.path()).unwrap();
        let (recovered, warning) = store.open_session().unwrap().load().unwrap();
        assert_eq!(recovered.preferences.model, "saved");
        assert!(warning.is_some());
    }
    #[test]
    fn export_cannot_overwrite_open_history() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStore::new(dir.path().join("state.json"));
        store.save(&PersistedState::default()).unwrap();
        assert!(
            store
                .export(&PersistedState::default(), store.path())
                .is_err()
        );
        assert!(decode_snapshot(b"{}").is_err());
    }
    #[test]
    fn failed_atomic_replace_keeps_destination() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("directory");
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("sentinel"), "safe").unwrap();
        assert!(atomic_write(&destination, b"replacement").is_err());
        assert_eq!(
            fs::read_to_string(destination.join("sentinel")).unwrap(),
            "safe"
        );
    }
}
