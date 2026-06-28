use std::{
    cmp::Ordering,
    collections::HashMap,
    path::PathBuf,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

pub const SCAN_STATS_SCHEMA_VERSION: u32 = 4;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScanFilterConfig {
    #[serde(default)]
    pub excluded_directories: Vec<String>,
    #[serde(default)]
    pub excluded_extensions: Vec<String>,
    #[serde(default)]
    pub same_file_system: bool,
}

impl Default for ScanFilterConfig {
    fn default() -> Self {
        Self {
            excluded_directories: Vec::new(),
            excluded_extensions: Vec::new(),
            same_file_system: false,
        }
    }
}

impl ScanFilterConfig {
    pub fn is_active(&self) -> bool {
        !self.excluded_directories.is_empty()
            || !self.excluded_extensions.is_empty()
            || self.same_file_system
    }
}

pub fn normalize_extension_filter(extension: &str) -> Option<String> {
    let extension = extension.trim();
    if extension.is_empty() {
        return None;
    }

    if extension == "无扩展名" || extension == "[无扩展名]" {
        return Some("[无扩展名]".to_owned());
    }

    let extension = extension.trim_start_matches('.').to_ascii_lowercase();
    if extension.is_empty() {
        None
    } else {
        Some(format!(".{extension}"))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileRecord {
    pub path: PathBuf,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub extension: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryRecord {
    pub path: PathBuf,
    pub total_size: u64,
    pub direct_file_count: u64,
    pub direct_file_size: u64,
    pub descendant_file_count: u64,
}

impl DirectoryRecord {
    pub fn name(&self) -> String {
        path_label(&self.path)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryNode {
    pub record: DirectoryRecord,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryTree {
    pub root_index: usize,
    pub nodes: Vec<DirectoryNode>,
    #[serde(skip, default)]
    pub path_index: HashMap<PathBuf, usize>,
}

impl DirectoryTree {
    pub fn rebuild_path_index(&mut self) {
        self.path_index.clear();
        self.path_index.reserve(self.nodes.len());
        for (index, node) in self.nodes.iter().enumerate() {
            self.path_index.insert(node.record.path.clone(), index);
        }
    }

    pub fn node_index_for_path(&self, path: &std::path::Path) -> Option<usize> {
        self.path_index.get(path).copied()
    }
}

impl ScanStats {
    pub fn rebuild_indexes(&mut self) {
        if let Some(tree) = &mut self.directory_tree {
            tree.rebuild_path_index();
        }
    }

    pub fn prepare_for_save(&mut self) {
        self.schema_version = SCAN_STATS_SCHEMA_VERSION;
        self.saved_at_unix_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|duration| duration.as_secs());
        self.app_version = Some(env!("CARGO_PKG_VERSION").to_owned());
    }

    pub fn normalize_cache_metadata_after_load(&mut self) {
        if self.schema_version == 0 {
            self.schema_version = 1;
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionRecord {
    pub extension: String,
    pub total_size: u64,
    pub file_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanStats {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub saved_at_unix_secs: Option<u64>,
    #[serde(default)]
    pub app_version: Option<String>,
    pub root: PathBuf,
    #[serde(default)]
    pub filter_config: ScanFilterConfig,
    pub total_size: u64,
    pub file_count: u64,
    pub dir_count: u64,
    pub error_count: u64,
    pub largest_files: Vec<FileRecord>,
    #[serde(default)]
    pub all_files: Vec<FileRecord>,
    pub largest_dirs: Vec<DirectoryRecord>,
    pub extensions: Vec<ExtensionRecord>,
    pub errors: Vec<String>,
    #[serde(default)]
    pub directory_tree: Option<DirectoryTree>,
    /// Top-level directories (direct children of root) for incremental display
    #[serde(default)]
    pub top_level_dirs: Vec<DirectoryRecord>,
}

impl Default for ScanStats {
    fn default() -> Self {
        Self {
            schema_version: SCAN_STATS_SCHEMA_VERSION,
            saved_at_unix_secs: None,
            app_version: Some(env!("CARGO_PKG_VERSION").to_owned()),
            root: PathBuf::new(),
            filter_config: ScanFilterConfig::default(),
            total_size: 0,
            file_count: 0,
            dir_count: 0,
            error_count: 0,
            largest_files: Vec::new(),
            all_files: Vec::new(),
            largest_dirs: Vec::new(),
            extensions: Vec::new(),
            errors: Vec::new(),
            directory_tree: None,
            top_level_dirs: Vec::new(),
        }
    }
}

fn default_schema_version() -> u32 {
    SCAN_STATS_SCHEMA_VERSION
}

#[derive(Debug, Clone)]
pub struct ScanAccumulator {
    root: PathBuf,
    filter_config: ScanFilterConfig,
    total_size: u64,
    file_count: u64,
    dir_count: u64,
    error_count: u64,
    dir_sizes: HashMap<PathBuf, DirectoryRecord>,
    extension_sizes: HashMap<String, ExtensionRecord>,
    all_files: Vec<FileRecord>,
    largest_files: Vec<FileRecord>,
    errors: Vec<String>,
    /// Cached top-level directories (direct children of root), sorted by size
    cached_top_level_dirs: Option<Vec<DirectoryRecord>>,
    /// Cache invalidation flag
    cache_dirty: bool,
    /// Scan start time for ETA calculation
    scan_started_at: Instant,
}

impl Default for ScanAccumulator {
    fn default() -> Self {
        Self {
            root: PathBuf::new(),
            filter_config: ScanFilterConfig::default(),
            total_size: 0,
            file_count: 0,
            dir_count: 0,
            error_count: 0,
            dir_sizes: HashMap::new(),
            extension_sizes: HashMap::new(),
            all_files: Vec::new(),
            largest_files: Vec::new(),
            errors: Vec::new(),
            cached_top_level_dirs: None,
            cache_dirty: true,
            scan_started_at: Instant::now(),
        }
    }
}

impl ScanAccumulator {
    pub fn new(root: PathBuf) -> Self {
        Self::new_with_filter_config(root, ScanFilterConfig::default())
    }

    pub fn new_with_filter_config(root: PathBuf, filter_config: ScanFilterConfig) -> Self {
        let mut this = Self {
            root: root.clone(),
            filter_config,
            ..Self::default()
        };
        this.record_directory(root);
        this
    }

    pub fn record_directory(&mut self, path: PathBuf) {
        if self.dir_sizes.contains_key(&path) {
            return;
        }

        self.dir_count += 1;
        self.dir_sizes.insert(
            path.clone(),
            DirectoryRecord {
                path,
                total_size: 0,
                direct_file_count: 0,
                direct_file_size: 0,
                descendant_file_count: 0,
            },
        );
        // Invalidate cache when new directory is added
        self.cache_dirty = true;
    }

    pub fn record_file(&mut self, file: FileRecord) {
        self.total_size = self.total_size.saturating_add(file.size);
        self.file_count += 1;
        self.cache_dirty = true; // Invalidate cache when file sizes change

        let extension = self
            .extension_sizes
            .entry(file.extension.clone())
            .or_insert_with(|| ExtensionRecord {
                extension: file.extension.clone(),
                total_size: 0,
                file_count: 0,
            });
        extension.total_size = extension.total_size.saturating_add(file.size);
        extension.file_count += 1;

        if let Some(parent) = file.path.parent() {
            let parent = parent.to_path_buf();
            self.record_directory(parent.clone());
            if let Some(parent_record) = self.dir_sizes.get_mut(&parent) {
                parent_record.direct_file_count += 1;
                parent_record.direct_file_size =
                    parent_record.direct_file_size.saturating_add(file.size);
            }

            for ancestor in parent.ancestors() {
                if !ancestor.starts_with(&self.root) {
                    break;
                }

                let ancestor = ancestor.to_path_buf();
                self.record_directory(ancestor.clone());
                if let Some(dir) = self.dir_sizes.get_mut(&ancestor) {
                    dir.total_size = dir.total_size.saturating_add(file.size);
                    dir.descendant_file_count += 1;
                }
            }
        }

        self.all_files.push(file.clone());
        push_largest(&mut self.largest_files, file, 250, |item| item.size);
    }

    /// Record a file in QuickCount mode (only count, no size metadata)
    pub fn record_file_count(&mut self) {
        self.file_count += 1;
        self.cache_dirty = true;
    }

    pub fn record_error(&mut self, message: String) {
        self.error_count += 1;
        if self.errors.len() < 300 {
            self.errors.push(message);
        }
    }

    /// Get elapsed time since scan started
    pub fn elapsed_time(&self) -> Duration {
        self.scan_started_at.elapsed()
    }

    /// Estimate remaining time based on progress ratio
    pub fn estimated_remaining_time(&self, progress_ratio: f64) -> Option<Duration> {
        if progress_ratio <= 0.0 || progress_ratio >= 1.0 {
            return None;
        }
        let elapsed = self.elapsed_time();
        let total_estimated_secs = elapsed.as_secs_f64() / progress_ratio;
        let total_estimated = Duration::from_secs_f64(total_estimated_secs);
        total_estimated.checked_sub(elapsed)
    }

    /// Get directory count (public accessor)
    pub fn get_dir_count(&self) -> u64 {
        self.dir_count
    }

    /// Get file count (public accessor)
    pub fn get_file_count(&self) -> u64 {
        self.file_count
    }

    pub fn progress_snapshot(&mut self) -> ScanStats {
        // Progress snapshot: skip expensive sorting for real-time display
        // Only compute top_level_dirs (cached) and essential data
        self.snapshot_optimized(false, false)
    }

    pub fn final_snapshot(&mut self) -> ScanStats {
        self.snapshot(true, true)
    }

    /// Get top-level directories (direct children of root) for incremental display.
    /// This is much faster than sorting all directories - O(d) instead of O(n log n)
    /// where d = number of direct children, n = total directories.
    pub fn get_top_level_dirs(&mut self) -> Vec<DirectoryRecord> {
        // Return cached result if still valid
        if !self.cache_dirty {
            if let Some(ref cached) = self.cached_top_level_dirs {
                return cached.clone();
            }
        }

        // Build list of direct children of root
        let mut top_level: Vec<DirectoryRecord> = self
            .dir_sizes
            .values()
            .filter(|dir| {
                // Only include directories that are direct children of root
                dir.path.parent() == Some(&self.root)
            })
            .cloned()
            .collect();

        // Sort by size (descending) then path
        top_level.sort_by(compare_size_then_path_dir);
        
        // Cache the result
        self.cached_top_level_dirs = Some(top_level.clone());
        self.cache_dirty = false;
        
        top_level
    }

    /// Get children of a specific directory path for lazy loading
    pub fn get_children_of(&self, parent_path: &std::path::Path) -> Vec<DirectoryRecord> {
        let mut children: Vec<DirectoryRecord> = self
            .dir_sizes
            .values()
            .filter(|dir| {
                // Only include directories that are direct children
                dir.path.parent() == Some(parent_path)
            })
            .cloned()
            .collect();

        children.sort_by(compare_size_then_path_dir);
        children
    }

    /// Optimized snapshot for progress updates - skips expensive sorting
    fn snapshot_optimized(&mut self, include_tree: bool, include_all_files: bool) -> ScanStats {
        // Use incremental top-level dirs (cached, O(d) where d = direct children)
        let top_level_dirs = self.get_top_level_dirs();

        // Skip sorting all dirs during scanning - only keep recent updates
        // This is O(1) instead of O(n log n)
        let largest_dirs = Vec::new();

        // Skip sorting extensions during scanning
        let extensions = Vec::new();

        let mut largest_files = self.largest_files.clone();
        largest_files.sort_by(compare_size_then_path_file);

        let all_files = if include_all_files {
            let mut all_files = self.all_files.clone();
            all_files.sort_by(compare_path_then_size_file);
            all_files
        } else {
            Vec::new()
        };

        ScanStats {
            schema_version: SCAN_STATS_SCHEMA_VERSION,
            saved_at_unix_secs: None,
            app_version: Some(env!("CARGO_PKG_VERSION").to_owned()),
            root: self.root.clone(),
            filter_config: self.filter_config.clone(),
            total_size: self.total_size,
            file_count: self.file_count,
            dir_count: self.dir_count,
            error_count: self.error_count,
            largest_files,
            all_files,
            largest_dirs,
            extensions,
            errors: self.errors.clone(),
            directory_tree: include_tree.then(|| self.build_directory_tree()),
            top_level_dirs,
        }
    }

    fn snapshot(&mut self, include_tree: bool, include_all_files: bool) -> ScanStats {
        // Use incremental top-level dirs for real-time display (much faster than sorting all)
        let top_level_dirs = self.get_top_level_dirs();

        let mut largest_dirs: Vec<_> = self.dir_sizes.values().cloned().collect();
        largest_dirs.sort_by(compare_size_then_path_dir);
        largest_dirs.truncate(250);

        let mut extensions: Vec<_> = self.extension_sizes.values().cloned().collect();
        extensions.sort_by(compare_size_then_extension);
        extensions.truncate(250);

        let mut largest_files = self.largest_files.clone();
        largest_files.sort_by(compare_size_then_path_file);

        let all_files = if include_all_files {
            let mut all_files = self.all_files.clone();
            all_files.sort_by(compare_path_then_size_file);
            all_files
        } else {
            Vec::new()
        };

        ScanStats {
            schema_version: SCAN_STATS_SCHEMA_VERSION,
            saved_at_unix_secs: None,
            app_version: Some(env!("CARGO_PKG_VERSION").to_owned()),
            root: self.root.clone(),
            filter_config: self.filter_config.clone(),
            total_size: self.total_size,
            file_count: self.file_count,
            dir_count: self.dir_count,
            error_count: self.error_count,
            largest_files,
            all_files,
            largest_dirs,
            extensions,
            errors: self.errors.clone(),
            directory_tree: include_tree.then(|| self.build_directory_tree()),
            top_level_dirs,
        }
    }

    fn build_directory_tree(&self) -> DirectoryTree {
        let mut records: Vec<_> = self.dir_sizes.values().cloned().collect();
        records.sort_by(|left, right| left.path.cmp(&right.path));

        let mut path_index = HashMap::with_capacity(records.len());
        for (index, record) in records.iter().enumerate() {
            path_index.insert(record.path.clone(), index);
        }

        let mut nodes: Vec<_> = records
            .into_iter()
            .map(|record| DirectoryNode {
                record,
                parent: None,
                children: Vec::new(),
            })
            .collect();

        let root_index = path_index.get(&self.root).copied().unwrap_or(0);
        let node_count = nodes.len();
        for index in 0..node_count {
            if index == root_index {
                continue;
            }

            let parent_index = nodes[index]
                .record
                .path
                .parent()
                .and_then(|parent| path_index.get(parent).copied());

            if let Some(parent_index) = parent_index {
                nodes[index].parent = Some(parent_index);
                nodes[parent_index].children.push(index);
            }
        }

        let sort_keys: Vec<_> = nodes
            .iter()
            .map(|node| (node.record.total_size, node.record.path.clone()))
            .collect();
        for node in &mut nodes {
            node.children.sort_by(|left, right| {
                sort_keys[*right]
                    .0
                    .cmp(&sort_keys[*left].0)
                    .then_with(|| sort_keys[*left].1.cmp(&sort_keys[*right].1))
            });
        }

        DirectoryTree {
            root_index,
            nodes,
            path_index,
        }
    }
}

pub fn file_extension_label(path: &std::path::Path) -> String {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| format!(".{}", extension.to_ascii_lowercase()))
        .unwrap_or_else(|| "[无扩展名]".to_owned())
}

pub fn path_label(path: &std::path::Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| path.display().to_string())
}

fn push_largest<T, F>(items: &mut Vec<T>, item: T, limit: usize, size_of: F)
where
    F: Fn(&T) -> u64,
{
    items.push(item);
    items.sort_by(|left, right| size_of(right).cmp(&size_of(left)));
    if items.len() > limit {
        items.truncate(limit);
    }
}

fn compare_size_then_path_file(left: &FileRecord, right: &FileRecord) -> Ordering {
    right
        .size
        .cmp(&left.size)
        .then_with(|| left.path.cmp(&right.path))
}

fn compare_path_then_size_file(left: &FileRecord, right: &FileRecord) -> Ordering {
    left.path
        .cmp(&right.path)
        .then_with(|| right.size.cmp(&left.size))
}

fn compare_size_then_path_dir(left: &DirectoryRecord, right: &DirectoryRecord) -> Ordering {
    right
        .total_size
        .cmp(&left.total_size)
        .then_with(|| left.path.cmp(&right.path))
}

fn compare_size_then_extension(left: &ExtensionRecord, right: &ExtensionRecord) -> Ordering {
    right
        .total_size
        .cmp(&left.total_size)
        .then_with(|| left.extension.cmp(&right.extension))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_tree_preserves_direct_file_sizes() {
        let root = PathBuf::from("C:\\test-root");
        let child = root.join("child");
        let grandchild = child.join("grandchild");
        let mut accumulator = ScanAccumulator::new(root.clone());
        accumulator.record_directory(child.clone());
        accumulator.record_directory(grandchild.clone());
        accumulator.record_file(FileRecord {
            path: root.join("root.bin"),
            size: 10,
            modified: None,
            extension: ".bin".to_owned(),
        });
        accumulator.record_file(FileRecord {
            path: child.join("child.bin"),
            size: 20,
            modified: None,
            extension: ".bin".to_owned(),
        });
        accumulator.record_file(FileRecord {
            path: grandchild.join("grandchild.bin"),
            size: 30,
            modified: None,
            extension: ".bin".to_owned(),
        });

        let stats = accumulator.final_snapshot();
        let tree = stats.directory_tree.as_ref().unwrap();
        assert_directory_size_invariant(tree, tree.root_index);

        let root_node = &tree.nodes[tree.node_index_for_path(&root).unwrap()];
        assert_eq!(root_node.record.direct_file_size, 10);
        assert_eq!(root_node.record.total_size, 60);

        let child_node = &tree.nodes[tree.node_index_for_path(&child).unwrap()];
        assert_eq!(child_node.record.direct_file_size, 20);
        assert_eq!(child_node.record.total_size, 50);

        let grandchild_node = &tree.nodes[tree.node_index_for_path(&grandchild).unwrap()];
        assert_eq!(grandchild_node.record.direct_file_size, 30);
        assert_eq!(grandchild_node.record.total_size, 30);
    }

    #[test]
    fn scan_stats_prepare_for_save_sets_cache_metadata() {
        let mut stats = ScanStats::default();
        stats.schema_version = 0;
        stats.app_version = None;

        stats.prepare_for_save();

        assert_eq!(stats.schema_version, SCAN_STATS_SCHEMA_VERSION);
        assert!(stats.saved_at_unix_secs.is_some());
        assert_eq!(
            stats.app_version.as_deref(),
            Some(env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn final_snapshot_retains_all_files_but_progress_snapshot_does_not() {
        let root = PathBuf::from("C:\\many-files");
        let mut accumulator = ScanAccumulator::new(root.clone());
        for index in 0..300 {
            accumulator.record_file(FileRecord {
                path: root.join(format!("file-{index:03}.bin")),
                size: index,
                modified: None,
                extension: ".bin".to_owned(),
            });
        }

        let progress = accumulator.progress_snapshot();
        assert!(progress.all_files.is_empty());
        assert_eq!(progress.largest_files.len(), 250);

        let final_stats = accumulator.final_snapshot();
        assert_eq!(final_stats.all_files.len(), 300);
        assert_eq!(final_stats.largest_files.len(), 250);
        assert!(
            final_stats
                .all_files
                .iter()
                .any(|file| file.path.ends_with("file-000.bin"))
        );
    }

    #[test]
    fn old_json_without_all_files_loads_with_empty_index() {
        let json = r#"{
            "schema_version": 2,
            "root": "C:\\\\old-cache",
            "total_size": 0,
            "file_count": 0,
            "dir_count": 0,
            "error_count": 0,
            "largest_files": [],
            "largest_dirs": [],
            "extensions": [],
            "errors": []
        }"#;

        let stats: ScanStats = serde_json::from_str(json).unwrap();
        assert!(stats.all_files.is_empty());
    }

    #[test]
    fn filter_config_round_trips_in_snapshots() {
        let root = PathBuf::from("C:\\filtered-root");
        let filter_config = ScanFilterConfig {
            excluded_directories: vec!["target".to_owned()],
            excluded_extensions: vec![".tmp".to_owned()],
            same_file_system: true,
        };
        let accumulator = ScanAccumulator::new_with_filter_config(root, filter_config.clone());

        let stats = accumulator.final_snapshot();

        assert_eq!(stats.filter_config, filter_config);
    }

    #[test]
    fn old_json_without_filter_config_loads_with_default_filter() {
        let json = r#"{
            "schema_version": 3,
            "root": "C:\\\\old-cache",
            "total_size": 0,
            "file_count": 0,
            "dir_count": 0,
            "error_count": 0,
            "largest_files": [],
            "largest_dirs": [],
            "extensions": [],
            "errors": []
        }"#;

        let stats: ScanStats = serde_json::from_str(json).unwrap();
        assert_eq!(stats.filter_config, ScanFilterConfig::default());
    }

    #[test]
    fn extension_filter_normalization_matches_file_labels() {
        assert_eq!(normalize_extension_filter("tmp").as_deref(), Some(".tmp"));
        assert_eq!(normalize_extension_filter(".LOG").as_deref(), Some(".log"));
        assert_eq!(
            normalize_extension_filter("[无扩展名]").as_deref(),
            Some("[无扩展名]")
        );
        assert_eq!(normalize_extension_filter("  "), None);
    }

    fn assert_directory_size_invariant(tree: &DirectoryTree, index: usize) -> u64 {
        let node = &tree.nodes[index];
        let child_total: u64 = node
            .children
            .iter()
            .map(|child| assert_directory_size_invariant(tree, *child))
            .sum();
        assert_eq!(
            node.record.total_size,
            node.record.direct_file_size + child_total,
            "size invariant failed for {}",
            node.record.path.display()
        );
        node.record.total_size
    }
}
