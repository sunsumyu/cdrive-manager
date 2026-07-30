//! Persistent file search index for fast file lookup.
//!
//! Provides Everything-like instant search by maintaining a SQLite-backed
//! file index that persists across application restarts.
//!
//! ## Usage
//!
//! ```ignore
//! use cdrive_manager::search_index::{build_index_from_scan, search_by_name, FileSearchResult};
//!
//! // After a scan completes, build the index:
//! // build_index_from_scan(&scan_stats)?;
//!
//! // Later, search the index:
//! let results = search_by_name("c:/", "document", 50)?;
//! ```

mod schema;
mod frn_db;
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
pub use db::{
    build_index_from_scan, delete_entry, delete_entry_by_name, index_count, index_exists,
    init_index_tables, root_key, search_by_name, upsert_entry,
    FileSearchResult,
};
#[allow(unused_imports)]
pub use usn_journal::{spawn_usn_listener, UsnEvent, UsnListenerConfig, UsnListenerHandle};
#[allow(unused_imports)]
pub use worker::{
    spawn_build_index, spawn_search, spawn_usn_index_listener, SearchHandle,
    SearchIndexEvent, SearchIndexHandle, SearchResult,
};
