//! Search index database operations
//!
//! Provides persistent file indexing using SQLite, enabling fast file search
//! similar to Everything. Uses FTS5 for full-text search and supports
//! incremental updates via USN Journal events.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};

use crate::model::ScanStats;
use crate::model::FileRecord;

/// Default path to the SQLite cache database.
fn default_cache_db_path() -> anyhow::Result<PathBuf> {
    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
        return Ok(PathBuf::from(local_app_data)
            .join("cdrive-manager")
            .join("scan-cache.sqlite3"));
    }
    Ok(std::env::current_dir()?.join(".cdrive-manager-cache.sqlite3"))
}

/// A single file search result
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

/// Initialize search index tables in the SQLite database.
///
/// Creates the canonical `file_index` table along with an FTS5 external-content
/// full-text index (`file_index_fts`) kept in sync via triggers. The FTS5 table
/// is what powers `search_by_name`; the B-tree indexes back path/parent/ext
/// lookups used during incremental updates.
pub fn init_index_tables(connection: &Connection) -> rusqlite::Result<()> {
    connection.execute_batch(
        "-- File index table for fast search
        CREATE TABLE IF NOT EXISTS file_index (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            root_key TEXT NOT NULL,
            name TEXT NOT NULL,
            path TEXT NOT NULL UNIQUE,
            parent_path TEXT NOT NULL,
            extension TEXT,
            size INTEGER NOT NULL DEFAULT 0,
            modified INTEGER,
            is_directory INTEGER NOT NULL DEFAULT 0,
            indexed_at INTEGER NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_file_name ON file_index(root_key, name);
        CREATE INDEX IF NOT EXISTS idx_file_path ON file_index(root_key, path);
        CREATE INDEX IF NOT EXISTS idx_file_parent ON file_index(root_key, parent_path);
        CREATE INDEX IF NOT EXISTS idx_file_ext ON file_index(root_key, extension);

        -- Metadata table for index state tracking
        CREATE TABLE IF NOT EXISTS index_metadata (
            root_key TEXT PRIMARY KEY,
            total_files INTEGER NOT NULL DEFAULT 0,
            total_dirs INTEGER NOT NULL DEFAULT 0,
            indexed_at INTEGER NOT NULL,
            last_update INTEGER NOT NULL
        );

        -- FRN to path mapping table for USN Journal path resolution
        CREATE TABLE IF NOT EXISTS frn_path_map (
            root_key TEXT NOT NULL,
            frn TEXT NOT NULL,
            path TEXT NOT NULL,
            parent_frn TEXT,
            is_directory INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (root_key, frn)
        );

        CREATE INDEX IF NOT EXISTS idx_frn_root ON frn_path_map(root_key, frn);
        CREATE INDEX IF NOT EXISTS idx_frn_parent ON frn_path_map(root_key, parent_frn);",
    )?;

    // FTS5 external-content full-text index over file_index.
    // `content='file_index'` + `content_rowid='id'` keeps the FTS table as a
    // thin index that mirrors file_index rows; triggers below keep it in sync
    // for INSERT/UPDATE/DELETE, so all write paths (bulk rebuild, USN upsert,
    // delete) maintain the index transparently.
    connection.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS file_index_fts USING fts5(
            name,
            path,
            content='file_index',
            content_rowid='id',
            tokenize='unicode61 remove_diacritics 2'
        );

        -- Sync triggers: keep file_index_fts aligned with file_index.
        CREATE TRIGGER IF NOT EXISTS file_index_ai AFTER INSERT ON file_index BEGIN
            INSERT INTO file_index_fts(rowid, name, path)
            VALUES (new.id, new.name, new.path);
        END;

        CREATE TRIGGER IF NOT EXISTS file_index_ad AFTER DELETE ON file_index BEGIN
            INSERT INTO file_index_fts(file_index_fts, rowid, name, path)
            VALUES ('delete', old.id, old.name, old.path);
        END;

        CREATE TRIGGER IF NOT EXISTS file_index_au AFTER UPDATE ON file_index BEGIN
            INSERT INTO file_index_fts(file_index_fts, rowid, name, path)
            VALUES ('delete', old.id, old.name, old.path);
            INSERT INTO file_index_fts(rowid, name, path)
            VALUES (new.id, new.name, new.path);
        END;",
    )?;

    // Backfill safety net: if the FTS5 table exists but is empty while
    // file_index has rows (e.g. upgrading from a pre-FTS5 DB), rebuild the
    // index from the external content table in one pass.
    let fts_count: i64 = connection.query_row(
        "SELECT count(*) FROM file_index_fts",
        [],
        |row| row.get(0),
    )?;
    let base_count: i64 = connection.query_row(
        "SELECT count(*) FROM file_index",
        [],
        |row| row.get(0),
    )?;
    if fts_count == 0 && base_count > 0 {
        connection.execute("INSERT INTO file_index_fts(file_index_fts) VALUES('rebuild')", [])?;
    }

    Ok(())
}

/// Apply SQLite pragmas that maximize read/scan throughput and batch write
/// speed. WAL mode allows concurrent readers during writes; `synchronous=NORMAL`
/// is safe under WAL and avoids an fsync per commit; `mmap_size` lets SQLite
/// memory-map the DB file; a larger `cache_size` keeps hot pages in RAM.
pub fn apply_pragmas(connection: &Connection) -> rusqlite::Result<()> {
    connection.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA temp_store = MEMORY;
         PRAGMA mmap_size = 268435456;
         PRAGMA cache_size = -65536;
         PRAGMA foreign_keys = ON;",
    )?;
    Ok(())
}

/// Open the search index database, initializing tables and applying pragmas.
/// All public functions should go through this instead of `Connection::open` to
/// guarantee a consistent schema and tuned runtime.
pub fn open_db() -> rusqlite::Result<Connection> {
    let db_path = default_cache_db_path()
        .unwrap_or_else(|_| PathBuf::from(".cdrive-manager-cache.sqlite3"));
    let conn = Connection::open(&db_path)?;
    init_index_tables(&conn)?;
    apply_pragmas(&conn)?;
    Ok(conn)
}

/// Normalize a path into a root key (stable identifier)
pub fn root_key(root: &Path) -> String {
    let canonical = std::fs::canonicalize(root)
        .unwrap_or_else(|_| root.to_path_buf());
    canonical.to_string_lossy().to_lowercase().replace("\\", "/")
}

/// Build the search index from a completed scan's stats.
/// Clears any existing index for this root and rebuilds it.
///
/// Runs the entire rebuild inside a transaction with prepared statements, and
/// commits in batches of `BATCH_SIZE` rows to bound memory while still avoiding
/// a per-row fsync. The FTS5 table is auto-maintained by triggers during the
/// inserts; a final `'rebuild'` is issued as a consistency safety net.
pub fn build_index_from_scan(stats: &ScanStats) -> rusqlite::Result<usize> {
    const BATCH_SIZE: usize = 10_000;

    let conn = open_db()?;
    let key = root_key(&stats.root);

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    // Wipe this root's rows. Triggers propagate deletes to file_index_fts.
    conn.execute("BEGIN", [])?;
    conn.execute("DELETE FROM file_index WHERE root_key = ?1", params![&key])?;
    conn.execute("DELETE FROM index_metadata WHERE root_key = ?1", params![&key])?;

    let mut file_count = 0u64;
    let mut dir_count = 0u64;

    // Prepared statement shared across all inserts (dirs, files, largest_files).
    let mut stmt = conn.prepare(
        "INSERT INTO file_index (root_key, name, path, parent_path, extension, size, modified, is_directory, indexed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(path) DO UPDATE SET
            name = excluded.name,
            parent_path = excluded.parent_path,
            extension = excluded.extension,
            size = excluded.size,
            modified = excluded.modified,
            is_directory = excluded.is_directory,
            indexed_at = excluded.indexed_at",
    )?;

    // Helper: insert one row and commit/reopen transaction every BATCH_SIZE rows.
    let mut batch_progress: usize = 0;
    let flush_if_needed = |batch_progress: &mut usize| -> rusqlite::Result<()> {
        if *batch_progress >= BATCH_SIZE {
            conn.execute("COMMIT", [])?;
            conn.execute("BEGIN", [])?;
            *batch_progress = 0;
        }
        Ok(())
    };

    // Directory tree nodes
    if let Some(tree) = &stats.directory_tree {
        for node in &tree.nodes {
            let record = &node.record;
            let name = record.path.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let parent_path = record.path.parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            let ext = record.path.extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_lowercase();

            stmt.execute(params![
                &key, name, record.path.display().to_string(),
                parent_path, ext, record.total_size as i64,
                // Directories don't track mtime in the scan record.
                rusqlite::types::Null,
                1i32, now
            ])?;
            dir_count += 1;
            batch_progress += 1;
            flush_if_needed(&mut batch_progress)?;
        }
    }

    // Files (primary source). Falls back to largest_files when all_files empty.
    let file_source: &[FileRecord] = if !stats.all_files.is_empty() {
        &stats.all_files
    } else {
        &stats.largest_files
    };
    for file in file_source {
        let name = file.path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let parent_path = file.path.parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let modified = file.modified
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64);

        stmt.execute(params![
            &key, name, file.path.display().to_string(),
            parent_path, file.extension.clone(), file.size as i64,
            modified, 0i32, now
        ])?;
        file_count += 1;
        batch_progress += 1;
        flush_if_needed(&mut batch_progress)?;
    }

    drop(stmt);

    // Update metadata
    conn.execute(
        "INSERT INTO index_metadata (root_key, total_files, total_dirs, indexed_at, last_update)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(root_key) DO UPDATE SET
            total_files = excluded.total_files,
            total_dirs = excluded.total_dirs,
            indexed_at = excluded.indexed_at,
            last_update = excluded.last_update",
        params![
            &key,
            file_count as i64,
            dir_count as i64,
            now,
            now,
        ],
    )?;

    conn.execute("COMMIT", [])?;

    // FTS5 safety net: rebuild from the external content table so the index is
    // guaranteed consistent even if triggers missed a path during the bulk load.
    conn.execute("INSERT INTO file_index_fts(file_index_fts) VALUES('rebuild')", [])?;

    let total = file_count + dir_count;
    Ok(total as usize)
}

/// Build an FTS5 MATCH expression from a raw query string.
///
/// The query is split on non-alphanumeric boundaries; each token becomes a
/// prefix match (`name : token*`). Tokens are AND-ed together. `name :`
/// scopes the match to the name column, which is what users overwhelmingly
/// search by. Returns `None` if no usable token survives (caller falls back to
/// a `1=0` guard so the query returns nothing instead of erroring).
fn build_fts_match_expr(query: &str) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    for raw in query.split(|c: char| !c.is_alphanumeric()) {
        let token = raw.trim();
        if token.is_empty() {
            continue;
        }
        // FTS5 quoting: double single-quotes inside a quoted string.
        let quoted = token.replace('\'', "''");
        parts.push(format!("name : '\"{}\"*'", quoted));
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join(" AND "))
}

/// Search files by name using the FTS5 full-text index.
///
/// Strategy: FTS5 `MATCH` narrows candidates via the inverted index in
/// sub-millisecond even over millions of rows; a final `LIKE '%query%'`
/// filter on name/path restores the original substring-contain semantics that
/// FTS5 token-based matching alone wouldn't fully cover (e.g. matches in the
/// middle of a token).
///
/// Returns up to `limit` results, ordered by name-exact-match priority then
/// size descending — preserving the previous ordering contract.
pub fn search_by_name(root_key: &str, query: &str, limit: usize) -> rusqlite::Result<Vec<FileSearchResult>> {
    let conn = open_db()?;

    let trimmed = query.trim();
    if trimmed.is_empty() {
        // 空查询：返回该 root 下的全部文件，按大小降序。
        static ALL_SQL: &str = "\
            SELECT name, path, parent_path, extension, size, modified, is_directory
            FROM file_index
            WHERE root_key = ?1
            ORDER BY is_directory ASC, size DESC
            LIMIT ?2";
        let mut stmt = conn.prepare(ALL_SQL)?;
        let rows = stmt.query_map(
            params![root_key, limit as i64],
            |row| {
                Ok(FileSearchResult {
                    name: row.get(0)?,
                    path: row.get(1)?,
                    parent_path: row.get(2)?,
                    extension: row.get(3)?,
                    size: row.get::<usize, i64>(4).unwrap_or(0) as u64,
                    modified: row.get::<usize, Option<i64>>(5)?.map(|v| v as u64),
                    is_directory: row.get::<usize, i32>(6).unwrap_or(0) != 0,
                })
            },
        )?;

        let mut results = Vec::new();
        for result in rows {
            results.push(result?);
        }
        return Ok(results);
    }

    let like_pattern = format!("%{}%", trimmed.replace('%', "\\%").replace('_', "\\_"));
    let match_expr = build_fts_match_expr(trimmed);

    // If FTS5 can't produce a valid expression (e.g. query is all punctuation like ".mp4"),
    // fall back to a pure LIKE query so the search still works.
    let use_fts = match_expr.is_some();
    let match_expr_str = match_expr.unwrap_or_else(|| "1=0".to_string());

    // Use a generous inner LIMIT so the LIKE post-filter has room to discard
    // FTS hits that don't truly contain the raw query substring, while still
    // bounding work. Outer LIMIT is the user-requested cap.
    let inner_limit = limit.saturating_mul(4).max(limit);

    let mut results = Vec::new();

    if use_fts {
        static SQL: &str = "\
            SELECT name, path, parent_path, extension, size, modified, is_directory
            FROM (
                SELECT r.name, r.path, r.parent_path, r.extension, r.size, r.modified, r.is_directory
                FROM file_index_fts f
                JOIN file_index r ON r.id = f.rowid
                WHERE r.root_key = ?1
                  AND file_index_fts MATCH ?2
                  AND (r.name LIKE ?3 ESCAPE '\\' OR r.path LIKE ?3 ESCAPE '\\')
                ORDER BY
                    CASE WHEN r.name LIKE ?3 ESCAPE '\\' THEN 0 ELSE 1 END,
                    r.size DESC
                LIMIT ?4
            )
            ORDER BY
                CASE WHEN name LIKE ?3 ESCAPE '\\' THEN 0 ELSE 1 END,
                size DESC
            LIMIT ?5";

        let mut stmt = conn.prepare(SQL)?;
        let rows = stmt.query_map(
            params![
                root_key,
                &match_expr_str,
                &like_pattern,
                inner_limit as i64,
                limit as i64,
            ],
            |row| {
                Ok(FileSearchResult {
                    name: row.get(0)?,
                    path: row.get(1)?,
                    parent_path: row.get(2)?,
                    extension: row.get(3)?,
                    size: row.get::<usize, i64>(4).unwrap_or(0) as u64,
                    modified: row.get::<usize, Option<i64>>(5)?.map(|v| v as u64),
                    is_directory: row.get::<usize, i32>(6).unwrap_or(0) != 0,
                })
            },
        )?;

        for result in rows {
            results.push(result?);
        }
    }

    // Fallback to LIKE-only if FTS5 returned nothing (or wasn't usable).
    if results.is_empty() {
        static LIKE_SQL: &str = "
            SELECT name, path, parent_path, extension, size, modified, is_directory
            FROM file_index
            WHERE root_key = ?1
              AND (name LIKE ?2 ESCAPE '\\' OR path LIKE ?2 ESCAPE '\\')
            ORDER BY
                CASE WHEN name LIKE ?2 ESCAPE '\\' THEN 0 ELSE 1 END,
                size DESC
            LIMIT ?3";

        let mut stmt = conn.prepare(LIKE_SQL)?;
        let rows = stmt.query_map(
            params![root_key, &like_pattern, limit as i64],
            |row| {
                Ok(FileSearchResult {
                    name: row.get(0)?,
                    path: row.get(1)?,
                    parent_path: row.get(2)?,
                    extension: row.get(3)?,
                    size: row.get::<usize, i64>(4).unwrap_or(0) as u64,
                    modified: row.get::<usize, Option<i64>>(5)?.map(|v| v as u64),
                    is_directory: row.get::<usize, i32>(6).unwrap_or(0) != 0,
                })
            },
        )?;

        for result in rows {
            results.push(result?);
        }
    }

    Ok(results)
}

/// Check if an index exists for a given root key.
pub fn index_exists(root_key: &str) -> rusqlite::Result<bool> {
    if !default_cache_db_path()
        .map(|p| p.exists())
        .unwrap_or(false)
    {
        return Ok(false);
    }
    let conn = open_db()?;
    let exists: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM index_metadata WHERE root_key = ?1 LIMIT 1",
            params![root_key],
            |row| row.get(0),
        )
        .optional()?;
    Ok(exists.is_some())
}

/// Get the total number of indexed entries for a root key.
pub fn index_count(root_key: &str) -> rusqlite::Result<u64> {
    if !default_cache_db_path()
        .map(|p| p.exists())
        .unwrap_or(false)
    {
        return Ok(0);
    }
    let conn = open_db()?;
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM file_index WHERE root_key = ?1",
        params![root_key],
        |row| row.get(0),
    )?;
    Ok(count as u64)
}

/// Insert or update a single file/directory entry.
/// Triggers propagate the change into file_index_fts.
pub fn upsert_entry(
    root_key: &str,
    name: &str,
    path: &str,
    parent_path: &str,
    extension: Option<&str>,
    size: u64,
    modified: Option<u64>,
    is_directory: bool,
) -> rusqlite::Result<()> {
    let conn = open_db()?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    conn.execute(
        "INSERT INTO file_index (root_key, name, path, parent_path, extension, size, modified, is_directory, indexed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(path) DO UPDATE SET
            name = excluded.name,
            parent_path = excluded.parent_path,
            extension = excluded.extension,
            size = excluded.size,
            modified = excluded.modified,
            is_directory = excluded.is_directory,
            indexed_at = excluded.indexed_at",
        params![
            root_key, name, path, parent_path,
            extension.unwrap_or(""), size as i64,
            modified.map(|v| v as i64), is_directory as i32, now
        ],
    )?;

    Ok(())
}

/// Delete an entry by path. FTS5 trigger propagates the delete.
pub fn delete_entry(path: &str) -> rusqlite::Result<()> {
    let conn = open_db()?;
    conn.execute("DELETE FROM file_index WHERE path = ?1", params![path])?;
    Ok(())
}

/// Best-effort delete of any indexed row in `root_key` whose path ends with the
/// given file name. Used as a fallback when USN delete events arrive without a
/// resolvable path (e.g. the FRN map never recorded the entry). Matches only
/// exact trailing segments by checking `path LIKE '%\<name>' OR path LIKE '%/<name>'`
/// to avoid collateral deletes on substrings.
pub fn delete_entry_by_name(root_key: &str, name: &str) -> rusqlite::Result<()> {
    if name.is_empty() {
        return Ok(());
    }
    let escaped = name.replace('%', "\\%").replace('_', "\\_");
    // Match either a Windows or POSIX separator preceding the name at the end.
    let like = format!("%\\{}", escaped);
    let conn = open_db()?;
    conn.execute(
        "DELETE FROM file_index
         WHERE root_key = ?1
           AND (path LIKE ?2 ESCAPE '\\' OR path LIKE ?3 ESCAPE '\\')",
        params![root_key, &like, &format!("%/{}", escaped)],
    )?;
    Ok(())
}

// ─── FRN Path Map Functions ────────────────────────────────────────

/// Insert or update an FRN-to-path mapping entry.
pub fn upsert_frn_path(root_key: &str, frn: &str, path: &str, parent_frn: Option<&str>, is_directory: bool) -> rusqlite::Result<()> {
    let conn = open_db()?;
    conn.execute(
        "INSERT INTO frn_path_map (root_key, frn, path, parent_frn, is_directory)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(root_key, frn) DO UPDATE SET
            path = excluded.path,
            parent_frn = excluded.parent_frn,
            is_directory = excluded.is_directory",
        params![
            root_key, frn, path,
            parent_frn, is_directory as i32
        ],
    )?;
    Ok(())
}

/// Resolve a path from FRN and file name (via parent FRN).
pub fn resolve_path_from_frn(root_key: &str, frn: &str, file_name: &str) -> rusqlite::Result<Option<String>> {
    let conn = open_db()?;
    let parent_path: Option<String> = conn
        .query_row(
            "SELECT path FROM frn_path_map WHERE root_key = ?1 AND frn = ?2 LIMIT 1",
            params![root_key, frn],
            |row| row.get(0),
        )
        .optional()?;
    Ok(parent_path.map(|p| format!("{}\\{}", p, file_name)))
}

/// Look up the stored path for a given FRN (the path recorded for that entry
/// itself, not its parent). Returns `None` if the FRN isn't tracked.
pub fn lookup_frn_path(root_key: &str, frn: &str) -> Option<String> {
    let conn = open_db().ok()?;
    conn.query_row(
        "SELECT path FROM frn_path_map WHERE root_key = ?1 AND frn = ?2 LIMIT 1",
        params![root_key, frn],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .ok()
    .flatten()
}

/// Delete an FRN mapping entry (used when file is deleted).
pub fn delete_frn_path(root_key: &str, frn: &str) -> rusqlite::Result<()> {
    let conn = open_db()?;
    conn.execute(
        "DELETE FROM frn_path_map WHERE root_key = ?1 AND frn = ?2",
        params![root_key, frn],
    )?;
    Ok(())
}
