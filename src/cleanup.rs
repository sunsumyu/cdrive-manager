use std::{
    cmp::Ordering as CmpOrdering,
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

#[derive(Debug, Clone)]
pub struct CleanupPreviewOptions {
    pub root: PathBuf,
}

#[derive(Debug)]
pub struct CleanupPreviewHandle {
    pub receiver: Receiver<CleanupPreviewEvent>,
    cancel_flag: Arc<AtomicBool>,
}

impl CleanupPreviewHandle {
    pub fn cancel(&self) {
        self.cancel_flag.store(true, Ordering::Relaxed);
    }
}

#[derive(Debug, Clone)]
pub enum CleanupPreviewEvent {
    Progress(CleanupPreviewProgress),
    Finished(CleanupPreviewFinished),
}

#[derive(Debug, Clone)]
pub struct CleanupPreviewProgress {
    pub preview: Arc<CleanupPreview>,
    pub current_path: Option<PathBuf>,
    pub finished: bool,
    pub cancelled: bool,
}

#[derive(Debug, Clone)]
pub struct CleanupPreviewFinished {
    pub preview: Arc<CleanupPreview>,
    pub cancelled: bool,
}

#[derive(Debug, Clone)]
pub struct CleanupPreview {
    pub root: PathBuf,
    pub total_candidate_size: u64,
    pub reclaimable_size: u64,
    pub protected_size: u64,
    pub candidate_count: u64,
    pub protected_count: u64,
    pub error_count: u64,
    pub candidates: Vec<CleanupCandidate>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct CleanupCandidate {
    pub path: PathBuf,
    pub kind: CleanupCandidateKind,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub rule_id: &'static str,
    pub rule_label: &'static str,
    pub reason: String,
    pub risk: CleanupRisk,
    pub protected: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupCandidateKind {
    File,
}

impl CleanupCandidateKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::File => "文件",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupRisk {
    Low,
    Medium,
}

impl CleanupRisk {
    pub fn label(self) -> &'static str {
        match self {
            Self::Low => "低",
            Self::Medium => "中",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
        }
    }

    pub fn rank(self) -> u8 {
        match self {
            Self::Low => 0,
            Self::Medium => 1,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CleanupRule {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub risk: CleanupRisk,
    matcher: CleanupMatcher,
}

#[derive(Debug, Clone)]
enum CleanupMatcher {
    Extension(&'static [&'static str]),
    TempOrCacheDirectory,
}

const RETAINED_CANDIDATE_LIMIT: usize = 500;
const RETAINED_ERROR_LIMIT: usize = 300;
const PROGRESS_ENTRY_INTERVAL: u64 = 250;
const PROGRESS_TIME_INTERVAL: Duration = Duration::from_millis(250);
const TEMP_EXTENSIONS: &[&str] = &[".tmp", ".temp"];
const LOG_DUMP_EXTENSIONS: &[&str] = &[".log", ".dmp", ".etl"];
const BACKUP_EXTENSIONS: &[&str] = &[".bak", ".old"];

pub fn spawn_cleanup_preview(options: CleanupPreviewOptions) -> CleanupPreviewHandle {
    let (sender, receiver) = unbounded();
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let worker_cancel_flag = Arc::clone(&cancel_flag);

    thread::spawn(move || {
        let rules = default_cleanup_rules();
        let mut accumulator = CleanupPreviewAccumulator::new(options.root.clone());
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
                                if let Some(candidate) = cleanup_candidate_for_file(
                                    &options.root,
                                    &path,
                                    metadata.len(),
                                    metadata.modified().ok(),
                                    &rules,
                                ) {
                                    accumulator.record_candidate(candidate);
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
                        let _ =
                            sender.send(CleanupPreviewEvent::Progress(CleanupPreviewProgress {
                                preview: Arc::new(accumulator.snapshot()),
                                current_path: Some(path),
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

        let final_preview = Arc::new(accumulator.snapshot());
        let _ = sender.send(CleanupPreviewEvent::Progress(CleanupPreviewProgress {
            preview: Arc::clone(&final_preview),
            current_path: None,
            finished: true,
            cancelled,
        }));
        let _ = sender.send(CleanupPreviewEvent::Finished(CleanupPreviewFinished {
            preview: final_preview,
            cancelled,
        }));
    });

    CleanupPreviewHandle {
        receiver,
        cancel_flag,
    }
}

pub fn default_cleanup_rules() -> Vec<CleanupRule> {
    vec![
        CleanupRule {
            id: "temp_extension",
            label: "临时文件扩展名",
            description: "扩展名看起来像临时文件的单个文件。",
            risk: CleanupRisk::Low,
            matcher: CleanupMatcher::Extension(TEMP_EXTENSIONS),
        },
        CleanupRule {
            id: "log_dump_extension",
            label: "日志/转储文件",
            description: "常见日志或诊断转储文件。",
            risk: CleanupRisk::Medium,
            matcher: CleanupMatcher::Extension(LOG_DUMP_EXTENSIONS),
        },
        CleanupRule {
            id: "backup_extension",
            label: "备份/旧文件扩展名",
            description: "常见备份或旧版本文件。",
            risk: CleanupRisk::Medium,
            matcher: CleanupMatcher::Extension(BACKUP_EXTENSIONS),
        },
        CleanupRule {
            id: "temp_cache_directory",
            label: "临时/缓存目录中的文件",
            description: "路径位于名称明显为 Temp、Tmp、Cache 或 Caches 的目录下。",
            risk: CleanupRisk::Low,
            matcher: CleanupMatcher::TempOrCacheDirectory,
        },
    ]
}

pub fn is_protected_path(path: &Path) -> bool {
    let normalized = path
        .display()
        .to_string()
        .replace('/', "\\")
        .to_ascii_lowercase();

    if normalized.starts_with("c:\\windows")
        || normalized.starts_with("c:\\program files\\")
        || normalized == "c:\\program files"
        || normalized.starts_with("c:\\program files (x86)\\")
        || normalized == "c:\\program files (x86)"
    {
        return true;
    }

    path.components().any(|component| {
        let text = component.as_os_str().to_string_lossy().to_ascii_lowercase();
        matches!(
            text.as_str(),
            "desktop"
                | "documents"
                | "downloads"
                | "pictures"
                | "music"
                | "videos"
                | "文档"
                | "桌面"
                | "图片"
                | "音乐"
                | "视频"
        )
    })
}

pub fn cleanup_candidate_for_file(
    root: &Path,
    path: &Path,
    size: u64,
    modified: Option<SystemTime>,
    rules: &[CleanupRule],
) -> Option<CleanupCandidate> {
    if !path.starts_with(root) {
        return None;
    }

    rules.iter().find_map(|rule| {
        rule.matches(path).map(|reason| {
            let protected = is_protected_path(path);
            CleanupCandidate {
                path: path.to_path_buf(),
                kind: CleanupCandidateKind::File,
                size,
                modified,
                rule_id: rule.id,
                rule_label: rule.label,
                reason,
                risk: rule.risk,
                protected,
            }
        })
    })
}

impl CleanupRule {
    fn matches(&self, path: &Path) -> Option<String> {
        match &self.matcher {
            CleanupMatcher::Extension(extensions) => {
                let extension = path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .map(|extension| format!(".{}", extension.to_ascii_lowercase()))?;
                extensions.contains(&extension.as_str()).then(|| {
                    format!(
                        "扩展名 {} 命中规则：{}。{}",
                        extension, self.label, self.description
                    )
                })
            }
            CleanupMatcher::TempOrCacheDirectory => path
                .parent()
                .and_then(temp_or_cache_component)
                .map(|component| format!("位于临时/缓存目录：{}。{}", component, self.description)),
        }
    }
}

#[derive(Debug, Clone)]
struct CleanupPreviewAccumulator {
    root: PathBuf,
    total_candidate_size: u64,
    reclaimable_size: u64,
    protected_size: u64,
    candidate_count: u64,
    protected_count: u64,
    error_count: u64,
    candidates: Vec<CleanupCandidate>,
    errors: Vec<String>,
}

impl CleanupPreviewAccumulator {
    fn new(root: PathBuf) -> Self {
        Self {
            root,
            total_candidate_size: 0,
            reclaimable_size: 0,
            protected_size: 0,
            candidate_count: 0,
            protected_count: 0,
            error_count: 0,
            candidates: Vec::new(),
            errors: Vec::new(),
        }
    }

    fn record_candidate(&mut self, candidate: CleanupCandidate) {
        self.total_candidate_size = self.total_candidate_size.saturating_add(candidate.size);
        self.candidate_count += 1;

        if candidate.protected {
            self.protected_size = self.protected_size.saturating_add(candidate.size);
            self.protected_count += 1;
        } else {
            self.reclaimable_size = self.reclaimable_size.saturating_add(candidate.size);
        }

        self.candidates.push(candidate);
        self.candidates.sort_by(compare_candidate_size_then_path);
        if self.candidates.len() > RETAINED_CANDIDATE_LIMIT {
            self.candidates.truncate(RETAINED_CANDIDATE_LIMIT);
        }
    }

    fn record_error(&mut self, message: String) {
        self.error_count += 1;
        if self.errors.len() < RETAINED_ERROR_LIMIT {
            self.errors.push(message);
        }
    }

    fn snapshot(&self) -> CleanupPreview {
        CleanupPreview {
            root: self.root.clone(),
            total_candidate_size: self.total_candidate_size,
            reclaimable_size: self.reclaimable_size,
            protected_size: self.protected_size,
            candidate_count: self.candidate_count,
            protected_count: self.protected_count,
            error_count: self.error_count,
            candidates: self.candidates.clone(),
            errors: self.errors.clone(),
        }
    }
}

fn compare_candidate_size_then_path(
    left: &CleanupCandidate,
    right: &CleanupCandidate,
) -> CmpOrdering {
    right
        .size
        .cmp(&left.size)
        .then_with(|| left.path.cmp(&right.path))
}

fn temp_or_cache_component(path: &Path) -> Option<String> {
    path.components().find_map(|component| {
        let text = component.as_os_str().to_string_lossy();
        let normalized = text.to_ascii_lowercase();
        matches!(normalized.as_str(), "temp" | "tmp" | "cache" | "caches")
            .then(|| text.into_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_extension_rule_matches_temp_files() {
        let rules = default_cleanup_rules();
        let root = PathBuf::from("C:\\work");
        let candidate =
            cleanup_candidate_for_file(&root, &root.join("build").join("x.tmp"), 42, None, &rules)
                .unwrap();

        assert_eq!(candidate.rule_id, "temp_extension");
        assert_eq!(candidate.size, 42);
        assert!(!candidate.protected);
    }

    #[test]
    fn temp_cache_directory_rule_matches_files_inside_cache_dir() {
        let rules = default_cleanup_rules();
        let root = PathBuf::from("C:\\work");
        let candidate = cleanup_candidate_for_file(
            &root,
            &root.join("Cache").join("payload.bin"),
            10,
            None,
            &rules,
        )
        .unwrap();

        assert_eq!(candidate.rule_id, "temp_cache_directory");
    }

    #[test]
    fn protected_candidates_do_not_count_as_reclaimable() {
        let mut accumulator = CleanupPreviewAccumulator::new(PathBuf::from("C:\\"));
        accumulator.record_candidate(CleanupCandidate {
            path: PathBuf::from("C:\\Windows\\Temp\\x.tmp"),
            kind: CleanupCandidateKind::File,
            size: 100,
            modified: None,
            rule_id: "temp_extension",
            rule_label: "临时文件扩展名",
            reason: "test".to_owned(),
            risk: CleanupRisk::Low,
            protected: true,
        });
        accumulator.record_candidate(CleanupCandidate {
            path: PathBuf::from("C:\\Users\\Alice\\AppData\\Local\\Temp\\x.tmp"),
            kind: CleanupCandidateKind::File,
            size: 50,
            modified: None,
            rule_id: "temp_extension",
            rule_label: "临时文件扩展名",
            reason: "test".to_owned(),
            risk: CleanupRisk::Low,
            protected: false,
        });

        let preview = accumulator.snapshot();
        assert_eq!(preview.total_candidate_size, 150);
        assert_eq!(preview.protected_size, 100);
        assert_eq!(preview.reclaimable_size, 50);
        assert_eq!(preview.protected_count, 1);
    }

    #[test]
    fn retained_candidates_are_sorted_by_size() {
        let mut accumulator = CleanupPreviewAccumulator::new(PathBuf::from("C:\\work"));
        accumulator.record_candidate(test_candidate("small.tmp", 1));
        accumulator.record_candidate(test_candidate("large.tmp", 100));
        accumulator.record_candidate(test_candidate("medium.tmp", 50));

        let preview = accumulator.snapshot();
        let sizes: Vec<_> = preview
            .candidates
            .iter()
            .map(|candidate| candidate.size)
            .collect();
        assert_eq!(sizes, vec![100, 50, 1]);
    }

    fn test_candidate(name: &str, size: u64) -> CleanupCandidate {
        CleanupCandidate {
            path: PathBuf::from("C:\\work").join(name),
            kind: CleanupCandidateKind::File,
            size,
            modified: None,
            rule_id: "temp_extension",
            rule_label: "临时文件扩展名",
            reason: "test".to_owned(),
            risk: CleanupRisk::Low,
            protected: false,
        }
    }
}
