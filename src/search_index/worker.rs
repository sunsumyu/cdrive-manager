//! Background worker for maintaining the search index.
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
use crate::search_index::db::{
    build_index_from_scan, delete_entry, delete_entry_by_name, lookup_frn_path,
    resolve_path_from_frn, search_by_name, upsert_entry, upsert_frn_path,
    delete_frn_path, FileSearchResult,
};
use crate::search_index::usn_journal::{spawn_usn_listener, UsnEvent, UsnListenerConfig};

/// Events reported by the search index worker.
#[derive(Debug, Clone)]
pub enum SearchIndexEvent {
    /// Index build started.
    Building { root_key: String, total_files: u64 },
    /// Progress update during index build.
    Progress { root_key: String, processed: u64 },
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
        if worker_cancel.load(Ordering::Relaxed) {
            return;
        }

        let root_key = crate::search_index::db::root_key(&stats.root);
        let total_files = stats.file_count;

        let _ = sender.send(SearchIndexEvent::Building {
            root_key: root_key.clone(),
            total_files,
        });

        match build_index_from_scan(&stats) {
            Ok(total) => {
                let _ = sender.send(SearchIndexEvent::Finished {
                    root_key,
                    total_entries: total as u64,
                });
            }
            Err(e) => {
                let _ = sender.send(SearchIndexEvent::Error(format!("Index build failed: {}", e)));
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

            match event {
                UsnEvent::FileCreated { frn, parent_frn, file_name, is_directory } => {
                    // Resolve parent path from FRN
                    let parent_path = resolve_path_from_frn(&root_key, &parent_frn, &file_name)
                        .ok()
                        .flatten()
                        .unwrap_or_else(|| format!("FRN:{}/{}", parent_frn, file_name));

                    let path = std::path::Path::new(&parent_path).join(&file_name).to_string_lossy().to_string();

                    // Upsert FRN mapping
                    let _ = upsert_frn_path(&root_key, &frn, &path, Some(&parent_frn), is_directory);

                    // Upsert search index
                    if let Err(e) = upsert_entry(
                        &root_key,
                        &file_name,
                        &path,
                        &parent_path,
                        std::path::Path::new(&file_name).extension().and_then(|e| e.to_str()),
                        0,
                        None,
                        is_directory,
                    ) {
                        eprintln!("USN upsert error: {}", e);
                    }
                    total_changes += 1;
                }
                UsnEvent::FileDeleted { frn, parent_frn, file_name } => {
                    // Look up the stored path before wiping the FRN map so we
                    // can target the search-index row by path (the index keys
                    // on path, not FRN).
                    let stored_path = lookup_frn_path(&root_key, &frn);
                    let _ = delete_frn_path(&root_key, &frn);

                    // Delete from search index. Prefer the stored path; fall
                    // back to the parent_frn-resolved path; finally fall back
                    // to deleting by name under the resolved parent so we don't
                    // leave orphan rows.
                    let path = stored_path
                        .or_else(|| {
                            resolve_path_from_frn(&root_key, &parent_frn, &file_name)
                                .ok()
                                .flatten()
                        });
                    if let Some(p) = path {
                        let _ = delete_entry(&p);
                    } else {
                        // Last resort: drop any rows whose path ends with the
                        // deleted file name. This catches paths we never
                        // resolved but still indexed under a FRN: stub.
                        let _ = delete_entry_by_name(&root_key, &file_name);
                    }
                    total_changes += 1;
                }
                UsnEvent::FileModified { frn: _, parent_frn, file_name } => {
                    // Re-resolve path and update
                    let parent_path = resolve_path_from_frn(&root_key, &parent_frn, &file_name)
                        .ok()
                        .flatten()
                        .unwrap_or_else(|| format!("FRN:{}/{}", parent_frn, file_name));

                    let path = std::path::Path::new(&parent_path).join(&file_name).to_string_lossy().to_string();

                    if let Err(e) = upsert_entry(
                        &root_key,
                        &file_name,
                        &path,
                        &parent_path,
                        std::path::Path::new(&file_name).extension().and_then(|e| e.to_str()),
                        0,
                        None,
                        false,
                    ) {
                        eprintln!("USN modify error: {}", e);
                    }
                    total_changes += 1;
                }
                UsnEvent::FileRenamed { old_frn, new_frn, parent_frn, file_name } => {
                    // Delete old, insert new
                    let _ = delete_frn_path(&root_key, &old_frn);

                    let parent_path = resolve_path_from_frn(&root_key, &parent_frn, &file_name)
                        .ok()
                        .flatten()
                        .unwrap_or_else(|| format!("FRN:{}/{}", parent_frn, file_name));

                    let path = std::path::Path::new(&parent_path).join(&file_name).to_string_lossy().to_string();

                    // Insert new FRN mapping
                    let _ = upsert_frn_path(&root_key, &new_frn, &path, Some(&parent_frn), false);

                    // Update search index
                    let _ = upsert_entry(
                        &root_key,
                        &file_name,
                        &path,
                        &parent_path,
                        std::path::Path::new(&file_name).extension().and_then(|e| e.to_str()),
                        0,
                        None,
                        false,
                    );
                    total_changes += 1;
                }
            }

            // Report progress every 100 changes
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
    /// Search failed with a database error.
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

/// Spawn a background search. The worker thread runs `search_by_name` against
/// the FTS5-backed index and sends the result through the returned receiver.
/// Cancellation is best-effort: SQLite itself can't be interrupted mid-query,
/// but the flag is checked before the query starts so a queued-up result can be
/// discarded by the caller when it arrives.
pub fn spawn_search(root_key: String, query: String, limit: usize) -> SearchHandle {
    let (sender, receiver) = unbounded::<SearchResult>();
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancel_flag);

    thread::spawn(move || {
        if worker_cancel.load(Ordering::Relaxed) {
            return;
        }
        match search_by_name(&root_key, &query, limit) {
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
