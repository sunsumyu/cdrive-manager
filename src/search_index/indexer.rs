//! tantivy 搜索索引核心管理器。
//!
//! 协调 schema、FRN 映射、查询编译, 提供索引构建、搜索、增量更新能力。

use std::path::Path;
use std::sync::{Arc, RwLock};
use anyhow::{Context, Result};
use tantivy::{
    collector::TopDocs, doc, DocAddress,
    directory::MmapDirectory,
    query::{Occur, TermQuery},
    schema::document::TantivyDocument,
    Index, ReloadPolicy, Term,
};

use crate::model::ScanStats;
use crate::search_index::frn_db;
use crate::search_index::query::{compile_query, parse_query};
use crate::search_index::schema::{create_schema, index_directory, FieldId};
use crate::search_index::usn_journal::UsnEvent;

/// 搜索结果(与旧 API 兼容)。
#[derive(Debug, Clone)]
pub struct FileSearchResult {
    pub name: String,
    pub path: String,
    pub parent_path: String,
    pub extension: String,
    pub size: u64,
    pub modified: Option<u64>,
    pub is_directory: bool,
}

/// tantivy 搜索索引管理器。
pub struct SearchIndexer {
    index: Index,
    writer: Arc<RwLock<tantivy::IndexWriter>>,
    reader: tantivy::IndexReader,
    schema: tantivy::schema::Schema,
}

impl SearchIndexer {
    /// 打开或创建索引。
    pub fn open() -> Result<Self> {
        let dir = index_directory().context("获取索引目录失败")?;
        let schema = create_schema();
        let directory = MmapDirectory::open(&dir)
            .context("打开 tantivy MmapDirectory 失败")?;
        let index = Index::open_or_create(directory, schema.clone())
            .context("打开 tantivy 索引失败")?;
        let writer = Arc::new(RwLock::new(
            index.writer(50_000_000).context("创建 IndexWriter 失败")?,
        ));
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()
            .context("创建 IndexReader 失败")?;
        Ok(Self { index: index, writer, reader, schema })
    }

    /// 手动重新加载 reader, 用于测试后立即可见写入结果。
    pub fn reload(&self) -> Result<()> {
        self.reader.reload().context("重新加载 IndexReader 失败")?;
        Ok(())
    }

    /// 从扫描结果批量构建索引, 返回索引条目数。
    pub fn build_from_scan(
        &self,
        stats: &ScanStats,
        root_key: &str,
        progress: impl Fn(u64, u64),
    ) -> Result<u64> {
        {
            let mut writer = self.writer.write().unwrap();
            writer.delete_all_documents()?;
            let conn = frn_db::open_frn_db()?;
            frn_db::clear_frn_for_root(&conn, root_key)?;

            let dir_count = stats.directory_tree.as_ref().map(|t| t.nodes.len()).unwrap_or(0);
            let file_count = stats.all_files.len();
            let total = (dir_count + file_count) as u64;
            let mut processed = 0u64;

            if let Some(tree) = &stats.directory_tree {
                for node in &tree.nodes {
                    let rec = &node.record;
                    let name = rec.path.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    let parent_path = rec.path.parent()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_default();
                    let ext = rec.path.extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("")
                        .to_lowercase();
                    let doc = self.make_document(
                        root_key, &name, &rec.path.display().to_string(),
                        &parent_path, &ext, rec.total_size, None, true, "",
                    );
                    writer.add_document(doc)?;
                    processed += 1;
                    if processed % 1000 == 0 {
                        progress(processed, total);
                    }
                }
            }

            for file in &stats.all_files {
                let name = file.path.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                let parent_path = file.path.parent()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default();
                let modified = file.modified
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs());
                let ext = file.extension.trim_start_matches('.').to_lowercase();
                let doc = self.make_document(
                    root_key, &name, &file.path.display().to_string(),
                    &parent_path, &ext, file.size, modified, false, "",
                );
                writer.add_document(doc)?;
                processed += 1;
                if processed % 1000 == 0 {
                    progress(processed, total);
                }
            }

            writer.commit()?;
        }
        let total = (stats.directory_tree.as_ref().map(|t| t.nodes.len()).unwrap_or(0)
            + stats.all_files.len()) as u64;
        progress(total, total);
        Ok(total)
    }

    /// 执行搜索。
    pub fn search(
        &self,
        root_key: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<FileSearchResult>> {
        let node = parse_query(query).context("解析查询失败")?;
        let compiled = compile_query(&node, &self.schema)?;
        let root_term = Term::from_field_text(FieldId::root_key(&self.schema), root_key);
        let root_query = TermQuery::new(root_term, Default::default());
        let final_query = tantivy::query::BooleanQuery::new(vec![
            (Occur::Must, Box::new(root_query)),
            (Occur::Must, compiled),
        ]);

        let searcher = self.reader.searcher();
        let top_docs = searcher.search(&final_query, &TopDocs::with_limit(limit))?;

        let mut results = Vec::with_capacity(top_docs.len());
        for (_score, addr) in top_docs {
            let doc: TantivyDocument = searcher.doc(addr)?;
            results.push(self.doc_to_result(&doc));
        }
        Ok(results)
    }

    pub fn delete_by_path(&self, path: &str) -> Result<()> {
        // 使用 path_exact (raw 分词) 进行精确匹配删除
        let term = Term::from_field_text(FieldId::path_exact(&self.schema), path);
        let mut writer = self.writer.write().unwrap();
        writer.delete_term(term);
        writer.commit()?;
        Ok(())
    }

    pub fn upsert_entry(
        &self,
        root_key: &str,
        name: &str,
        path: &str,
        parent_path: &str,
        extension: Option<&str>,
        size: u64,
        modified: Option<u64>,
        is_directory: bool,
    ) -> Result<()> {
        let mut writer = self.writer.write().unwrap();
        let term = Term::from_field_text(FieldId::path(&self.schema), path);
        writer.delete_term(term);
        let doc = self.make_document(
            root_key, name, path, parent_path,
            extension.unwrap_or(""), size, modified, is_directory, "",
        );
        writer.add_document(doc)?;
        writer.commit()?;
        Ok(())
    }

    pub fn handle_usn_event(&self, event: UsnEvent, root_key: &str) -> Result<()> {
        let conn = frn_db::open_frn_db()?;
        match event {
            UsnEvent::FileCreated { frn, parent_frn, file_name, is_directory } => {
                let parent_path = frn_db::resolve_path_from_frn(&conn, root_key, &parent_frn, &file_name)?
                    .unwrap_or_else(|| format!("FRN:{}", parent_frn));
                let path = Path::new(&parent_path).join(&file_name)
                    .to_string_lossy().to_string();
                let ext = Path::new(&file_name).extension()
                    .and_then(|e| e.to_str()).unwrap_or("");
                self.upsert_entry(root_key, &file_name, &path, &parent_path,
                    Some(ext), 0, None, is_directory)?;
                frn_db::upsert_frn_path(&conn, root_key, &frn, &path, Some(&parent_frn), is_directory)?;
            }
            UsnEvent::FileDeleted { frn, .. } => {
                if let Some(path) = frn_db::lookup_frn_path(&conn, root_key, &frn)? {
                    self.delete_by_path(&path)?;
                }
                frn_db::delete_frn_path(&conn, root_key, &frn)?;
            }
            UsnEvent::FileModified { frn, parent_frn, file_name } => {
                if let Some(old_path) = frn_db::lookup_frn_path(&conn, root_key, &frn)? {
                    self.delete_by_path(&old_path)?;
                }
                let parent_path = frn_db::resolve_path_from_frn(&conn, root_key, &parent_frn, &file_name)?
                    .unwrap_or_else(|| format!("FRN:{}", parent_frn));
                let path = Path::new(&parent_path).join(&file_name)
                    .to_string_lossy().to_string();
                let ext = Path::new(&file_name).extension()
                    .and_then(|e| e.to_str()).unwrap_or("");
                self.upsert_entry(root_key, &file_name, &path, &parent_path,
                    Some(ext), 0, None, false)?;
                frn_db::upsert_frn_path(&conn, root_key, &frn, &path, Some(&parent_frn), false)?;
            }
            UsnEvent::FileRenamed { old_frn, new_frn, parent_frn, file_name } => {
                if let Some(old_path) = frn_db::lookup_frn_path(&conn, root_key, &old_frn)? {
                    self.delete_by_path(&old_path)?;
                }
                frn_db::delete_frn_path(&conn, root_key, &old_frn)?;
                let parent_path = frn_db::resolve_path_from_frn(&conn, root_key, &parent_frn, &file_name)?
                    .unwrap_or_else(|| format!("FRN:{}", parent_frn));
                let path = Path::new(&parent_path).join(&file_name)
                    .to_string_lossy().to_string();
                let ext = Path::new(&file_name).extension()
                    .and_then(|e| e.to_str()).unwrap_or("");
                self.upsert_entry(root_key, &file_name, &path, &parent_path,
                    Some(ext), 0, None, false)?;
                frn_db::upsert_frn_path(&conn, root_key, &new_frn, &path, Some(&parent_frn), false)?;
            }
        }
        Ok(())
    }

    pub fn index_count(&self, root_key: &str) -> Result<u64> {
        let root_term = Term::from_field_text(FieldId::root_key(&self.schema), root_key);
        let query = Box::new(TermQuery::new(root_term, Default::default()));
        let searcher = self.reader.searcher();
        // 通过 TopDocs 查询最大数来获取计数
        let max_limit = 100000;
        let top_docs = searcher.search(&*query, &TopDocs::with_limit(max_limit))?;
        Ok(top_docs.len() as u64)
    }

    pub fn index_exists(&self, root_key: &str) -> Result<bool> {
        Ok(self.index_count(root_key)? > 0)
    }

    fn make_document(
        &self,
        root_key: &str,
        name: &str,
        path: &str,
        parent_path: &str,
        extension: &str,
        size: u64,
        modified: Option<u64>,
        is_directory: bool,
        frn: &str,
    ) -> TantivyDocument {
        let modified_days = modified.map(|m| m / 86400).unwrap_or(0);
        doc!(
            FieldId::name(&self.schema) => name,
            FieldId::path(&self.schema) => path,
            FieldId::path_exact(&self.schema) => path,
            FieldId::parent_path(&self.schema) => parent_path,
            FieldId::extension(&self.schema) => extension,
            FieldId::size(&self.schema) => size,
            FieldId::modified(&self.schema) => modified.unwrap_or(0),
            FieldId::modified_days(&self.schema) => modified_days,
            FieldId::is_directory(&self.schema) => is_directory,
            FieldId::root_key(&self.schema) => root_key,
            FieldId::frn(&self.schema) => frn,
        )
    }

    fn doc_to_result(&self, doc: &TantivyDocument) -> FileSearchResult {
        use tantivy::schema::OwnedValue;
        let get_str = |f| {
            doc.get_first(f)
                .and_then(|v| match v {
                    OwnedValue::Str(s) => Some(s.as_str()),
                    OwnedValue::PreTokStr(p) => Some(p.text.as_str()),
                    _ => None,
                })
                .unwrap_or("")
                .to_owned()
        };
        let get_u64 = |f| {
            doc.get_first(f)
                .and_then(|v| match v {
                    OwnedValue::U64(v) => Some(*v),
                    OwnedValue::I64(v) => Some(*v as u64),
                    _ => None,
                })
                .unwrap_or(0)
        };
        let get_bool =
            |f| doc.get_first(f).and_then(|v| match v {
                OwnedValue::Bool(v) => Some(*v),
                _ => None,
            }).unwrap_or(false);
        let modified = doc
            .get_first(FieldId::modified(&self.schema))
            .and_then(|v| match v {
                OwnedValue::U64(v) => Some(*v),
                _ => None,
            })
            .filter(|&v| v > 0);
        FileSearchResult {
            name: get_str(FieldId::name(&self.schema)),
            path: get_str(FieldId::path(&self.schema)),
            parent_path: get_str(FieldId::parent_path(&self.schema)),
            extension: get_str(FieldId::extension(&self.schema)),
            size: get_u64(FieldId::size(&self.schema)),
            modified,
            is_directory: get_bool(FieldId::is_directory(&self.schema)),
        }
    }
}

/// 将路径规范化为 root key (小写, 正斜杠)。
pub fn root_key(root: &Path) -> String {
    let canonical = std::fs::canonicalize(root)
        .unwrap_or_else(|_| root.to_path_buf());
    canonical.to_string_lossy().to_lowercase().replace("\\", "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{FileRecord, ScanStats};
    use std::ffi::OsString;
    use std::path::PathBuf;

    fn setup_temp_index() -> SearchIndexer {
        let cdrive_dir = std::env::temp_dir().join(format!(
            "cdrive-manager-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let parent = cdrive_dir.parent().unwrap().to_path_buf();
        std::fs::create_dir_all(&parent).unwrap();
        // 通过覆盖 LOCALAPPDATA 让索引写到临时目录, 避免并行测试锁冲突
        let _guard = LocalAppdataGuard::override_with(&cdrive_dir);
        SearchIndexer::open().expect("应能打开索引")
    }

    // 在测试期间临时覆盖 LOCALAPPDATA 的环境变量。
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

    fn make_test_stats() -> ScanStats {
        let mut stats = ScanStats::default();
        stats.root = PathBuf::from("C:\\test");
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
        ];
        stats.file_count = 2;
        stats
    }

    #[test]
    fn build_from_scan_indexes_files() {
        let indexer = setup_temp_index();
        let stats = make_test_stats();
        let count = indexer.build_from_scan(&stats, "c:/test", |_, _| {}).unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn search_by_keyword_returns_matches() {
        let indexer = setup_temp_index();
        let stats = make_test_stats();
        indexer.build_from_scan(&stats, "c:/test", |_, _| {}).unwrap();
        indexer.reload().unwrap();
        let results = indexer.search("c:/test", "report", 10).unwrap();
        assert!(!results.is_empty());
    }

    #[test]
    fn search_by_extension_returns_matches() {
        let indexer = setup_temp_index();
        let stats = make_test_stats();
        indexer.build_from_scan(&stats, "c:/test", |_, _| {}).unwrap();
        indexer.reload().unwrap();
        let results = indexer.search("c:/test", "ext:pdf", 10).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn search_by_size_range() {
        let indexer = setup_temp_index();
        let stats = make_test_stats();
        indexer.build_from_scan(&stats, "c:/test", |_, _| {}).unwrap();
        indexer.reload().unwrap();
        let results = indexer.search("c:/test", "size:>500KB", 10).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn search_by_regex() {
        let indexer = setup_temp_index();
        let stats = make_test_stats();
        indexer.build_from_scan(&stats, "c:/test", |_, _| {}).unwrap();
        indexer.reload().unwrap();
        let results = indexer.search("c:/test", "regex:report", 10).unwrap();
        assert!(!results.is_empty());
    }

    #[test]
    fn delete_by_path_removes_document() {
        let indexer = setup_temp_index();
        let stats = make_test_stats();
        indexer.build_from_scan(&stats, "c:/test", |_, _| {}).unwrap();
        indexer.reload().unwrap();
        indexer.delete_by_path("C:\\test\\report.pdf").unwrap();
        indexer.reload().unwrap();
        let results = indexer.search("c:/test", "report", 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn upsert_entry_adds_new_document() {
        let indexer = setup_temp_index();
        indexer.upsert_entry(
            "c:/test", "new.txt", "C:\\test\\new.txt",
            "C:\\test", Some("txt"), 100, None, false,
        ).unwrap();
        indexer.reload().unwrap();
        let results = indexer.search("c:/test", "new", 10).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn index_count_returns_correct_number() {
        let indexer = setup_temp_index();
        let stats = make_test_stats();
        indexer.build_from_scan(&stats, "c:/test", |_, _| {}).unwrap();
        indexer.reload().unwrap();
        let count = indexer.index_count("c:/test").unwrap();
        assert_eq!(count, 2);
    }
}
