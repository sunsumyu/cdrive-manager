//! Background worker for maintaining the tantivy search index.
//!
//! Provides background threads that:
//! 1. Build the index from scan results
//! 2. Listen to USN Journal for file system changes (incremental updates)
//! 3. Run ad-hoc searches off the UI thread so typing stays responsive

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use crossbeam_channel::{Receiver, unbounded};

use crate::model::ScanStats;
use crate::search_index::indexer::{FileSearchResult, SearchIndexer};
use crate::search_index::usn_journal::{spawn_usn_listener, UsnEvent, UsnListenerConfig};

/// Events reported by the search index worker.
#[derive(Debug, Clone)]
pub enum SearchIndexEvent {
    /// Index build started.
    Building { root_key: String, total_files: u64 },
    /// Index build completed.
    Finished { root_key: String, total_entries: u64 },
    /// Incremental update applied.
    Updated { root_key: String, changes: u64 },
    /// An error occurred.
    Error(String),
}

/// Handle to control and receive events from the search index worker.
pub struct SearchIndexHandle {
    pub receiver: Receiver<SearchIndexEvent>,
    cancel_flag: Arc<AtomicBool>,
}

impl SearchIndexHandle {
    pub fn cancel(&self) {
        self.cancel_flag.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel_flag.load(Ordering::Relaxed)
    }
}

/// Spawn a background worker to build the search index from scan results.
pub fn spawn_build_index(stats: Arc<ScanStats>) -> SearchIndexHandle {
    let (sender, receiver) = unbounded::<SearchIndexEvent>();
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancel_flag);

    thread::spawn(move || {
        let root_key = crate::search_index::indexer::root_key(&stats.root);
        let total_files = stats.file_count;
        let _ = sender.send(SearchIndexEvent::Building {
            root_key: root_key.clone(),
            total_files,
        });

        let indexer = match SearchIndexer::open() {
            Ok(x) => x,
            Err(e) => {
                let _ = sender.send(SearchIndexEvent::Error(format!(
                    "Failed to open search indexer: {e}"
                )));
                return;
            }
        };

        let root_key_clone = root_key.clone();
        let sender_clone = sender.clone();
        let worker_cancel_clone = worker_cancel.clone();
        let progress = move |p: u64, _total: u64| {
            if p % 1000 == 0 && !worker_cancel_clone.load(Ordering::Relaxed) {
                let _ = sender_clone.send(SearchIndexEvent::Updated {
                    root_key: root_key_clone.clone(),
                    changes: p,
                });
            }
        };

        if worker_cancel.load(Ordering::Relaxed) {
            return;
        }

        match indexer.build_from_scan(&stats, &root_key, progress) {
            Ok(cnt) => {
                let _ = sender.send(SearchIndexEvent::Finished {
                    root_key,
                    total_entries: cnt,
                });
            }
            Err(e) => {
                let _ = sender.send(SearchIndexEvent::Error(format!(
                    "Index build failed: {e}"
                )));
            }
        }
    });

    SearchIndexHandle { receiver, cancel_flag }
}

/// Spawn a background worker that listens for file system changes and updates the index.
pub fn spawn_usn_index_listener(
    root_key: String,
    drive_letter: char,
) -> SearchIndexHandle {
    let (sender, receiver) = unbounded::<SearchIndexEvent>();
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancel_flag);

    thread::spawn(move || {
        let indexer = match SearchIndexer::open() {
            Ok(x) => x,
            Err(e) => {
                let _ = sender.send(SearchIndexEvent::Error(format!(
                    "Failed to open search indexer for USN listener: {e}"
                )));
                return;
            }
        };

        let config = UsnListenerConfig {
            drive_letter,
            cancel_flag: worker_cancel.clone(),
        };

        let handle = spawn_usn_listener(config);
        let mut total_changes: u64 = 0;

        while let Ok(event) = handle.receiver.recv_timeout(std::time::Duration::from_millis(500)) {
            if worker_cancel.load(Ordering::Relaxed) {
                break;
            }

            if let Err(e) = indexer.handle_usn_event(event, &root_key) {
                eprintln!("USN event error: {e}");
            }
            total_changes += 1;

            if total_changes.is_multiple_of(100) {
                let _ = sender.send(SearchIndexEvent::Updated {
                    root_key: root_key.clone(),
                    changes: total_changes,
                });
            }
        }
    });

    SearchIndexHandle { receiver, cancel_flag }
}

/// Outcome of an asynchronous search query, delivered on the receiver.
#[derive(Debug, Clone)]
pub enum SearchResult {
    /// Query completed; carries the matched rows.
    Ok { query: String, results: Vec<FileSearchResult> },
    /// Search failed.
    Error(String),
}

/// Handle returned by [`spawn_search`]. Poll [`SearchHandle::receiver`] to
/// receive the result; call [`SearchHandle::cancel`] to abandon.
pub struct SearchHandle {
    pub receiver: Receiver<SearchResult>,
    cancel_flag: Arc<AtomicBool>,
}

impl SearchHandle {
    pub fn cancel(&self) {
        self.cancel_flag.store(true, Ordering::Relaxed);
    }
}

/// Spawn a background search against the tantivy index.
pub fn spawn_search(root_key: String, query: String, limit: usize) -> SearchHandle {
    let (sender, receiver) = unbounded::<SearchResult>();
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancel_flag);

    thread::spawn(move || {
        if worker_cancel.load(Ordering::Relaxed) {
            return;
        }
        let indexer = match SearchIndexer::open() {
            Ok(x) => x,
            Err(e) => {
                let _ = sender.send(SearchResult::Error(format!(
                    "Failed to open search indexer: {e}"
                )));
                return;
            }
        };
        match indexer.search(&root_key, &query, limit) {
            Ok(results) => {
                let _ = sender.send(SearchResult::Ok { query, results });
            }
            Err(e) => {
                let _ = sender.send(SearchResult::Error(e.to_string()));
            }
        }
    });

    SearchHandle { receiver, cancel_flag }
}
