use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::Context;
use rusqlite::{Connection, OptionalExtension, params};

use crate::model::ScanStats;

pub fn default_cache_db_path() -> anyhow::Result<PathBuf> {
    if let Some(local_app_data) = env::var_os("LOCALAPPDATA") {
        return Ok(PathBuf::from(local_app_data)
            .join("cdrive-manager")
            .join("scan-cache.sqlite3"));
    }

    Ok(env::current_dir()?.join(".cdrive-manager-cache.sqlite3"))
}

pub fn save_latest_scan(stats: &ScanStats) -> anyhow::Result<PathBuf> {
    let db_path = default_cache_db_path()?;
    save_latest_scan_to_path(&db_path, stats)?;
    Ok(db_path)
}

pub fn load_latest_scan(root: &Path) -> anyhow::Result<Option<ScanStats>> {
    let db_path = default_cache_db_path()?;
    if !db_path.exists() {
        return Ok(None);
    }
    load_latest_scan_from_path(&db_path, root)
}

pub fn save_latest_scan_to_path(db_path: &Path, stats: &ScanStats) -> anyhow::Result<()> {
    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let connection = Connection::open(db_path)?;
    initialize_schema(&connection)?;

    let mut stats = stats.clone();
    stats.prepare_for_save();
    let stats_json = serde_json::to_vec(&stats)?;
    let root_key = root_key(&stats.root);
    let saved_at = stats.saved_at_unix_secs.unwrap_or_default();

    connection.execute(
        "INSERT INTO scan_cache (
            root_key,
            root_display,
            schema_version,
            app_version,
            saved_at_unix_secs,
            total_size,
            file_count,
            dir_count,
            error_count,
            stats_json
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
        ON CONFLICT(root_key) DO UPDATE SET
            root_display = excluded.root_display,
            schema_version = excluded.schema_version,
            app_version = excluded.app_version,
            saved_at_unix_secs = excluded.saved_at_unix_secs,
            total_size = excluded.total_size,
            file_count = excluded.file_count,
            dir_count = excluded.dir_count,
            error_count = excluded.error_count,
            stats_json = excluded.stats_json",
        params![
            root_key,
            stats.root.display().to_string(),
            stats.schema_version as i64,
            stats.app_version.as_deref(),
            saved_at as i64,
            stats.total_size as i64,
            stats.file_count as i64,
            stats.dir_count as i64,
            stats.error_count as i64,
            stats_json,
        ],
    )?;

    Ok(())
}

pub fn load_latest_scan_from_path(
    db_path: &Path,
    root: &Path,
) -> anyhow::Result<Option<ScanStats>> {
    let connection = Connection::open(db_path)?;
    initialize_schema(&connection)?;

    let stats_json: Option<Vec<u8>> = connection
        .query_row(
            "SELECT stats_json FROM scan_cache WHERE root_key = ?1",
            params![root_key(root)],
            |row| row.get(0),
        )
        .optional()?;

    stats_json
        .map(|stats_json| {
            let mut stats: ScanStats = serde_json::from_slice(&stats_json)
                .context("SQLite 缓存中的扫描结果 JSON 无法解析")?;
            stats.normalize_cache_metadata_after_load();
            stats.rebuild_indexes();
            Ok(stats)
        })
        .transpose()
}

fn initialize_schema(connection: &Connection) -> rusqlite::Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS scan_cache (
            root_key TEXT PRIMARY KEY,
            root_display TEXT NOT NULL,
            schema_version INTEGER NOT NULL,
            app_version TEXT,
            saved_at_unix_secs INTEGER NOT NULL,
            total_size INTEGER NOT NULL,
            file_count INTEGER NOT NULL,
            dir_count INTEGER NOT NULL,
            error_count INTEGER NOT NULL,
            stats_json BLOB NOT NULL
        );",
    )
}

fn root_key(path: &Path) -> String {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    canonical
        .display()
        .to_string()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::FileRecord;

    #[test]
    fn root_key_normalizes_case_and_slashes() {
        assert_eq!(
            root_key(Path::new("C:/Users/Alice/")),
            root_key(Path::new("c:\\users\\alice"))
        );
    }

    #[test]
    fn sqlite_cache_round_trips_scan_stats() {
        let db_path = env::temp_dir().join(format!(
            "cdrive-manager-test-{}.sqlite3",
            std::process::id()
        ));
        let _ = fs::remove_file(&db_path);
        let root = env::temp_dir().join(format!("cdrive-manager-root-{}", std::process::id()));
        let mut stats = ScanStats::default();
        stats.root = root.clone();
        stats.total_size = 10;
        stats.file_count = 1;
        stats.all_files.push(FileRecord {
            path: root.join("a.bin"),
            size: 10,
            modified: None,
            extension: ".bin".to_owned(),
        });

        save_latest_scan_to_path(&db_path, &stats).unwrap();
        let loaded = load_latest_scan_from_path(&db_path, &root)
            .unwrap()
            .unwrap();

        assert_eq!(loaded.root, root);
        assert_eq!(loaded.total_size, 10);
        assert_eq!(loaded.all_files.len(), 1);
        assert!(loaded.saved_at_unix_secs.is_some());

        let _ = fs::remove_file(&db_path);
    }
}
