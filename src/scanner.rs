use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use crossbeam_channel::{Receiver, unbounded};
use walkdir::WalkDir;

use crate::model::{FileRecord, ScanAccumulator, ScanStats, file_extension_label};

#[derive(Debug, Clone)]
pub struct ScanOptions {
    pub root: PathBuf,
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
        let mut accumulator = ScanAccumulator::new(options.root.clone());
        let mut entries_since_update = 0_u64;
        let mut last_progress_at = Instant::now();
        let mut cancelled = false;

        let walker = WalkDir::new(&options.root)
            .follow_links(false)
            .same_file_system(false)
            .into_iter();

        for entry in walker.filter_entry(|entry| !is_probably_recursive_link(entry)) {
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
                    } else if file_type.is_file() {
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
