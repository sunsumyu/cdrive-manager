//! Persistent file search index for fast file lookup.
//!
//! Provides Everything-like instant search backed by tantivy full-text engine.

mod schema;
mod frn_db;
mod query;
mod indexer;
mod usn_journal;
mod worker;

// 测试专用：进程级 LOCALAPPDATA 覆盖是全局的，所有涉及索引目录的测试
// 必须共用同一把锁串行化，避免并行测试互相覆盖环境变量导致索引锁冲突。
#[cfg(test)]
pub(crate) mod test_lock {
    use std::sync::Mutex;
    /// 序列化所有会覆盖 LOCALAPPDATA 的搜索索引测试。
    pub(crate) static INDEX_TEST_LOCK: Mutex<()> = Mutex::new(());
}

#[allow(unused_imports)]
pub use schema::{create_schema, index_directory, frn_db_path, FieldId};
#[allow(unused_imports)]
pub use frn_db::{
    open_frn_db, upsert_frn_path, lookup_frn_path,
    resolve_path_from_frn, delete_frn_path, clear_frn_for_root,
};
#[allow(unused_imports)]
pub use query::{parse_query, parse_size, compile_query, QueryNode, CompareOp, DateValue, QueryParseError};
#[allow(unused_imports)]
pub use indexer::{SearchIndexer, root_key, FileSearchResult};
#[allow(unused_imports)]
pub use usn_journal::{spawn_usn_listener, UsnEvent, UsnListenerConfig, UsnListenerHandle};
#[allow(unused_imports)]
pub use worker::{
    spawn_build_index, spawn_search, spawn_usn_index_listener, SearchHandle,
    SearchIndexEvent, SearchIndexHandle, SearchResult,
};
