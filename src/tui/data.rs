use std::sync::{mpsc, Arc};

/// Events the UI thread applies to `App` state. Task 3 only ever produces
/// `Refreshed` (via `DataHub::refresh`); the other variants are produced by
/// Task 4's worker threads once they exist, so they're unconstructed here.
pub enum Event {
    Refreshed {
        sources: Vec<crate::store::SourceMeta>,
        runs: Vec<crate::store::RunView>,
    },
    // Consumed by plan-6 Task 4 (snapshot-loading worker).
    #[allow(dead_code)]
    Snapshots {
        source: String,
        snapshots: Vec<crate::restic::Snapshot>,
    },
    // Consumed by plan-6 Task 4+ (action workers reporting completion).
    #[allow(dead_code)]
    ActionDone(String),
    // Consumed by plan-6 Task 4+ (action workers reporting failure).
    #[allow(dead_code)]
    ActionFailed(String),
}

/// Owns the config and db path needed to talk to the store, plus the
/// channel Task 4's background workers will use to report `Event`s back to
/// the UI thread. `new` opens nothing eagerly: no file handle or connection
/// is held between calls, so a slow or unreachable DB never blocks startup
/// beyond the first explicit `refresh()`.
pub struct DataHub {
    // Held for Task 4's workers, which need it to build engine/restic
    // config; Task 3's synchronous `refresh()` only needs `db_path`.
    #[allow(dead_code)]
    cfg: Arc<crate::config::Config>,
    db_path: String,
    // Cloned by Task 4's spawned workers to send `Event`s back once they
    // finish; Task 3 never sends on this end, only drains `rx`.
    #[allow(dead_code)]
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
}
