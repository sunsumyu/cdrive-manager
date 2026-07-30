//! Persistent file search index for fast file lookup.
//!
//! Provides Everything-like instant search backed by tantivy full-text engine.

mod schema;
mod frn_db;
mod query;
mod indexer;
mod db;
mod usn_journal;
mod worker;

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
pub use indexer::{SearchIndexer, root_key};
#[allow(unused_imports)]
pub use usn_journal::{spawn_usn_listener, UsnEvent, UsnListenerConfig, UsnListenerHandle};
#[allow(unused_imports)]
pub use worker::{
    spawn_build_index, spawn_search, spawn_usn_index_listener, SearchHandle,
    SearchIndexEvent, SearchIndexHandle, SearchResult,
};
// 兼容旧 API (db.rs)
#[allow(unused_imports)]
pub use db::{
    build_index_from_scan, delete_entry, delete_entry_by_name, index_count, index_exists,
    init_index_tables, search_by_name, FileSearchResult, upsert_entry,
    lookup_frn_path as db_lookup_frn_path, upsert_frn_path as db_upsert_frn_path,
    resolve_path_from_frn as db_resolve_path_from_frn, delete_frn_path as db_delete_frn_path,
};
