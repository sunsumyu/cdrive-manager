use std::{
    cmp::Ordering as CmpOrdering,
    collections::HashMap,
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime},
};

use crossbeam_channel::{Receiver, unbounded};
use walkdir::WalkDir;

use crate::cleanup::is_protected_path;

#[derive(Debug, Clone)]
pub struct DuplicatePreviewOptions {
    pub root: PathBuf,
    pub min_size: u64,
}

#[derive(Debug)]
pub struct DuplicatePreviewHandle {
    pub receiver: Receiver<DuplicatePreviewEvent>,
    cancel_flag: Arc<AtomicBool>,
}

impl DuplicatePreviewHandle {
    pub fn cancel(&self) {
        self.cancel_flag.store(true, Ordering::Relaxed);
    }
}

#[derive(Debug, Clone)]
pub enum DuplicatePreviewEvent {
    Progress(DuplicatePreviewProgress),
    Finished(DuplicatePreviewFinished),
}

#[derive(Debug, Clone)]
pub struct DuplicatePreviewProgress {
    pub preview: Arc<DuplicatePreview>,
    pub current_path: Option<PathBuf>,
    pub phase: DuplicatePreviewPhase,
    pub finished: bool,
    pub cancelled: bool,
}

#[derive(Debug, Clone)]
pub struct DuplicatePreviewFinished {
    pub preview: Arc<DuplicatePreview>,
    pub cancelled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DuplicatePreviewPhase {
    CollectingSizes,
    HashingCandidates,
    Finished,
}

impl DuplicatePreviewPhase {
    pub fn label(self) -> &'static str {
        match self {
            Self::CollectingSizes => "收集同大小候选",
            Self::HashingCandidates => "计算候选哈希",
            Self::Finished => "完成",
        }
    }
}

#[derive(Debug, Clone)]
pub struct DuplicatePreview {
    pub root: PathBuf,
    pub min_size: u64,
    pub scanned_file_count: u64,
    pub hashed_file_count: u64,
    pub duplicate_group_count: u64,
    pub duplicate_file_count: u64,
    pub duplicate_size: u64,
    pub reclaimable_size: u64,
    pub protected_size: u64,
    pub error_count: u64,
    pub groups: Vec<DuplicateGroup>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct DuplicateGroup {
    pub size: u64,
    pub hash: String,
    pub files: Vec<DuplicateFile>,
    pub keep_path: PathBuf,
    pub duplicate_count: u64,
    pub duplicate_size: u64,
    pub reclaimable_size: u64,
    pub protected_size: u64,
}

#[derive(Debug, Clone)]
pub struct DuplicateFile {
    pub path: PathBuf,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub protected: bool,
    pub keep: bool,
}

#[derive(Debug, Clone)]
struct CandidateFile {
    path: PathBuf,
    size: u64,
    modified: Option<SystemTime>,
}

const DEFAULT_MIN_DUPLICATE_SIZE: u64 = 1;
const RETAINED_GROUP_LIMIT: usize = 200;
const RETAINED_ERROR_LIMIT: usize = 300;
const PROGRESS_ENTRY_INTERVAL: u64 = 250;
const PROGRESS_TIME_INTERVAL: Duration = Duration::from_millis(250);

impl Default for DuplicatePreviewOptions {
    fn default() -> Self {
        Self {
            root: PathBuf::new(),
            min_size: DEFAULT_MIN_DUPLICATE_SIZE,
        }
    }
}

pub fn spawn_duplicate_preview(options: DuplicatePreviewOptions) -> DuplicatePreviewHandle {
    let (sender, receiver) = unbounded();
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let worker_cancel_flag = Arc::clone(&cancel_flag);

    thread::spawn(move || {
        let mut accumulator =
            DuplicatePreviewAccumulator::new(options.root.clone(), options.min_size);
        let mut files_by_size: HashMap<u64, Vec<CandidateFile>> = HashMap::new();
        let mut entries_since_update = 0_u64;
        let mut last_progress_at = Instant::now();
        let mut cancelled = false;

        let walker = WalkDir::new(&options.root)
            .follow_links(false)
            .same_file_system(false)
            .into_iter();

        for entry in walker.filter_entry(|entry| !entry.file_type().is_symlink()) {
            if worker_cancel_flag.load(Ordering::Relaxed) {
                cancelled = true;
                break;
            }

            match entry {
                Ok(entry) => {
                    let path = entry.path().to_path_buf();
                    if entry.file_type().is_file() {
                        match entry.metadata() {
                            Ok(metadata) => {
                                accumulator.scanned_file_count += 1;
                                let size = metadata.len();
                                if size >= options.min_size && size > 0 {
                                    files_by_size.entry(size).or_default().push(CandidateFile {
                                        path: path.clone(),
                                        size,
                                        modified: metadata.modified().ok(),
                                    });
                                }
                            }
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
                        let _ = sender.send(DuplicatePreviewEvent::Progress(
                            DuplicatePreviewProgress {
                                preview: Arc::new(accumulator.snapshot()),
                                current_path: Some(path),
                                phase: DuplicatePreviewPhase::CollectingSizes,
                                finished: false,
                                cancelled: false,
                            },
                        ));
                    }
                }
                Err(error) => {
                    accumulator.record_error(error.to_string());
                    entries_since_update += 1;
                }
            }
        }

        if !cancelled {
            let duplicate_size_candidates: Vec<_> = files_by_size
                .into_iter()
                .filter(|(_, files)| files.len() > 1)
                .collect();

            'hashing: for (_size, files) in duplicate_size_candidates {
                let mut files_by_hash: HashMap<String, Vec<CandidateFile>> = HashMap::new();
                for file in files {
                    if worker_cancel_flag.load(Ordering::Relaxed) {
                        cancelled = true;
                        break 'hashing;
                    }

                    match hash_file(&file.path) {
                        Ok(hash) => {
                            accumulator.hashed_file_count += 1;
                            let current_path = file.path.clone();
                            files_by_hash.entry(hash).or_default().push(file);

                            entries_since_update += 1;
                            if entries_since_update >= PROGRESS_ENTRY_INTERVAL
                                && last_progress_at.elapsed() >= PROGRESS_TIME_INTERVAL
                            {
                                entries_since_update = 0;
                                last_progress_at = Instant::now();
                                let _ = sender.send(DuplicatePreviewEvent::Progress(
                                    DuplicatePreviewProgress {
                                        preview: Arc::new(accumulator.snapshot()),
                                        current_path: Some(current_path),
                                        phase: DuplicatePreviewPhase::HashingCandidates,
                                        finished: false,
                                        cancelled: false,
                                    },
                                ));
                            }
                        }
                        Err(error) => {
                            accumulator.record_error(format!(
                                "计算哈希失败：{} ({})",
                                file.path.display(),
                                error
                            ));

                            entries_since_update += 1;
                            if entries_since_update >= PROGRESS_ENTRY_INTERVAL
                                && last_progress_at.elapsed() >= PROGRESS_TIME_INTERVAL
                            {
                                entries_since_update = 0;
                                last_progress_at = Instant::now();
                                let _ = sender.send(DuplicatePreviewEvent::Progress(
                                    DuplicatePreviewProgress {
                                        preview: Arc::new(accumulator.snapshot()),
                                        current_path: Some(file.path.clone()),
                                        phase: DuplicatePreviewPhase::HashingCandidates,
                                        finished: false,
                                        cancelled: false,
                                    },
                                ));
                            }
                        }
                    }
                }

                for (hash, files) in files_by_hash {
                    if files.len() > 1 {
                        accumulator.record_group(duplicate_group_from_candidates(hash, files));
                    }
                }
            }
        }

        let final_preview = Arc::new(accumulator.snapshot());
        let _ = sender.send(DuplicatePreviewEvent::Progress(DuplicatePreviewProgress {
            preview: Arc::clone(&final_preview),
            current_path: None,
            phase: DuplicatePreviewPhase::Finished,
            finished: true,
            cancelled,
        }));
        let _ = sender.send(DuplicatePreviewEvent::Finished(DuplicatePreviewFinished {
            preview: final_preview,
            cancelled,
        }));
    });

    DuplicatePreviewHandle {
        receiver,
        cancel_flag,
    }
}

fn duplicate_group_from_candidates(
    hash: String,
    mut candidates: Vec<CandidateFile>,
) -> DuplicateGroup {
    candidates.sort_by(compare_keep_preference);
    let keep_path = candidates[0].path.clone();
    let size = candidates[0].size;
    let mut duplicate_size = 0_u64;
    let mut reclaimable_size = 0_u64;
    let mut protected_size = 0_u64;
    let mut duplicate_count = 0_u64;

    let files: Vec<_> = candidates
        .into_iter()
        .enumerate()
        .map(|(index, candidate)| {
            let keep = index == 0;
            let protected = is_protected_path(&candidate.path);
            if !keep {
                duplicate_count += 1;
                duplicate_size = duplicate_size.saturating_add(candidate.size);
                if protected {
                    protected_size = protected_size.saturating_add(candidate.size);
                } else {
                    reclaimable_size = reclaimable_size.saturating_add(candidate.size);
                }
            }
            DuplicateFile {
                path: candidate.path,
                size: candidate.size,
                modified: candidate.modified,
                protected,
                keep,
            }
        })
        .collect();

    DuplicateGroup {
        size,
        hash,
        files,
        keep_path,
        duplicate_count,
        duplicate_size,
        reclaimable_size,
        protected_size,
    }
}

fn compare_keep_preference(left: &CandidateFile, right: &CandidateFile) -> CmpOrdering {
    left.modified
        .cmp(&right.modified)
        .then_with(|| left.path.cmp(&right.path))
}

fn hash_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

#[derive(Debug, Clone)]
struct DuplicatePreviewAccumulator {
    root: PathBuf,
    min_size: u64,
    scanned_file_count: u64,
    hashed_file_count: u64,
    duplicate_group_count: u64,
    duplicate_file_count: u64,
    duplicate_size: u64,
    reclaimable_size: u64,
    protected_size: u64,
    error_count: u64,
    groups: Vec<DuplicateGroup>,
    errors: Vec<String>,
}

impl DuplicatePreviewAccumulator {
    fn new(root: PathBuf, min_size: u64) -> Self {
        Self {
            root,
            min_size,
            scanned_file_count: 0,
            hashed_file_count: 0,
            duplicate_group_count: 0,
            duplicate_file_count: 0,
            duplicate_size: 0,
            reclaimable_size: 0,
            protected_size: 0,
            error_count: 0,
            groups: Vec::new(),
            errors: Vec::new(),
        }
    }

    fn record_group(&mut self, group: DuplicateGroup) {
        self.duplicate_group_count += 1;
        self.duplicate_file_count = self
            .duplicate_file_count
            .saturating_add(group.duplicate_count);
        self.duplicate_size = self.duplicate_size.saturating_add(group.duplicate_size);
        self.reclaimable_size = self.reclaimable_size.saturating_add(group.reclaimable_size);
        self.protected_size = self.protected_size.saturating_add(group.protected_size);

        self.groups.push(group);
        self.groups.sort_by(compare_group_reclaimable_then_size);
        if self.groups.len() > RETAINED_GROUP_LIMIT {
            self.groups.truncate(RETAINED_GROUP_LIMIT);
        }
    }

    fn record_error(&mut self, message: String) {
        self.error_count += 1;
        if self.errors.len() < RETAINED_ERROR_LIMIT {
            self.errors.push(message);
        }
    }

    fn snapshot(&self) -> DuplicatePreview {
        DuplicatePreview {
            root: self.root.clone(),
            min_size: self.min_size,
            scanned_file_count: self.scanned_file_count,
            hashed_file_count: self.hashed_file_count,
            duplicate_group_count: self.duplicate_group_count,
            duplicate_file_count: self.duplicate_file_count,
            duplicate_size: self.duplicate_size,
            reclaimable_size: self.reclaimable_size,
            protected_size: self.protected_size,
            error_count: self.error_count,
            groups: self.groups.clone(),
            errors: self.errors.clone(),
        }
    }
}

fn compare_group_reclaimable_then_size(
    left: &DuplicateGroup,
    right: &DuplicateGroup,
) -> CmpOrdering {
    right
        .reclaimable_size
        .cmp(&left.reclaimable_size)
        .then_with(|| right.duplicate_size.cmp(&left.duplicate_size))
        .then_with(|| left.keep_path.cmp(&right.keep_path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{env, fs};

    #[test]
    fn same_content_forms_duplicate_group() {
        let root = test_root("same-content");
        fs::create_dir_all(&root).unwrap();
        let first = root.join("a.bin");
        let second = root.join("b.bin");
        fs::write(&first, b"same").unwrap();
        fs::write(&second, b"same").unwrap();

        let group = duplicate_group_from_candidates(
            hash_file(&first).unwrap(),
            vec![candidate(&first, 4), candidate(&second, 4)],
        );

        assert_eq!(group.duplicate_count, 1);
        assert_eq!(group.reclaimable_size, 4);
        assert_eq!(group.files.iter().filter(|file| file.keep).count(), 1);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn different_content_hashes_differ() {
        let root = test_root("different-content");
        fs::create_dir_all(&root).unwrap();
        let first = root.join("a.bin");
        let second = root.join("b.bin");
        fs::write(&first, b"left").unwrap();
        fs::write(&second, b"rght").unwrap();

        assert_ne!(hash_file(&first).unwrap(), hash_file(&second).unwrap());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn protected_duplicates_are_not_reclaimable() {
        let files = vec![
            candidate(Path::new("C:\\Awork\\keep.bin"), 10),
            candidate(Path::new("C:\\Users\\Alice\\Documents\\duplicate.bin"), 10),
        ];
        let group = duplicate_group_from_candidates("hash".to_owned(), files);

        assert_eq!(group.duplicate_size, 10);
        assert_eq!(group.protected_size, 10);
        assert_eq!(group.reclaimable_size, 0);
    }

    #[test]
    fn keep_selection_is_path_stable() {
        let group = duplicate_group_from_candidates(
            "hash".to_owned(),
            vec![
                candidate(Path::new("C:\\work\\b.bin"), 10),
                candidate(Path::new("C:\\work\\a.bin"), 10),
            ],
        );

        assert!(group.keep_path.ends_with("a.bin"));
    }

    fn candidate(path: &Path, size: u64) -> CandidateFile {
        CandidateFile {
            path: path.to_path_buf(),
            size,
            modified: None,
        }
    }

    fn test_root(name: &str) -> PathBuf {
        env::temp_dir().join(format!(
            "cdrive-manager-duplicates-{name}-{}",
            std::process::id()
        ))
    }
}
