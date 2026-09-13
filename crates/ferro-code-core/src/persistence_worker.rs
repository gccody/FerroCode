use crate::{PersistedState, StoreSession};
use crossbeam_channel::{Receiver, Sender, bounded, unbounded};
use std::{thread, time::Duration};

struct Save {
    state: PersistedState,
    revision: u64,
    restore: bool,
    completion: Option<Sender<Result<(), String>>>,
}

pub struct SaveResult {
    pub revision: u64,
    pub result: Result<(), String>,
}

/// One writer owns the session lock. A bounded mailbox avoids accumulating
/// full-history snapshots if storage is slower than the autosave interval.
pub struct PersistenceWorker {
    requests: Sender<Save>,
    pub results: Receiver<SaveResult>,
}

impl PersistenceWorker {
    pub fn start(session: StoreSession, mut writable: bool) -> Result<Self, String> {
        let (tx, rx) = bounded::<Save>(1);
        let (result_tx, results) = unbounded();
        thread::Builder::new()
            .name("history-writer".into())
            .spawn(move || {
                for save in rx {
                    let result = if save.restore {
                        session.restore(&save.state).map_err(|e| e.to_string())
                    } else if writable {
                        session.save(&save.state).map_err(|e| e.to_string())
                    } else {
                        Err("History saving is paused. Restore a valid backup in Settings.".into())
                    };
                    if save.restore && result.is_ok() {
                        writable = true;
                    }
                    if let Some(completion) = save.completion {
                        let _ = completion.send(result.clone());
                    }
                    let _ = result_tx.send(SaveResult {
                        revision: save.revision,
                        result,
                    });
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            requests: tx,
            results,
        })
    }

    pub fn save(&self, state: PersistedState, revision: u64) -> bool {
        self.requests
            .try_send(Save {
                state,
                revision,
                restore: false,
                completion: None,
            })
            .is_ok()
    }

    /// Used on explicit restore and shutdown; acknowledgement means the synced
    /// transaction is on disk, not merely queued in memory.
    pub fn flush(&self, state: PersistedState, restore: bool) -> Result<(), String> {
        let (tx, rx) = bounded(1);
        self.requests
            .send_timeout(
                Save {
                    state,
                    revision: 0,
                    restore,
                    completion: Some(tx),
                },
                Duration::from_secs(30),
            )
            .map_err(|e| format!("History writer unavailable: {e}"))?;
        if restore {
            // Restore runs on the file worker. Do not resume autosaving the old
            // UI state while a slow restore could still complete on disk.
            rx.recv()
                .map_err(|e| format!("History writer stopped: {e}"))?
        } else {
            rx.recv_timeout(Duration::from_secs(30))
                .map_err(|e| format!("History save did not finish: {e}"))?
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LocalStore;
    #[test]
    fn paused_writer_requires_restore_then_serializes_subsequent_saves() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStore::new(dir.path().join("state.json"));
        std::fs::write(store.path(), "broken").unwrap();
        let writer = PersistenceWorker::start(store.open_session().unwrap(), false).unwrap();
        assert!(writer.flush(PersistedState::default(), false).is_err());
        assert_eq!(std::fs::read_to_string(store.path()).unwrap(), "broken");
        let mut state = PersistedState::default();
        state.preferences.model = "restored".into();
        writer.flush(state.clone(), true).unwrap();
        state.preferences.model = "latest".into();
        writer.flush(state, false).unwrap();
        assert_eq!(store.load().unwrap().preferences.model, "latest");
    }
}
