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
        rx.recv_timeout(Duration::from_secs(30))
            .map_err(|e| format!("History save did not finish: {e}"))?
    }
}
