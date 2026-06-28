use std::{
    cmp::Ordering,
    collections::HashMap,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

pub const SCAN_STATS_SCHEMA_VERSION: u32 = 2;

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
    pub total_size: u64,
    pub file_count: u64,
    pub dir_count: u64,
    pub error_count: u64,
    pub largest_files: Vec<FileRecord>,
    pub largest_dirs: Vec<DirectoryRecord>,
    pub extensions: Vec<ExtensionRecord>,
    pub errors: Vec<String>,
    #[serde(default)]
    pub directory_tree: Option<DirectoryTree>,
}

impl Default for ScanStats {
    fn default() -> Self {
        Self {
            schema_version: SCAN_STATS_SCHEMA_VERSION,
            saved_at_unix_secs: None,
            app_version: Some(env!("CARGO_PKG_VERSION").to_owned()),
            root: PathBuf::new(),
            total_size: 0,
            file_count: 0,
            dir_count: 0,
            error_count: 0,
            largest_files: Vec::new(),
            largest_dirs: Vec::new(),
            extensions: Vec::new(),
            errors: Vec::new(),
            directory_tree: None,
        }
    }
}

fn default_schema_version() -> u32 {
    SCAN_STATS_SCHEMA_VERSION
}

#[derive(Debug, Clone, Default)]
pub struct ScanAccumulator {
    root: PathBuf,
    total_size: u64,
    file_count: u64,
    dir_count: u64,
    error_count: u64,
    dir_sizes: HashMap<PathBuf, DirectoryRecord>,
    extension_sizes: HashMap<String, ExtensionRecord>,
    largest_files: Vec<FileRecord>,
    errors: Vec<String>,
}

impl ScanAccumulator {
    pub fn new(root: PathBuf) -> Self {
        let mut this = Self {
            root: root.clone(),
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
    }

    pub fn record_file(&mut self, file: FileRecord) {
        self.total_size = self.total_size.saturating_add(file.size);
        self.file_count += 1;

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

        push_largest(&mut self.largest_files, file, 250, |item| item.size);
    }

    pub fn record_error(&mut self, message: String) {
        self.error_count += 1;
        if self.errors.len() < 300 {
            self.errors.push(message);
        }
    }

    pub fn progress_snapshot(&self) -> ScanStats {
        self.snapshot(false)
    }

    pub fn final_snapshot(&self) -> ScanStats {
        self.snapshot(true)
    }

    fn snapshot(&self, include_tree: bool) -> ScanStats {
        let mut largest_dirs: Vec<_> = self.dir_sizes.values().cloned().collect();
        largest_dirs.sort_by(compare_size_then_path_dir);
        largest_dirs.truncate(250);

        let mut extensions: Vec<_> = self.extension_sizes.values().cloned().collect();
        extensions.sort_by(compare_size_then_extension);
        extensions.truncate(250);

        let mut largest_files = self.largest_files.clone();
        largest_files.sort_by(compare_size_then_path_file);

        ScanStats {
            schema_version: SCAN_STATS_SCHEMA_VERSION,
            saved_at_unix_secs: None,
            app_version: Some(env!("CARGO_PKG_VERSION").to_owned()),
            root: self.root.clone(),
            total_size: self.total_size,
            file_count: self.file_count,
            dir_count: self.dir_count,
            error_count: self.error_count,
            largest_files,
            largest_dirs,
            extensions,
            errors: self.errors.clone(),
            directory_tree: include_tree.then(|| self.build_directory_tree()),
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
