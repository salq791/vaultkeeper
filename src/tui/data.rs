use std::sync::{mpsc, Arc};

/// Events the UI thread applies to `App` state. `Refreshed` comes from the
/// synchronous `DataHub::refresh`; the other three are produced by Task 4's
/// worker threads (`spawn_backup`/`spawn_verify`/`load_snapshots`) below.
pub enum Event {
    Refreshed {
        sources: Vec<crate::store::SourceMeta>,
        runs: Vec<crate::store::RunView>,
    },
    Snapshots {
        source: String,
        snapshots: Vec<crate::restic::Snapshot>,
    },
    ActionDone(String),
    ActionFailed(String),
}

/// Owns the config and db path needed to talk to the store, plus the
/// channel the background workers spawned below use to report `Event`s back
/// to the UI thread. `new` opens nothing eagerly: no file handle or
/// connection is held between calls, so a slow or unreachable DB never
/// blocks startup beyond the first explicit `refresh()`.
pub struct DataHub {
    // Cloned into every spawned worker so it can build engine/restic config
    // off the render thread; `refresh()` only needs `db_path`.
    cfg: Arc<crate::config::Config>,
    db_path: String,
    // Cloned by each spawned worker to send its `Event` back once it
    // finishes; the UI thread only ever drains the paired `rx`.
    tx: mpsc::Sender<Event>,
    rx: mpsc::Receiver<Event>,
}

impl DataHub {
    pub fn new(cfg: crate::config::Config, db_path: String) -> anyhow::Result<DataHub> {
        let (tx, rx) = mpsc::channel();
        Ok(DataHub {
            cfg: Arc::new(cfg),
            db_path,
            tx,
            rx,
        })
    }

    /// Synchronous SQLite read: opens a fresh `Store`, reads the non-secret
    /// source list and recent run history, and returns them as one event.
    pub fn refresh(&self) -> anyhow::Result<Event> {
        let store =
            crate::store::Store::open(&self.db_path, crate::crypto::MasterKey::from_env()?)?;
        let sources = store.list_sources_meta()?;
        let runs = store.recent_runs_view(200)?;
        Ok(Event::Refreshed { sources, runs })
    }

    /// Drains one pending event from a worker without blocking; `None` when
    /// the channel is empty.
    pub fn try_recv(&self) -> Option<Event> {
        self.rx.try_recv().ok()
    }

    /// Runs a backup for `name` on a background thread and reports the
    /// outcome as an `Event`, never touching the render thread.
    pub fn spawn_backup(&self, name: String) {
        let tx = self.tx.clone();
        let cfg = self.cfg.clone();
        let db = self.db_path.clone();
        std::thread::spawn(move || {
            let label = action_label("backup", &name);
            let res = crate::exec::execute_source(&cfg, &db, &name);
            let _ = match res {
                Ok(out) => tx.send(Event::ActionDone(format!("{label}: {}", out.status))),
                Err(e) => tx.send(Event::ActionFailed(format!("{label}: {e:#}"))),
            };
        });
    }

    /// Runs a restore-and-check verify for `name` on a background thread;
    /// identical shape to `spawn_backup` but calls `execute_verify`.
    pub fn spawn_verify(&self, name: String) {
        let tx = self.tx.clone();
        let cfg = self.cfg.clone();
        let db = self.db_path.clone();
        std::thread::spawn(move || {
            let label = action_label("verify", &name);
            let res = crate::exec::execute_verify(&cfg, &db, &name);
            let _ = match res {
                Ok(out) => tx.send(Event::ActionDone(format!("{label}: {}", out.status))),
                Err(e) => tx.send(Event::ActionFailed(format!("{label}: {e:#}"))),
            };
        });
    }

    /// Lists `name`'s snapshots on a background thread, sorted newest first
    /// (parsed RFC3339 time, same comparator as `restic::latest_snapshot`
    /// but reversed since that helper wants oldest-last for `.pop()`).
    pub fn load_snapshots(&self, name: String) {
        let tx = self.tx.clone();
        let cfg = self.cfg.clone();
        std::thread::spawn(move || {
            use crate::restic::Repo as _;
            let repo = crate::exec::build_repo(&cfg);
            let tag = format!("source={name}");
            match repo.snapshots(Some(&tag)) {
                Ok(mut snaps) => {
                    snaps.sort_by_key(|s| {
                        chrono::DateTime::parse_from_rfc3339(&s.time)
                            .map(|t| t.timestamp())
                            .unwrap_or(i64::MIN)
                    });
                    snaps.reverse();
                    let _ = tx.send(Event::Snapshots {
                        source: name,
                        snapshots: snaps,
                    });
                }
                Err(e) => {
                    let _ = tx.send(Event::ActionFailed(format!("snapshots {name}: {e:#}")));
                }
            }
        });
    }
}

/// Shared label formatter for busy-list entries and action-outcome
/// messages, so `mod.rs`'s dispatch (labeling `app.busy`) and these workers
/// (labeling their `Event`) never drift apart.
pub fn action_label(kind: &str, name: &str) -> String {
    format!("{kind} {name}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_labels() {
        assert_eq!(action_label("backup", "a-db"), "backup a-db");
    }
}
