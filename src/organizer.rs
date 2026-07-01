//! Personal file organizer for relocating user data folders
//!
//! This module provides safe file transfer from C:\ user folders (Desktop, Documents,
//! Downloads, etc.) to a target data directory on another drive.
//!
//! ## Safety design
//! - Full preview before any file is moved
//! - Copy-then-verify-then-delete semantics (never cut without verification)
//! - Per-file error tracking so partial failures don't block the rest
//! - Source files are preserved if the copy fails
//! - Conflict detection for existing files at the destination
//! - Dry-run by default: nothing is moved until the user explicitly confirms

use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use anyhow::{Context, Result};
use crossbeam_channel::{Receiver, Sender, unbounded};
use serde::{Deserialize, Serialize};

/// Well-known personal data categories that live under the user profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PersonalFolder {
    Desktop,
    Documents,
    Downloads,
    Pictures,
    Videos,
    Music,
    Favorites,
}

impl Default for PersonalFolder {
    fn default() -> Self {
        Self::Desktop
    }
}

impl PersonalFolder {
    pub fn label(self) -> &'static str {
        match self {
            Self::Desktop => "桌面",
            Self::Documents => "文档",
            Self::Downloads => "下载",
            Self::Pictures => "图片",
            Self::Videos => "视频",
            Self::Music => "音乐",
            Self::Favorites => "收藏夹",
        }
    }

    pub fn default_folder_name(self) -> &'static str {
        match self {
            Self::Desktop => "Desktop",
            Self::Documents => "Documents",
            Self::Downloads => "Downloads",
            Self::Pictures => "Pictures",
            Self::Videos => "Videos",
            Self::Music => "Music",
            Self::Favorites => "Favorites",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Self::Desktop => "🖥",
            Self::Documents => "📄",
            Self::Downloads => "📥",
            Self::Pictures => "🖼",
            Self::Videos => "🎬",
            Self::Music => "🎵",
            Self::Favorites => "⭐",
        }
    }

    pub fn default_path(&self) -> Option<PathBuf> {
        match self {
            Self::Desktop => dirs::desktop_dir(),
            Self::Documents => dirs::document_dir(),
            Self::Downloads => dirs::download_dir(),
            Self::Pictures => dirs::picture_dir(),
            Self::Videos => dirs::video_dir(),
            Self::Music => dirs::audio_dir(),
            Self::Favorites => {
                if let Some(home) = dirs::home_dir() {
                    let fav = home.join("Links");
                    if fav.is_dir() {
                        return Some(fav);
                    }
                    // Fallback: Windows stores Favorites as %USERPROFILE%\Favorites
                    let legacy = home.join("Favorites");
                    if legacy.is_dir() {
                        return Some(legacy);
                    }
                }
                None
            }
        }
    }
}

/// How to handle a file that already exists at the destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConflictStrategy {
    /// Skip the file and leave source untouched.
    Skip,
    /// Overwrite the destination with the source file.
    Overwrite,
    /// Rename the new file with a numeric suffix (e.g. `file (1).txt`).
    RenameNew,
    /// Keep the newer file (whichever has the later modification time).
    KeepNewer,
}

impl Default for ConflictStrategy {
    fn default() -> Self {
        Self::Skip
    }
}

impl ConflictStrategy {
    pub fn label(self) -> &'static str {
        match self {
            Self::Skip => "跳过已有文件",
            Self::Overwrite => "覆盖已有文件",
            Self::RenameNew => "重命名新文件",
            Self::KeepNewer => "保留较新文件",
        }
    }
}

/// One candidate item for the organizer: a file inside a personal folder.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrganizerItem {
    pub folder: PersonalFolder,
    pub source_path: PathBuf,
    pub target_path: PathBuf,
    pub size: u64,
    pub modified: Option<std::time::SystemTime>,
    pub is_directory: bool,
    pub conflict: ConflictStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConflictStatus {
    None,
    Exists,
}

impl OrganizerItem {
    pub fn display_source(&self) -> String {
        self.source_path.display().to_string()
    }

    pub fn display_target(&self) -> String {
        self.target_path.display().to_string()
    }
}

/// Aggregated statistics for one personal folder candidate.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FolderCandidateStats {
    pub folder: PersonalFolder,
    pub source_path: PathBuf,
    pub target_path: PathBuf,
    pub total_items: u64,
    pub total_size: u64,
    pub directory_count: u64,
    pub file_count: u64,
    pub conflict_count: u64,
}

/// The preview result the user reviews before committing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrganizerPreview {
    pub target_root: PathBuf,
    pub conflict_strategy: ConflictStrategy,
    pub folders: Vec<FolderCandidateStats>,
    pub items: Vec<OrganizerItem>,
    pub total_items: u64,
    pub total_size: u64,
    pub total_conflicts: u64,
}

impl OrganizerPreview {
    pub fn enabled_folder_count(&self) -> usize {
        self.folders.iter().filter(|f| f.total_items > 0).count()
    }
}

#[derive(Debug, Clone)]
pub enum OrganizerEvent {
    Progress(OrganizerProgress),
    Finished(OrganizerFinished),
}

#[derive(Debug, Clone)]
pub struct OrganizerProgress {
    pub processed_items: u64,
    pub processed_bytes: u64,
    pub total_items: u64,
    pub total_bytes: u64,
    pub current_source: PathBuf,
    pub current_target: PathBuf,
    pub success_count: u64,
    pub skipped_count: u64,
    pub error_count: u64,
    pub finished: bool,
    pub cancelled: bool,
}

#[derive(Debug, Clone)]
pub struct OrganizerFinished {
    pub success_count: u64,
    pub skipped_count: u64,
    pub error_count: u64,
    pub moved_bytes: u64,
    pub errors: Vec<OrganizerError>,
    pub cancelled: bool,
}

#[derive(Debug, Clone)]
pub struct OrganizerError {
    pub source: PathBuf,
    pub target: PathBuf,
    pub message: String,
}

pub struct OrganizerHandle {
    pub receiver: Receiver<OrganizerEvent>,
    cancel_flag: Arc<AtomicBool>,
}

impl OrganizerHandle {
    pub fn cancel(&self) {
        self.cancel_flag.store(true, Ordering::Relaxed);
    }
}

/// Options driving the organizer preview / transfer.
pub struct OrganizerOptions {
    pub target_root: PathBuf,
    pub enabled_folders: Vec<PersonalFolder>,
    pub conflict_strategy: ConflictStrategy,
    /// Maximum number of files collected per folder before truncating the preview.
    pub preview_item_cap: usize,
}

impl Default for OrganizerOptions {
    fn default() -> Self {
        Self {
            target_root: PathBuf::new(),
            enabled_folders: PersonalFolder::default_enabled(),
            conflict_strategy: ConflictStrategy::default(),
            preview_item_cap: 2000,
        }
    }
}

impl PersonalFolder {
    fn default_enabled() -> Vec<Self> {
        vec![
            Self::Desktop,
            Self::Documents,
            Self::Downloads,
            Self::Pictures,
            Self::Videos,
            Self::Music,
        ]
    }
}

/// Build a preview without moving anything.
pub fn build_preview(options: &OrganizerOptions) -> Result<OrganizerPreview> {
    anyhow::ensure!(
        options.target_root.exists(),
        "目标根目录不存在：{}",
        options.target_root.display()
    );
    anyhow::ensure!(
        options.target_root.is_dir(),
        "目标根目录不是目录：{}",
        options.target_root.display()
    );

    let mut preview_folders = Vec::new();
    let mut preview_items = Vec::new();
    let mut total_items: u64 = 0;
    let mut total_size: u64 = 0;
    let mut total_conflicts: u64 = 0;

    for folder in &options.enabled_folders {
        let source = match folder.default_path() {
            Some(p) => p,
            None => continue,
        };
        if !source.exists() {
            continue;
        }
        let target_base = options.target_root.join(folder.default_folder_name());

        let mut folder_stats = FolderCandidateStats {
            folder: *folder,
            source_path: source.clone(),
            target_path: target_base.clone(),
            total_items: 0,
            total_size: 0,
            directory_count: 0,
            file_count: 0,
            conflict_count: 0,
        };

        let walker = walkdir::WalkDir::new(&source)
            .follow_links(false)
            .min_depth(1);

        for entry in walker {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let relative = match entry.path().strip_prefix(&source) {
                Ok(r) => r.to_path_buf(),
                Err(_) => continue,
            };
            let target_path = target_base.join(&relative);

            let metadata = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };

            let is_directory = metadata.is_dir();
            let size = if is_directory { 0 } else { metadata.len() };
            let modified = metadata.modified().ok();
            let conflict = if target_path.exists() {
                ConflictStatus::Exists
            } else {
                ConflictStatus::None
            };

            folder_stats.total_items += 1;
            folder_stats.total_size += size;
            if is_directory {
                folder_stats.directory_count += 1;
            } else {
                folder_stats.file_count += 1;
            }
            if conflict == ConflictStatus::Exists {
                folder_stats.conflict_count += 1;
                total_conflicts += 1;
            }

            total_items += 1;
            total_size += size;

            if preview_items.len() < options.preview_item_cap {
                preview_items.push(OrganizerItem {
                    folder: *folder,
                    source_path: entry.path().to_path_buf(),
                    target_path,
                    size,
                    modified,
                    is_directory,
                    conflict,
                });
            }
        }

        if folder_stats.total_items > 0 {
            preview_folders.push(folder_stats);
        }
    }

    Ok(OrganizerPreview {
        target_root: options.target_root.clone(),
        conflict_strategy: options.conflict_strategy,
        folders: preview_folders,
        items: preview_items,
        total_items,
        total_size,
        total_conflicts,
    })
}

/// Compute the effective target path for an item according to the conflict strategy.
fn resolve_conflict(item: &OrganizerItem, strategy: ConflictStrategy) -> Option<PathBuf> {
    if item.conflict == ConflictStatus::None {
        return Some(item.target_path.clone());
    }

    match strategy {
        ConflictStrategy::Skip => None,
        ConflictStrategy::Overwrite => Some(item.target_path.clone()),
        ConflictStrategy::RenameNew => rename_with_suffix(&item.target_path),
        ConflictStrategy::KeepNewer => {
            let source_newer = item.modified.and_then(|m| {
                let target_meta = std::fs::metadata(&item.target_path).ok()?;
                let target_mod = target_meta.modified().ok()?;
                Some(m > target_mod)
            });
            match source_newer {
                Some(true) => Some(item.target_path.clone()),
                _ => None,
            }
        }
    }
}

fn rename_with_suffix(path: &Path) -> Option<PathBuf> {
    let parent = path.parent()?;
    let stem = path.file_stem()?.to_str()?;
    let ext = path.extension().map(|e| e.to_str()).unwrap_or(None);
    let mut counter = 1u64;
    loop {
        let new_name = if let Some(extension) = ext {
            format!("{} ({}).{}", stem, counter, extension)
        } else {
            format!("{} ({})", stem, counter)
        };
        let candidate = parent.join(&new_name);
        if !candidate.exists() {
            return Some(candidate);
        }
        counter += 1;
        if counter > 10_000 {
            return None;
        }
    }
}

/// Execute the actual file transfer in a background worker.
pub fn spawn_organizer_transfer(preview: Arc<OrganizerPreview>) -> OrganizerHandle {
    let (sender, receiver) = unbounded();
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancel_flag);

    std::thread::spawn(move || {
        run_transfer(&preview, &sender, &worker_cancel);
    });

    OrganizerHandle {
        receiver,
        cancel_flag,
    }
}

fn run_transfer(
    preview: &OrganizerPreview,
    sender: &Sender<OrganizerEvent>,
    cancel_flag: &AtomicBool,
) {
    let mut processed_items = 0u64;
    let mut processed_bytes = 0u64;
    let mut success_count = 0u64;
    let mut skipped_count = 0u64;
    let mut error_count = 0u64;
    let mut moved_bytes = 0u64;
    let mut errors = Vec::new();

    for item in &preview.items {
        if cancel_flag.load(Ordering::Relaxed) {
            break;
        }

        let target = match resolve_conflict(item, preview.conflict_strategy) {
            Some(t) => t,
            None => {
                skipped_count += 1;
                processed_items += 1;
                processed_bytes += item.size;
                emit_progress(
                    sender,
                    processed_items,
                    processed_bytes,
                    preview,
                    &item.source_path,
                    &item.target_path,
                    success_count,
                    skipped_count,
                    error_count,
                    false,
                    false,
                );
                continue;
            }
        };

        emit_progress(
            sender,
            processed_items,
            processed_bytes,
            preview,
            &item.source_path,
            &target,
            success_count,
            skipped_count,
            error_count,
            false,
            false,
        );

        if item.is_directory {
            match std::fs::create_dir_all(&target) {
                Ok(()) => success_count += 1,
                Err(error) => {
                    error_count += 1;
                    errors.push(OrganizerError {
                        source: item.source_path.clone(),
                        target: target.clone(),
                        message: format!("创建目录失败：{}", error),
                    });
                }
            }
            processed_items += 1;
        } else {
            match copy_file_verify(&item.source_path, &target) {
                Ok(bytes_copied) => {
                    // Verify size matches before removing the source.
                    let verify_ok = std::fs::metadata(&target)
                        .ok()
                        .map(|m| m.len() == bytes_copied as u64);

                    if verify_ok == Some(true) {
                        if std::fs::remove_file(&item.source_path).is_ok() {
                            success_count += 1;
                            moved_bytes += bytes_copied;
                        } else {
                            // Copy succeeded but remove failed; count as success but warn.
                            success_count += 1;
                            moved_bytes += bytes_copied;
                            errors.push(OrganizerError {
                                source: item.source_path.clone(),
                                target: target.clone(),
                                message: "文件已复制，但删除源文件失败（文件仍保留在原位）"
                                    .to_owned(),
                            });
                        }
                    } else {
                        error_count += 1;
                        errors.push(OrganizerError {
                            source: item.source_path.clone(),
                            target: target.clone(),
                            message: "复制后大小校验失败，源文件已保留".to_owned(),
                        });
                    }
                }
                Err(error) => {
                    error_count += 1;
                    errors.push(OrganizerError {
                        source: item.source_path.clone(),
                        target: target.clone(),
                        message: format!("{}", error),
                    });
                }
            }
            processed_items += 1;
            processed_bytes += item.size;
        }
    }

    emit_progress(
        sender,
        processed_items,
        processed_bytes,
        preview,
        Path::new(""),
        Path::new(""),
        success_count,
        skipped_count,
        error_count,
        true,
        cancel_flag.load(Ordering::Relaxed),
    );

    let _ = sender.send(OrganizerEvent::Finished(OrganizerFinished {
        success_count,
        skipped_count,
        error_count,
        moved_bytes,
        errors,
        cancelled: cancel_flag.load(Ordering::Relaxed),
    }));
}

fn emit_progress(
    sender: &Sender<OrganizerEvent>,
    processed_items: u64,
    processed_bytes: u64,
    preview: &OrganizerPreview,
    current_source: &Path,
    current_target: &Path,
    success_count: u64,
    skipped_count: u64,
    error_count: u64,
    finished: bool,
    cancelled: bool,
) {
    let _ = sender.send(OrganizerEvent::Progress(OrganizerProgress {
        processed_items,
        processed_bytes,
        total_items: preview.total_items,
        total_bytes: preview.total_size,
        current_source: current_source.to_path_buf(),
        current_target: current_target.to_path_buf(),
        success_count,
        skipped_count,
        error_count,
        finished,
        cancelled,
    }));
}

/// Copy a single file and return the number of bytes copied.
/// Uses a streaming copy so large files do not need to fit in memory.
fn copy_file_verify(source: &Path, target: &Path) -> Result<u64> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("无法创建目标目录 {}", parent.display()))?;
    }

    let mut src_file = std::fs::File::open(source)
        .with_context(|| format!("无法打开源文件 {}", source.display()))?;
    let mut dst_file = std::fs::File::create(target)
        .with_context(|| format!("无法创建目标文件 {}", target.display()))?;

    let bytes_copied = std::io::copy(&mut src_file, &mut dst_file)
        .with_context(|| format!("复制 {} 失败", source.display()))?;

    dst_file
        .sync_all()
        .with_context(|| format!("同步目标文件失败 {}", target.display()))?;

    Ok(bytes_copied)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_with_suffix_avoids_existing() {
        let dir = std::env::temp_dir().join("cdrive_test_rename");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let base = dir.join("notes.txt");
        std::fs::write(&base, "a").unwrap();
        let first = dir.join("notes (1).txt");
        std::fs::write(&first, "b").unwrap();

        let result = rename_with_suffix(&base).unwrap();
        assert_eq!(result, dir.join("notes (2).txt"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_conflict_skip_returns_none_on_existing() {
        let item = OrganizerItem {
            folder: PersonalFolder::Documents,
            source_path: PathBuf::from("C:\\Users\\u\\Documents\\a.txt"),
            target_path: PathBuf::from("D:\\Data\\Documents\\a.txt"),
            size: 10,
            modified: None,
            is_directory: false,
            conflict: ConflictStatus::Exists,
        };
        assert!(resolve_conflict(&item, ConflictStrategy::Skip).is_none());
    }

    #[test]
    fn resolve_conflict_overwrite_returns_target() {
        let item = OrganizerItem {
            folder: PersonalFolder::Documents,
            source_path: PathBuf::from("C:\\Users\\u\\Documents\\a.txt"),
            target_path: PathBuf::from("D:\\Data\\Documents\\a.txt"),
            size: 10,
            modified: None,
            is_directory: false,
            conflict: ConflictStatus::Exists,
        };
        let result = resolve_conflict(&item, ConflictStrategy::Overwrite).unwrap();
        assert_eq!(result, PathBuf::from("D:\\Data\\Documents\\a.txt"));
    }
}
