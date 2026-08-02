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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{FileRecord, ScanStats};
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    // LOCALAPPDATA 覆盖是进程级的；使用 search_index::test_lock 共享的全局锁
    // 串行化所有涉及索引目录的测试（含 indexer.rs），避免并行时互相覆盖环境变量。

    struct LocalAppdataGuard {
        old: Option<OsString>,
    }
    impl LocalAppdataGuard {
        fn override_with(dir: &Path) -> Self {
            let old = std::env::var_os("LOCALAPPDATA");
            unsafe { std::env::set_var("LOCALAPPDATA", dir) };
            LocalAppdataGuard { old }
        }
    }
    impl Drop for LocalAppdataGuard {
        fn drop(&mut self) {
            if let Some(ref old) = self.old {
                unsafe { std::env::set_var("LOCALAPPDATA", old) };
            } else {
                unsafe { std::env::remove_var("LOCALAPPDATA") };
            }
        }
    }

    fn temp_cdrive_dir() -> PathBuf {
        std::env::temp_dir().join(format!(
            "cdrive-manager-e2e-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn make_test_stats() -> ScanStats {
        let mut stats = ScanStats::default();
        stats.root = PathBuf::from("C:\\test");
        stats.file_count = 3;
        stats.all_files = vec![
            FileRecord {
                path: PathBuf::from("C:\\test\\report.pdf"),
                size: 1024 * 1024,
                modified: Some(std::time::SystemTime::now()),
                extension: ".pdf".to_owned(),
            },
            FileRecord {
                path: PathBuf::from("C:\\test\\data.xlsx"),
                size: 2048,
                modified: Some(std::time::SystemTime::now()),
                extension: ".xlsx".to_owned(),
            },
            FileRecord {
                path: PathBuf::from("C:\\test\\sub\\notes.txt"),
                size: 64,
                modified: Some(std::time::SystemTime::now()),
                extension: ".txt".to_owned(),
            },
        ];
        stats
    }

    /// 端到端：spawn_build_index 建索引 → spawn_search 搜索 → 校验结果。
    #[test]
    fn e2e_build_then_search_round_trip() {
        let _lock = crate::search_index::test_lock::INDEX_TEST_LOCK.lock().unwrap();
        let cdrive_dir = temp_cdrive_dir();
        std::fs::create_dir_all(&cdrive_dir).unwrap();
        let _guard = LocalAppdataGuard::override_with(&cdrive_dir);

        let stats = Arc::new(make_test_stats());

        // 1) 后台构建索引
        let build_handle = spawn_build_index(stats);
        let root_key = crate::search_index::indexer::root_key(Path::new("C:\\test"));

        // 2) 等待 Finished 事件（最长 10 秒）
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut built_total: Option<u64> = None;
        loop {
            match build_handle
                .receiver
                .recv_timeout(Duration::from_millis(200))
            {
                Ok(SearchIndexEvent::Finished { total_entries, .. }) => {
                    built_total = Some(total_entries);
                    break;
                }
                Ok(SearchIndexEvent::Error(e)) => panic!("构建失败: {e}"),
                Ok(_) => continue,
                Err(_) if Instant::now() > deadline => panic!("等待构建完成超时"),
                Err(_) => continue,
            }
        }
        assert_eq!(built_total, Some(3), "应索引 3 个文件");

        // 3) 后台搜索
        let search_handle = spawn_search(root_key, "report".to_owned(), 10);
        let deadline = Instant::now() + Duration::from_secs(10);
        let result = loop {
            match search_handle.receiver.recv_timeout(Duration::from_millis(200)) {
                Ok(r) => break r,
                Err(_) if Instant::now() > deadline => panic!("等待搜索完成超时"),
                Err(_) => continue,
            }
        };

        match result {
            SearchResult::Ok { results, .. } => {
                assert_eq!(results.len(), 1, "应命中 report.pdf");
                assert_eq!(results[0].name, "report.pdf");
                assert_eq!(results[0].extension, "pdf");
            }
            SearchResult::Error(e) => panic!("搜索失败: {e}"),
        }
    }

    /// 端到端：搜索 DSL 高级语法（ext:/size:）走 worker 线程同样工作。
    #[test]
    fn e2e_search_dsl_field_queries() {
        let _lock = crate::search_index::test_lock::INDEX_TEST_LOCK.lock().unwrap();
        let cdrive_dir = temp_cdrive_dir();
        std::fs::create_dir_all(&cdrive_dir).unwrap();
        let _guard = LocalAppdataGuard::override_with(&cdrive_dir);

        let stats = Arc::new(make_test_stats());
        let build_handle = spawn_build_index(stats);
        let root_key = crate::search_index::indexer::root_key(Path::new("C:\\test"));

        // 等待构建完成
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match build_handle
                .receiver
                .recv_timeout(Duration::from_millis(200))
            {
                Ok(SearchIndexEvent::Finished { .. }) => break,
                Ok(SearchIndexEvent::Error(e)) => panic!("构建失败: {e}"),
                Ok(_) => continue,
                Err(_) if Instant::now() > deadline => panic!("等待构建完成超时"),
                Err(_) => continue,
            }
        }

        // ext:pdf → 1 个
        let handle = spawn_search(root_key.clone(), "ext:pdf".to_owned(), 10);
        match handle.receiver.recv_timeout(Duration::from_secs(5)).unwrap() {
            SearchResult::Ok { results, .. } => assert_eq!(results.len(), 1),
            SearchResult::Error(e) => panic!("搜索失败: {e}"),
        }

        // size:>500KB → 1 个 (report.pdf 1MB)
        let handle = spawn_search(root_key.clone(), "size:>500KB".to_owned(), 10);
        match handle.receiver.recv_timeout(Duration::from_secs(5)).unwrap() {
            SearchResult::Ok { results, .. } => assert_eq!(results.len(), 1),
            SearchResult::Error(e) => panic!("搜索失败: {e}"),
        }

        // 无匹配 → 0 个
        let handle = spawn_search(root_key.clone(), "nonexistentxyz".to_owned(), 10);
        match handle.receiver.recv_timeout(Duration::from_secs(5)).unwrap() {
            SearchResult::Ok { results, .. } => assert!(results.is_empty()),
            SearchResult::Error(e) => panic!("搜索失败: {e}"),
        }
    }
}
