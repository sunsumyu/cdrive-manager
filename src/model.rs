use std::{cmp::Ordering, collections::HashMap, path::PathBuf, time::SystemTime};

use serde::{Deserialize, Serialize};

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
    pub descendant_file_count: u64,
}

impl DirectoryRecord {
    pub fn name(&self) -> String {
        path_label(&self.path)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionRecord {
    pub extension: String,
    pub total_size: u64,
    pub file_count: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScanStats {
    pub root: PathBuf,
    pub total_size: u64,
    pub file_count: u64,
    pub dir_count: u64,
    pub error_count: u64,
    pub largest_files: Vec<FileRecord>,
    pub largest_dirs: Vec<DirectoryRecord>,
    pub extensions: Vec<ExtensionRecord>,
    pub errors: Vec<String>,
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

    pub fn snapshot(&self) -> ScanStats {
        let mut largest_dirs: Vec<_> = self.dir_sizes.values().cloned().collect();
        largest_dirs.sort_by(compare_size_then_path_dir);
        largest_dirs.truncate(250);

        let mut extensions: Vec<_> = self.extension_sizes.values().cloned().collect();
        extensions.sort_by(compare_size_then_extension);
        extensions.truncate(250);

        let mut largest_files = self.largest_files.clone();
        largest_files.sort_by(compare_size_then_path_file);

        ScanStats {
            root: self.root.clone(),
            total_size: self.total_size,
            file_count: self.file_count,
            dir_count: self.dir_count,
            error_count: self.error_count,
            largest_files,
            largest_dirs,
            extensions,
            errors: self.errors.clone(),
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
