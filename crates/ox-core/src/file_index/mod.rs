use rusqlite::{Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tracing;

pub mod registry;
pub use registry::FileIndexRegistry;

/// 文件索引条目
#[derive(Debug, Clone)]
pub struct FileIndexEntry {
    pub id: i64,
    pub filename: String,
    pub full_path: String,
    pub file_type: Option<String>,
}

/// 文件索引管理器
pub struct FileIndexManager {
    conn: Arc<Mutex<Connection>>,
}

impl FileIndexManager {
    /// 创建新的文件索引管理器并初始化数据库
    pub fn new(db_path: &Path) -> anyhow::Result<Self> {
        // 确保父目录存在
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let conn = Connection::open(db_path)?;

        // 启用 WAL 模式以提高并发性能
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;

        // 创建表结构
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS file_index (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                filename TEXT NOT NULL,
                full_path TEXT NOT NULL UNIQUE,
                file_type TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_filename ON file_index(filename);",
        )?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// 从现有连接创建（用于内部使用）
    fn from_connection(conn: Arc<Mutex<Connection>>) -> Self {
        Self { conn }
    }

    /// 清空索引（用于重建）
    pub fn clear(&self) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM file_index", [])?;
        Ok(())
    }

    /// 批量插入文件索引
    pub fn batch_insert(&self, entries: &[FileIndexEntry]) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        let tx = conn.unchecked_transaction()?;

        for entry in entries {
            let file_type_str = entry.file_type.as_deref().unwrap_or("");
            tx.execute(
                "INSERT OR REPLACE INTO file_index (filename, full_path, file_type) 
                 VALUES (?1, ?2, ?3)",
                [
                    entry.filename.as_str(),
                    entry.full_path.as_str(),
                    file_type_str,
                ],
            )?;
        }

        tx.commit()?;
        Ok(())
    }

    /// 根据文件名查找（可能返回多个结果）
    pub fn find_by_filename(&self, filename: &str) -> anyhow::Result<Vec<FileIndexEntry>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, filename, full_path, file_type 
             FROM file_index 
             WHERE filename = ?1",
        )?;

        let rows = stmt.query_map([filename], |row| {
            Ok(FileIndexEntry {
                id: row.get(0)?,
                filename: row.get(1)?,
                full_path: row.get(2)?,
                file_type: row.get(3)?,
            })
        })?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }

        Ok(results)
    }

    /// 根据文件ID精确查找
    pub fn find_by_id(&self, id: i64) -> anyhow::Result<Option<FileIndexEntry>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, filename, full_path, file_type 
             FROM file_index 
             WHERE id = ?1",
        )?;

        let result = stmt
            .query_row([id], |row| {
                Ok(FileIndexEntry {
                    id: row.get(0)?,
                    filename: row.get(1)?,
                    full_path: row.get(2)?,
                    file_type: row.get(3)?,
                })
            })
            .optional()?;

        Ok(result)
    }

    /// 获取所有文件列表（用于 file_list 工具）
    pub fn list_all_files(&self) -> anyhow::Result<Vec<FileIndexEntry>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, filename, full_path, file_type 
             FROM file_index 
             ORDER BY full_path",
        )?;

        let rows = stmt.query_map([], |row| {
            Ok(FileIndexEntry {
                id: row.get(0)?,
                filename: row.get(1)?,
                full_path: row.get(2)?,
                file_type: row.get(3)?,
            })
        })?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }

        Ok(results)
    }

    /// 删除文件索引
    pub fn delete_by_path(&self, full_path: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM file_index WHERE full_path = ?1", [full_path])?;
        Ok(())
    }

    /// 添加单个文件到索引（用于实时更新）
    pub fn add_file(&self, relative_path: &str) -> anyhow::Result<()> {
        let path = Path::new(relative_path);
        let filename = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();

        let file_type = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_string());

        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO file_index (filename, full_path, file_type) 
             VALUES (?1, ?2, ?3)",
            [
                &filename,
                relative_path,
                &file_type.as_deref().unwrap_or(""),
            ],
        )?;

        tracing::debug!("Added file to index: {}", relative_path);
        Ok(())
    }

    /// 从索引中移除文件（用于文件删除）
    pub fn remove_file_by_path(&self, relative_path: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM file_index WHERE full_path = ?1",
            [relative_path],
        )?;
        tracing::debug!("Removed file from index: {}", relative_path);
        Ok(())
    }

    /// 运行 Git 命令并返回输出
    fn run_git_cmd(args: &[&str], working_dir: &Path) -> anyhow::Result<String> {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(working_dir)
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("git command failed: {}", stderr));
        }

        Ok(String::from_utf8(output.stdout)?)
    }

    /// 解析文件列表为索引条目
    fn parse_file_list(file_list: &str) -> Vec<FileIndexEntry> {
        file_list
            .lines()
            .filter(|line| !line.is_empty())
            .map(|path_str| {
                let path = Path::new(path_str);
                let filename = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string();

                let file_type = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|s| s.to_string());

                FileIndexEntry {
                    id: 0, // Will be auto-assigned by SQLite
                    filename,
                    full_path: path_str.to_string(),
                    file_type,
                }
            })
            .collect()
    }

    /// 同步扫描：Git 追踪 + 未追踪文件（启动时使用）
    pub fn scan_from_git(&self, working_dir: &Path) -> anyhow::Result<usize> {
        tracing::info!("Starting file index scan from git...");

        // 检查是否是 Git 仓库
        if !working_dir.join(".git").exists() {
            return Err(anyhow::anyhow!(
                "Directory is not a git repository. Please run 'git init' first."
            ));
        }

        // 1. Git 追踪的文件
        let tracked = Self::run_git_cmd(&["ls-files", "--full-name"], working_dir)?;

        // 2. 未追踪的文件（尊重 .gitignore）
        let untracked =
            Self::run_git_cmd(&["ls-files", "--others", "--exclude-standard"], working_dir)?;

        // 3. 合并文件列表
        let all_files = format!("{}\n{}", tracked, untracked);
        let entries = Self::parse_file_list(&all_files);
        let count = entries.len();

        // 4. 清空并重新插入
        self.clear()?;
        self.batch_insert(&entries)?;

        tracing::info!("Indexed {} files from git", count);
        Ok(count)
    }

    /// 异步刷新索引（后台定期调用）
    pub async fn start_periodic_refresh(&self, working_dir: PathBuf, interval_secs: u64) {
        let conn_clone = Arc::clone(&self.conn);

        tokio::spawn(async move {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(interval_secs)).await;

                match Self::refresh_index(&conn_clone, &working_dir).await {
                    Ok(count) => {
                        tracing::debug!("Refreshed file index: {} files", count);
                    }
                    Err(e) => {
                        tracing::warn!("Failed to refresh file index: {}", e);
                    }
                }
            }
        });
    }

    /// 执行一次索引刷新
    async fn refresh_index(
        conn: &Arc<Mutex<Connection>>,
        working_dir: &Path,
    ) -> anyhow::Result<usize> {
        let manager = Self::from_connection(Arc::clone(conn));
        manager.scan_from_git(working_dir)
    }

    /// 启动文件系统监听（实时捕获文件变化）
    pub fn start_file_watcher(&self, working_dir: PathBuf) -> anyhow::Result<()> {
        use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
        use std::sync::mpsc;

        let file_index = Arc::clone(&self.conn);

        // 创建监听通道
        let (tx, rx) = mpsc::channel();
        let mut watcher = RecommendedWatcher::new(tx, notify::Config::default())?;

        // 递归监听工作目录
        watcher.watch(&working_dir, RecursiveMode::Recursive)?;

        // 启动后台处理线程
        std::thread::spawn(move || {
            tracing::info!("File watcher started for {:?}", working_dir);

            for result in rx {
                match result {
                    Ok(event) => {
                        // 过滤掉 .git、target 等目录
                        if Self::should_ignore_path(&event.paths, &working_dir) {
                            continue;
                        }

                        match event.kind {
                            EventKind::Create(_) | EventKind::Modify(_) => {
                                for path in &event.paths {
                                    if let Some(rel_path) = path.strip_prefix(&working_dir).ok() {
                                        let rel_str = rel_path.to_string_lossy();
                                        // 只处理文件，忽略目录
                                        if rel_str.contains('.') {
                                            let manager =
                                                Self::from_connection(Arc::clone(&file_index));
                                            if let Err(e) = manager.add_file(&rel_str) {
                                                tracing::warn!(
                                                    "Failed to update index for {}: {}",
                                                    rel_str,
                                                    e
                                                );
                                            } else {
                                                tracing::debug!("Indexed file: {}", rel_str);
                                            }
                                        }
                                    }
                                }
                            }
                            EventKind::Remove(_) => {
                                for path in &event.paths {
                                    if let Some(rel_path) = path.strip_prefix(&working_dir).ok() {
                                        let rel_str = rel_path.to_string_lossy();
                                        let manager =
                                            Self::from_connection(Arc::clone(&file_index));
                                        if let Err(e) = manager.remove_file_by_path(&rel_str) {
                                            tracing::warn!(
                                                "Failed to remove from index {}: {}",
                                                rel_str,
                                                e
                                            );
                                        } else {
                                            tracing::debug!("Removed from index: {}", rel_str);
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Watch error: {:?}", e);
                    }
                }
            }
        });

        Ok(())
    }

    /// 判断是否应该忽略某些路径
    fn should_ignore_path(paths: &[std::path::PathBuf], working_dir: &Path) -> bool {
        paths.iter().any(|p| {
            if let Ok(rel_path) = p.strip_prefix(working_dir) {
                rel_path.components().any(|c| {
                    matches!(
                        c.as_os_str().to_str(),
                        Some(".git")
                            | Some("target")
                            | Some("node_modules")
                            | Some(".ox")
                            | Some("dist")
                            | Some("build")
                    )
                })
            } else {
                false
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_create_index() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("file_index.db");

        let manager = FileIndexManager::new(&db_path).unwrap();
        assert!(
            manager
                .conn
                .lock()
                .unwrap()
                .prepare("SELECT COUNT(*) FROM file_index")
                .is_ok()
        );
    }

    #[test]
    fn test_batch_insert_and_query() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("file_index.db");

        let manager = FileIndexManager::new(&db_path).unwrap();

        let entries = vec![
            FileIndexEntry {
                id: 0,
                filename: "main.rs".to_string(),
                full_path: "src/main.rs".to_string(),
                file_type: Some("rs".to_string()),
            },
            FileIndexEntry {
                id: 0,
                filename: "lib.rs".to_string(),
                full_path: "src/lib.rs".to_string(),
                file_type: Some("rs".to_string()),
            },
        ];

        manager.batch_insert(&entries).unwrap();

        // 测试按文件名查找
        let results = manager.find_by_filename("main.rs").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].full_path, "src/main.rs");

        // 测试按 ID 查找
        let result = manager.find_by_id(results[0].id).unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().filename, "main.rs");

        // 测试列出所有文件
        let all = manager.list_all_files().unwrap();
        assert_eq!(all.len(), 2);
    }
}
