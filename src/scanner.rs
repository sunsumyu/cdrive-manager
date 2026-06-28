use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use crossbeam_channel::{Receiver, Sender, unbounded};
use rayon::prelude::*;
use walkdir::WalkDir;

use crate::model::{
    FileRecord, ScanAccumulator, ScanFilterConfig, ScanStats, file_extension_label,
};

#[derive(Debug, Clone)]
pub enum ScanMode {
    /// First pass: count directories and files only (fast)
    QuickCount,
    /// Second pass: full scan with file sizes and metadata
    FullScan,
    /// Multi-threaded full scan (parallel file processing)
    ParallelFullScan,
    /// NTFS MFT direct reading (Windows only, requires admin)
    MftScan,
}

impl Default for ScanMode {
    fn default() -> Self {
        Self::ParallelFullScan
    }
}

#[derive(Debug, Clone)]
pub struct ScanOptions {
    pub root: PathBuf,
    pub filter_config: ScanFilterConfig,
    pub mode: ScanMode,
    pub num_threads: Option<usize>,
    pub drive_letter: Option<char>,
}

impl ScanOptions {
    pub fn new(root: PathBuf, filter_config: ScanFilterConfig) -> Self {
        Self {
            root,
            filter_config,
            mode: ScanMode::default(),
            num_threads: None,
            drive_letter: None,
        }
    }
    
    pub fn with_mode(mut self, mode: ScanMode) -> Self {
        self.mode = mode;
        self
    }
    
    pub fn with_threads(mut self, num_threads: usize) -> Self {
        self.num_threads = Some(num_threads);
        self
    }
    
    pub fn with_drive_letter(mut self, drive_letter: char) -> Self {
        self.drive_letter = Some(drive_letter);
        self
    }
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
    pub estimated_total_dirs: Option<u64>,
    pub estimated_total_files: Option<u64>,
    pub scan_mode: ScanMode,
    pub active_threads: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct ScanFinished {
    pub stats: Arc<ScanStats>,
    pub cancelled: bool,
    pub scan_mode: ScanMode,
    pub total_dirs: u64,
    pub total_files: u64,
    pub elapsed_time: Duration,
}

const PROGRESS_ENTRY_INTERVAL: u64 = 250;
const PROGRESS_TIME_INTERVAL: Duration = Duration::from_millis(250);

/// Spawn a scan with the given options
pub fn spawn_scan(options: ScanOptions) -> ScanHandle {
    match options.mode {
        ScanMode::QuickCount | ScanMode::FullScan => spawn_single_thread_scan(options),
        ScanMode::ParallelFullScan => spawn_parallel_scan(options),
        ScanMode::MftScan => spawn_mft_scan(options),
    }
}

/// Single-threaded scan (original implementation)
fn spawn_single_thread_scan(options: ScanOptions) -> ScanHandle {
    let (sender, receiver) = unbounded();
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let worker_cancel_flag = Arc::clone(&cancel_flag);
    let scan_mode = options.mode.clone();

    thread::spawn(move || {
        let scan_start = Instant::now();
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
                        if let ScanMode::FullScan = &scan_mode {
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
                        } else {
                            accumulator.record_file_count();
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
                            estimated_total_dirs: None,
                            estimated_total_files: None,
                            scan_mode: scan_mode.clone(),
                            active_threads: Some(1),
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
        let total_dirs = accumulator.get_dir_count();
        let total_files = accumulator.get_file_count();
        let elapsed = scan_start.elapsed();

        let _ = sender.send(ScanEvent::Progress(ScanProgress {
            stats: Arc::new(accumulator.progress_snapshot()),
            current_path: None,
            finished: true,
            cancelled,
            estimated_total_dirs: Some(total_dirs),
            estimated_total_files: Some(total_files),
            scan_mode: scan_mode.clone(),
            active_threads: Some(1),
        }));
        let _ = sender.send(ScanEvent::Finished(ScanFinished {
            stats: final_stats,
            cancelled,
            scan_mode,
            total_dirs,
            total_files,
            elapsed_time: elapsed,
        }));
    });

    ScanHandle {
        receiver,
        cancel_flag,
    }
}

/// Multi-threaded parallel scan using rayon with adaptive batch sizing
fn spawn_parallel_scan(options: ScanOptions) -> ScanHandle {
    let (sender, receiver) = unbounded();
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let worker_cancel_flag = Arc::clone(&cancel_flag);
    let scan_mode = options.mode.clone();
    
    // Dynamic thread count: use 75% of CPU cores by default
    let cpu_count = num_cpus::get();
    let num_threads = options.num_threads.unwrap_or_else(|| {
        // Reserve 1-2 cores for UI responsiveness
        let reserved = if cpu_count <= 4 { 1 } else { 2 };
        cpu_count.saturating_sub(reserved).max(1)
    });

    thread::spawn(move || {
        let scan_start = Instant::now();
        
        // Build custom thread pool for better control
        let pool_result = rayon::ThreadPoolBuilder::new()
            .num_threads(num_threads)
            .thread_name(|i| format!("cdrive-scan-{}", i))
            .build();
        
        // Phase 1: Collect all directory paths (single thread, fast)
        let matcher = ScanFilterMatcher::new(options.root.clone(), options.filter_config.clone());
        let accumulator = Arc::new(std::sync::Mutex::new(
            ScanAccumulator::new_with_filter_config(
                options.root.clone(),
                options.filter_config.clone(),
            )
        ));
        let mut all_dirs: Vec<PathBuf> = Vec::new();
        let mut entries_since_update = 0_u64;
        let mut last_progress_at = Instant::now();

        let walker = WalkDir::new(&options.root)
            .follow_links(false)
            .same_file_system(options.filter_config.same_file_system)
            .into_iter();

        for entry in walker.filter_entry(|entry| matcher.should_descend(entry)) {
            if worker_cancel_flag.load(Ordering::Relaxed) {
                break;
            }

            if let Ok(entry) = entry {
                let path = entry.path().to_path_buf();
                if entry.file_type().is_dir() {
                    all_dirs.push(path.clone());
                    let mut acc = accumulator.lock().unwrap();
                    acc.record_directory(path.clone());
                }

                entries_since_update += 1;
                if entries_since_update >= PROGRESS_ENTRY_INTERVAL
                    && last_progress_at.elapsed() >= PROGRESS_TIME_INTERVAL
                {
                    entries_since_update = 0;
                    last_progress_at = Instant::now();
                    let mut acc = accumulator.lock().unwrap();
                    let _ = sender.send(ScanEvent::Progress(ScanProgress {
                        stats: Arc::new(acc.progress_snapshot()),
                        current_path: Some(entry.path().to_path_buf()),
                        finished: false,
                        cancelled: false,
                        estimated_total_dirs: Some(all_dirs.len() as u64),
                        estimated_total_files: None,
                        scan_mode: scan_mode.clone(),
                        active_threads: Some(1),
                    }));
                }
            }
        }

        if worker_cancel_flag.load(Ordering::Relaxed) {
            let _ = sender.send(ScanEvent::Finished(ScanFinished {
                stats: Arc::new(ScanStats::default()),
                cancelled: true,
                scan_mode: scan_mode.clone(),
                total_dirs: 0,
                total_files: 0,
                elapsed_time: scan_start.elapsed(),
            }));
            return;
        }

        let total_dir_count = all_dirs.len() as u64;
        
        // Phase 2: Adaptive parallel processing
        // Calculate optimal batch size based on workload
        let batch_size = calculate_adaptive_batch_size(total_dir_count, num_threads);
        
        let processed_dirs = Arc::new(AtomicU64::new(0));
        let total_files = Arc::new(AtomicU64::new(0));
        let last_progress_time = Arc::new(std::sync::Mutex::new(Instant::now()));

        // Process directories in adaptive batches
        match &pool_result {
            Ok(pool) => {
                pool.install(|| {
                    process_dirs_parallel(
                        &all_dirs, batch_size, &accumulator, &matcher,
                        &worker_cancel_flag, &processed_dirs, &total_files,
                        &last_progress_time, &sender, total_dir_count,
                        &scan_mode, num_threads
                    );
                });
            }
            Err(_) => {
                // Use global pool
                process_dirs_parallel(
                    &all_dirs, batch_size, &accumulator, &matcher,
                    &worker_cancel_flag, &processed_dirs, &total_files,
                    &last_progress_time, &sender, total_dir_count,
                    &scan_mode, num_threads
                );
            }
        }

        if worker_cancel_flag.load(Ordering::Relaxed) {
            let _ = sender.send(ScanEvent::Finished(ScanFinished {
                stats: Arc::new(accumulator.lock().unwrap().progress_snapshot()),
                cancelled: true,
                scan_mode: scan_mode.clone(),
                total_dirs: processed_dirs.load(Ordering::Relaxed),
                total_files: total_files.load(Ordering::Relaxed),
                elapsed_time: scan_start.elapsed(),
            }));
            return;
        }

        let final_stats = Arc::new(accumulator.lock().unwrap().final_snapshot());
        let final_dirs = processed_dirs.load(Ordering::Relaxed);
        let final_files = total_files.load(Ordering::Relaxed);
        let elapsed = scan_start.elapsed();

        let _ = sender.send(ScanEvent::Progress(ScanProgress {
            stats: Arc::new(accumulator.lock().unwrap().progress_snapshot()),
            current_path: None,
            finished: true,
            cancelled: false,
            estimated_total_dirs: Some(final_dirs),
            estimated_total_files: Some(final_files),
            scan_mode: scan_mode.clone(),
            active_threads: Some(num_threads),
        }));
        let _ = sender.send(ScanEvent::Finished(ScanFinished {
            stats: final_stats,
            cancelled: false,
            scan_mode,
            total_dirs: final_dirs,
            total_files: final_files,
            elapsed_time: elapsed,
        }));
    });

    ScanHandle {
        receiver,
        cancel_flag,
    }
}

/// Calculate adaptive batch size based on workload and thread count
fn calculate_adaptive_batch_size(total_dirs: u64, num_threads: usize) -> usize {
    // Adaptive strategy:
    // - Small workloads (< 100 dirs): batch size 1, immediate processing
    // - Medium workloads (100-1000 dirs): batch size 5-10
    // - Large workloads (1000-10000 dirs): batch size 10-50
    // - Huge workloads (> 10000 dirs): batch size 50-100
    //
    // Smaller batches = better load balancing but more overhead
    // Larger batches = less overhead but potential load imbalance
    
    if total_dirs < 100 {
        1
    } else if total_dirs < 1000 {
        5
    } else if total_dirs < 10000 {
        // Scale with thread count for better parallelism
        (num_threads * 2).max(10).min(50)
    } else {
        // For very large directories, use larger batches to reduce overhead
        (total_dirs as usize / num_threads / 4).max(50).min(200)
    }
}

/// Process directories in parallel with adaptive batching
#[inline]
fn process_dirs_parallel(
    all_dirs: &[PathBuf],
    batch_size: usize,
    accumulator: &Arc<std::sync::Mutex<ScanAccumulator>>,
    matcher: &ScanFilterMatcher,
    cancel_flag: &Arc<AtomicBool>,
    processed_dirs: &Arc<AtomicU64>,
    total_files: &Arc<AtomicU64>,
    last_progress_time: &Arc<std::sync::Mutex<Instant>>,
    sender: &Sender<ScanEvent>,
    total_dir_count: u64,
    scan_mode: &ScanMode,
    num_threads: usize,
) {
    use rayon::prelude::*;
    
    all_dirs.par_chunks(batch_size).for_each(|batch| {
        if cancel_flag.load(Ordering::Relaxed) {
            return;
        }

        for dir_path in batch {
            // Process files in this directory
            if let Ok(read_dir) = std::fs::read_dir(dir_path) {
                for entry in read_dir.flatten() {
                    let path = entry.path();
                    if path.is_file() && !matcher.excludes_file(&path) {
                        if let Ok(metadata) = entry.metadata() {
                            let file_record = FileRecord {
                                extension: file_extension_label(&path),
                                modified: metadata.modified().ok(),
                                path,
                                size: metadata.len(),
                            };
                            let mut acc = accumulator.lock().unwrap();
                            acc.record_file(file_record);
                            total_files.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
            }

            // Update counter
            let _processed = processed_dirs.fetch_add(1, Ordering::Relaxed) + 1;
            
            // Send progress updates (throttled)
            {
                let mut last = last_progress_time.lock().unwrap();
                if last.elapsed() >= PROGRESS_TIME_INTERVAL {
                    *last = Instant::now();
                    let mut acc = accumulator.lock().unwrap();
                    let _ = sender.send(ScanEvent::Progress(ScanProgress {
                        stats: Arc::new(acc.progress_snapshot()),
                        current_path: Some(dir_path.clone()),
                        finished: false,
                        cancelled: false,
                        estimated_total_dirs: Some(total_dir_count),
                        estimated_total_files: None,
                        scan_mode: scan_mode.clone(),
                        active_threads: Some(num_threads),
                    }));
                }
            }
        }
    });
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

/// MFT-based high-speed scan (Windows only)
#[cfg(windows)]
fn spawn_mft_scan(options: ScanOptions) -> ScanHandle {
    let (sender, receiver) = unbounded();
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let worker_cancel_flag = Arc::clone(&cancel_flag);
    
    thread::spawn(move || {
        use crate::mft::windows_mft::{MftScanConfig, scan_mft};
        
        let scan_start = Instant::now();
        
        // Extract drive letter from root path
        let drive_letter = options.root
            .to_str()
            .and_then(|s| s.chars().next())
            .unwrap_or('C');
        
        let config = MftScanConfig {
            drive_letter,
            cancel_flag: Arc::clone(&worker_cancel_flag),
        };
        
        // Try MFT scan first
        match scan_mft(config, sender.clone()) {
            Ok(result) => {
                let mut accumulator = ScanAccumulator::new_with_filter_config(
                    options.root.clone(),
                    options.filter_config.clone(),
                );
                
                for file in result.files {
                    accumulator.record_file(file);
                }
                
                let final_stats = Arc::new(accumulator.final_snapshot());
                let total_dirs = accumulator.get_dir_count() + result.dir_count;
                let total_files = accumulator.get_file_count() + result.file_count;
                
                let _ = sender.send(ScanEvent::Progress(ScanProgress {
                    stats: Arc::new(accumulator.progress_snapshot()),
                    current_path: None,
                    finished: true,
                    cancelled: false,
                    estimated_total_dirs: Some(total_dirs),
                    estimated_total_files: Some(total_files),
                    scan_mode: ScanMode::MftScan,
                    active_threads: Some(1),
                }));
                
                let _ = sender.send(ScanEvent::Finished(ScanFinished {
                    stats: final_stats,
                    cancelled: false,
                    scan_mode: ScanMode::MftScan,
                    total_dirs,
                    total_files,
                    elapsed_time: scan_start.elapsed(),
                }));
            }
            Err(e) => {
                // MFT scan failed, fallback to parallel scan
                eprintln!("MFT scan failed ({}), falling back to parallel scan...", e);
                
                // Notify UI about fallback
                let _ = sender.send(ScanEvent::Progress(ScanProgress {
                    stats: Arc::new(ScanStats::default()),
                    current_path: Some(std::path::PathBuf::from(format!(
                        "MFT 扫描失败: {}，回退到多线程扫描...", e
                    ))),
                    finished: false,
                    cancelled: false,
                    estimated_total_dirs: None,
                    estimated_total_files: None,
                    scan_mode: ScanMode::MftScan,
                    active_threads: Some(1),
                }));
                
                // Run parallel scan as fallback
                run_parallel_scan_fallback(options, sender, worker_cancel_flag, scan_start);
            }
        }
    });
    
    ScanHandle {
        receiver,
        cancel_flag,
    }
}

/// Run parallel scan as fallback when MFT scan fails
#[cfg(windows)]
fn run_parallel_scan_fallback(
    options: ScanOptions,
    sender: Sender<ScanEvent>,
    cancel_flag: Arc<AtomicBool>,
    scan_start: Instant,
) {
    use rayon::prelude::*;
    
    let num_threads = rayon::current_num_threads();
    let matcher = ScanFilterMatcher::new(options.root.clone(), options.filter_config.clone());
    
    // Phase 1: Collect directories
    let mut all_dirs: Vec<PathBuf> = Vec::new();
    let walker = WalkDir::new(&options.root)
        .follow_links(false)
        .same_file_system(options.filter_config.same_file_system)
        .into_iter();
    
    for entry in walker.filter_entry(|e| matcher.should_descend(e)) {
        if cancel_flag.load(Ordering::Relaxed) {
            break;
        }
        if let Ok(entry) = entry {
            if entry.file_type().is_dir() {
                all_dirs.push(entry.path().to_path_buf());
            }
        }
    }
    
    if cancel_flag.load(Ordering::Relaxed) {
        let _ = sender.send(ScanEvent::Finished(ScanFinished {
            stats: Arc::new(ScanStats::default()),
            cancelled: true,
            scan_mode: ScanMode::ParallelFullScan,
            total_dirs: 0,
            total_files: 0,
            elapsed_time: scan_start.elapsed(),
        }));
        return;
    }
    
    // Phase 2: Parallel processing
    let accumulator = Arc::new(std::sync::Mutex::new(
        ScanAccumulator::new_with_filter_config(options.root.clone(), options.filter_config.clone())
    ));
    
    all_dirs.par_iter().for_each(|dir_path| {
        if cancel_flag.load(Ordering::Relaxed) {
            return;
        }
        
        {
            let mut acc = accumulator.lock().unwrap();
            acc.record_directory(dir_path.clone());
        }
        
        if let Ok(read_dir) = std::fs::read_dir(dir_path) {
            for entry in read_dir.flatten() {
                let path = entry.path();
                if path.is_file() && !matcher.excludes_file(&path) {
                    if let Ok(metadata) = entry.metadata() {
                        let mut acc = accumulator.lock().unwrap();
                        acc.record_file(FileRecord {
                            path: path.clone(),
                            size: metadata.len(),
                            modified: metadata.modified().ok(),
                            extension: file_extension_label(&path),
                        });
                    }
                }
            }
        }
    });
    
    // Send final result
    let mut acc = accumulator.lock().unwrap();
    let final_stats = Arc::new(acc.final_snapshot());
    let total_dirs = acc.get_dir_count();
    let total_files = acc.get_file_count();
    
    let _ = sender.send(ScanEvent::Finished(ScanFinished {
        stats: final_stats,
        cancelled: false,
        scan_mode: ScanMode::ParallelFullScan,
        total_dirs,
        total_files,
        elapsed_time: scan_start.elapsed(),
    }));
}

/// MFT scan stub for non-Windows platforms
#[cfg(not(windows))]
fn spawn_mft_scan(options: ScanOptions) -> ScanHandle {
    let (sender, receiver) = unbounded();
    let cancel_flag = Arc::new(AtomicBool::new(false));
    
    thread::spawn(move || {
        let scan_start = Instant::now();
        eprintln!("MFT scanning is only supported on Windows");
        
        let _ = sender.send(ScanEvent::Finished(ScanFinished {
            stats: Arc::new(ScanStats::default()),
            cancelled: true,
            scan_mode: ScanMode::MftScan,
            total_dirs: 0,
            total_files: 0,
            elapsed_time: scan_start.elapsed(),
        }));
    });
    
    ScanHandle {
        receiver,
        cancel_flag,
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
