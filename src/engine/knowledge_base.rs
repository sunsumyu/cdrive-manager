//! 知识库缓存
//!
//! 使用 SQLite 持久化缓存 RiskEngine 分析结果，避免重复计算。
//! 缓存策略：
//! - 文件路径 + 修改时间 作为缓存键
//! - TTL 过期机制（默认 7 天）
//! - 启动时自动清理过期条目
//! - 单例模式：每个 app 实例共享同一个知识库

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{Connection, params};

use crate::engine::risk_engine::AnalysisResult;

// ============================================================
// 常量
// ============================================================

/// 缓存默认 TTL（秒）：7 天。
const DEFAULT_TTL_SECONDS: i64 = 7 * 24 * 3600;
/// 数据库文件名。
const KB_DB_NAME: &str = "cdrive_kb_v1.db";
/// 表版本（用于 Schema 升级）。
const SCHEMA_VERSION: i32 = 1;

// ============================================================
// 知识库引擎
// ============================================================

/// 本地知识库，基于 SQLite 持久化。
///
/// 线程安全：通过内部 `Mutex<Connection>` 实现。
pub struct KnowledgeBase {
    db_path: PathBuf,
    conn: Mutex<Connection>,
    ttl_seconds: i64,
}

impl KnowledgeBase {
    // ---------------------------------------------------------
    // 构造 / 初始化
    // ---------------------------------------------------------

    /// 打开（或创建）默认路径的知识库。
    pub fn open_default() -> Result<Self, String> {
        let db_dir = dirs::data_dir()
            .or_else(|| dirs::home_dir().map(|h| h.join(".local").join("share")))
            .unwrap_or_else(|| PathBuf::from("."));
        let db_path = db_dir.join(KB_DB_NAME);
        Self::open(db_path)
    }

    /// 打开（或创建）指定路径的知识库。
    pub fn open(db_path: impl Into<PathBuf>) -> Result<Self, String> {
        let db_path = db_path.into();
        let conn = Connection::open(&db_path).map_err(|e| format!("无法打开知识库: {}", e))?;

        let kb = Self {
            db_path: db_path.clone(),
            conn: Mutex::new(conn),
            ttl_seconds: DEFAULT_TTL_SECONDS,
        };
        kb.init_schema()?;
        kb.gc()?; // 启动时清理过期条目
        Ok(kb)
    }

    // ---------------------------------------------------------
    // 核心读写
    // ---------------------------------------------------------

    /// 查询缓存。
    pub fn get(&self, path: &Path, modified: u64) -> Option<AnalysisResult> {
        let key = cache_key(path, modified);
        let conn = self.conn.lock().ok()?;

        let result: Result<Vec<u8>, _> = conn
            .query_row(
                "SELECT result FROM kb_cache WHERE path_hash = ?1 AND modified = ?2",
                params![&key, modified as i64],
                |row| row.get(0),
            );

        let blob = match result {
            Ok(b) => b,
            Err(rusqlite::Error::QueryReturnedNoRows) => return None,
            Err(e) => {
                eprintln!("[KnowledgeBase] 查询失败: {}", e);
                return None;
            }
        };

        // 反序列化 JSON
        match serde_json::from_slice::<AnalysisResult>(&blob) {
            Ok(entry) => Some(entry),
            Err(e) => {
                eprintln!("[KnowledgeBase] JSON 反序列化失败: {}", e);
                None
            }
        }
    }

    /// 插入或更新缓存。
    pub fn put(
        &self,
        path: &Path,
        modified: u64,
        result: &AnalysisResult,
    ) -> Result<(), String> {
        let key = cache_key(path, modified);
        let blob = serde_json::to_vec(result).map_err(|e| e.to_string())?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let path_str = path.to_string_lossy();

        let conn = self.conn.lock().map_err(|e| format!("锁获取失败: {}", e))?;
        conn.execute(
            "INSERT OR REPLACE INTO kb_cache (path_hash, path, modified, result, cached_at) 
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![&key, &path_str, modified as i64, &blob, now],
        ).map_err(|e| e.to_string())?;

        Ok(())
    }

    /// 清理过期缓存，返回清理的条目数。
    pub fn gc(&self) -> Result<usize, String> {
        let cutoff = current_timestamp() - self.ttl_seconds;
        let conn = self.conn.lock().map_err(|e| format!("锁获取失败: {}", e))?;
        let affected = conn
            .execute("DELETE FROM kb_cache WHERE cached_at < ?1", params![cutoff])
            .map_err(|e| e.to_string())?;
        Ok(affected as usize)
    }

    /// 清空全部缓存（谨慎使用）。
    pub fn clear_all(&self) -> Result<usize, String> {
        let conn = self.conn.lock().map_err(|e| format!("锁获取失败: {}", e))?;
        let affected = conn
            .execute("DELETE FROM kb_cache", [])
            .map_err(|e| e.to_string())?;
        Ok(affected as usize)
    }

    /// 获取当前缓存条目总数。
    pub fn entry_count(&self) -> Result<usize, String> {
        let conn = self.conn.lock().map_err(|e| format!("锁获取失败: {}", e))?;
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM kb_cache", [], |row| row.get(0))
            .map_err(|e| e.to_string())?;
        Ok(count as usize)
    }

    // ---------------------------------------------------------
    // Schema
    // ---------------------------------------------------------

    fn init_schema(&self) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| format!("锁获取失败: {}", e))?;

        conn.execute_batch(&format!(
            "CREATE TABLE IF NOT EXISTS kb_cache (
                path_hash  TEXT PRIMARY KEY,
                path       TEXT NOT NULL,
                modified   INTEGER NOT NULL,
                result     BLOB NOT NULL,
                cached_at  INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_kb_cached_at ON kb_cache(cached_at);
            CREATE TABLE IF NOT EXISTS kb_meta (
                key   TEXT PRIMARY KEY,
                value TEXT
            );
            INSERT OR IGNORE INTO kb_meta (key, value) VALUES ('version', '{}');
            ", SCHEMA_VERSION))
            .map_err(|e| format!("Schema 初始化失败: {}", e))?;

        Ok(())
    }
}

impl Default for KnowledgeBase {
    fn default() -> Self {
        Self::open_default().unwrap_or_else(|e| {
            eprintln!("[KnowledgeBase] 默认初始化失败，使用内存后备: {}", e);
            Self::open_in_memory().expect("内存数据库不应失败")
        })
    }
}

impl std::fmt::Debug for KnowledgeBase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KnowledgeBase")
            .field("db_path", &self.db_path)
            .field("ttl_seconds", &self.ttl_seconds)
            .finish()
    }
}

// ============================================================
// 私有辅助函数
// ============================================================

/// 生成缓存键：Blake3 哈希（32 字节 hex）。
fn cache_key(path: &Path, modified: u64) -> String {
    use blake3::Hasher;
    let mut hasher = Hasher::new();
    hasher.update(path.to_string_lossy().as_bytes());
    hasher.update(&modified.to_le_bytes());
    hasher.finalize().to_hex().to_string()
}

// 辅助函数：当前 Unix 时间戳（秒）。
fn current_timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// 内存后备（当磁盘数据库不可用时）。
impl KnowledgeBase {
    /// 打开内存数据库（用于测试）。
    pub fn open_in_memory() -> Result<Self, String> {
        let conn = Connection::open_in_memory().map_err(|e| e.to_string())?;
        let kb = Self {
            db_path: PathBuf::from(":memory:"),
            conn: Mutex::new(conn),
            ttl_seconds: DEFAULT_TTL_SECONDS,
        };
        kb.init_schema()?;
        Ok(kb)
    }
}
