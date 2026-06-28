use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use crossbeam_channel::{Receiver, unbounded};
use walkdir::WalkDir;

use crate::model::{
    FileRecord, ScanAccumulator, ScanFilterConfig, ScanStats, file_extension_label,
};

#[derive(Debug, Clone)]
pub struct ScanOptions {
    pub root: PathBuf,
    pub filter_config: ScanFilterConfig,
}

#[derive(Debug)]
pub struct ScanHandle {
    pub receiver: Receiver<ScanEvent>,
    cancel_flag: Arc<AtomicBool>,
}

impl ScanHandle {
    pub fn cancel(&self) {
        self.cancel_flag.store(true, Ordering::Relaxed);
    }
}

#[derive(Debug, Clone)]
pub enum ScanEvent {
    Progress(ScanProgress),
    Finished(ScanFinished),
}

#[derive(Debug, Clone)]
pub struct ScanProgress {
    pub stats: Arc<ScanStats>,
    pub current_path: Option<PathBuf>,
    pub finished: bool,
    pub cancelled: bool,
}

#[derive(Debug, Clone)]
pub struct ScanFinished {
    pub stats: Arc<ScanStats>,
    pub cancelled: bool,
}

const PROGRESS_ENTRY_INTERVAL: u64 = 250;
const PROGRESS_TIME_INTERVAL: Duration = Duration::from_millis(250);

pub fn spawn_scan(options: ScanOptions) -> ScanHandle {
    let (sender, receiver) = unbounded();
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let worker_cancel_flag = Arc::clone(&cancel_flag);

    thread::spawn(move || {
        let matcher = ScanFilterMatcher::new(options.root.clone(), options.filter_config.clone());
        let mut accumulator = ScanAccumulator::new_with_filter_config(
            options.root.clone(),
            options.filter_config.clone(),
        );
        let mut entries_since_update = 0_u64;
        let mut last_progress_at = Instant::now();
        let mut cancelled = false;

        let walker = WalkDir::new(&options.root)
            .follow_links(false)
            .same_file_system(options.filter_config.same_file_system)
            .into_iter();

        for entry in walker.filter_entry(|entry| matcher.should_descend(entry)) {
            if worker_cancel_flag.load(Ordering::Relaxed) {
                cancelled = true;
                break;
            }

            match entry {
                Ok(entry) => {
                    let path = entry.path().to_path_buf();
                    let file_type = entry.file_type();

                    if file_type.is_dir() {
                        accumulator.record_directory(path.clone());
                    } else if file_type.is_file() && !matcher.excludes_file(&path) {
                        match entry.metadata() {
                            Ok(metadata) => accumulator.record_file(FileRecord {
                                extension: file_extension_label(&path),
                                modified: metadata.modified().ok(),
                                path,
                                size: metadata.len(),
                            }),
                            Err(error) => accumulator.record_error(format!(
                                "读取元数据失败：{} ({})",
                                entry.path().display(),
                                error
                            )),
                        }
                    }

                    entries_since_update += 1;
                    if entries_since_update >= PROGRESS_ENTRY_INTERVAL
                        && last_progress_at.elapsed() >= PROGRESS_TIME_INTERVAL
                    {
                        entries_since_update = 0;
                        last_progress_at = Instant::now();
                        let _ = sender.send(ScanEvent::Progress(ScanProgress {
                            stats: Arc::new(accumulator.progress_snapshot()),
                            current_path: Some(entry.path().to_path_buf()),
                            finished: false,
                            cancelled: false,
                        }));
                    }
                }
                Err(error) => {
                    accumulator.record_error(error.to_string());
                    entries_since_update += 1;
                }
            }
        }

        let final_stats = Arc::new(accumulator.final_snapshot());
        let _ = sender.send(ScanEvent::Progress(ScanProgress {
            stats: Arc::new(accumulator.progress_snapshot()),
            current_path: None,
            finished: true,
            cancelled,
        }));
        let _ = sender.send(ScanEvent::Finished(ScanFinished {
            stats: final_stats,
            cancelled,
        }));
    });

    ScanHandle {
        receiver,
        cancel_flag,
    }
}

fn is_probably_recursive_link(entry: &walkdir::DirEntry) -> bool {
    entry.file_type().is_symlink()
}

#[derive(Debug, Clone)]
struct ScanFilterMatcher {
    root: PathBuf,
    excluded_directories: HashSet<String>,
    excluded_extensions: HashSet<String>,
}

impl ScanFilterMatcher {
    fn new(root: PathBuf, filter_config: ScanFilterConfig) -> Self {
        Self {
            root,
            excluded_directories: filter_config
                .excluded_directories
                .into_iter()
                .map(|directory| directory.trim().to_ascii_lowercase())
                .filter(|directory| !directory.is_empty())
                .collect(),
            excluded_extensions: filter_config.excluded_extensions.into_iter().collect(),
        }
    }

    fn should_descend(&self, entry: &walkdir::DirEntry) -> bool {
        if is_probably_recursive_link(entry) {
            return false;
        }
        if entry.path() == self.root {
            return true;
        }
        if !entry.file_type().is_dir() {
            return true;
        }
        !self.excludes_directory(entry.path())
    }

    fn excludes_file(&self, path: &Path) -> bool {
        self.excluded_extensions
            .contains(&file_extension_label(path))
    }

    fn excludes_directory(&self, path: &Path) -> bool {
        path.file_name()
            .and_then(|name| name.to_str())
            .map(|name| {
                self.excluded_directories
                    .contains(&name.to_ascii_lowercase())
            })
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::normalize_extension_filter;

    #[test]
    fn scan_filter_matcher_excludes_extensions_by_label() {
        let matcher = ScanFilterMatcher::new(
            PathBuf::from("C:\\root"),
            ScanFilterConfig {
                excluded_directories: Vec::new(),
                excluded_extensions: vec![normalize_extension_filter("tmp").unwrap()],
                same_file_system: false,
            },
        );

        assert!(matcher.excludes_file(Path::new("C:\\root\\a.TMP")));
        assert!(!matcher.excludes_file(Path::new("C:\\root\\a.log")));
    }

    #[test]
    fn scan_filter_matcher_excludes_directory_names_case_insensitively() {
        let matcher = ScanFilterMatcher::new(
            PathBuf::from("C:\\root"),
            ScanFilterConfig {
                excluded_directories: vec!["node_modules".to_owned()],
                excluded_extensions: Vec::new(),
                same_file_system: false,
            },
        );

        assert!(matcher.excludes_directory(Path::new("C:\\root\\Node_Modules")));
        assert!(!matcher.excludes_directory(Path::new("C:\\root\\src")));
    }
}
