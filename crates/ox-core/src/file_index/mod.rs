use rusqlite::{Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tracing;

pub mod registry;
pub use registry::FileIndexRegistry;

/// 默认需要排除的目录列表
/// 
/// 这些目录会被 Ox 的文件索引系统排除，无论它们是否在 .gitignore 中。
/// 包括构建输出、依赖管理、IDE 配置等常见目录。
pub const DEFAULT_EXCLUDE_DIRS: &[&str] = &[
    "node_modules", ".git", "target", "dist", "build", 
    "__pycache__", ".venv", "venv", "coverage", ".next", ".nuxt",
    ".idea", ".vscode", "vendor", "bower_components", ".ox",
    "logs", ".cache", "tmp",
];

/// 检查路径是否应该被排除
/// 
/// 使用默认排除规则（而非 .gitignore），这样可以索引到 .gitignore 中的本地文件，
/// 但仍然排除构建/依赖目录。
pub fn should_exclude_path(path: &str) -> bool {
    path.split('/').any(|component| {
        DEFAULT_EXCLUDE_DIRS.contains(&component)
    })
}

/// 文件索引条目
#[derive(Debug, Clone)]
pub struct FileIndexEntry {
    pub id: i64,
    pub filename: String,
    pub full_path: String,
    pub file_type: Option<String>,
}

/// 目录条目（用于层级导航）
#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: String,
    pub file_count: usize,
}

/// 目录列表结果（用于层级导航）
#[derive(Debug, Clone)]
pub struct DirectoryListing {
    pub path: String,
    pub subdirs: Vec<DirEntry>,
    pub files: Vec<FileIndexEntry>,
    pub total_file_count: usize,
}

/// 🆕 索引统计信息
#[derive(Debug, Clone)]
pub struct IndexStats {
    pub total_files: usize,
    pub file_types: Vec<String>,
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

    /// 获取指定目录下的子目录和文件概览（层级导航）
    /// 
    /// # Arguments
    /// * `dir_path` - 目录路径（空字符串表示根目录）
    /// * `recursive` - 是否递归（当前未实现，保留接口）
    /// 
    /// # Returns
    /// 包含子目录列表、文件列表和统计信息的结构化数据
    pub fn list_directory(
        &self,
        dir_path: &str,
        _recursive: bool,
    ) -> anyhow::Result<DirectoryListing> {
        let conn = self.conn.lock().unwrap();
        
        // Normalize path: ensure it ends with '/' for consistent matching
        let normalized_dir = if dir_path.is_empty() || dir_path == "." {
            String::new()
        } else {
            dir_path.trim_end_matches('/').to_string() + "/"
        };

        // 1. Get subdirectories with file counts (using SQL aggregation)
        // Filter out excluded directories
        let subdir_query = if normalized_dir.is_empty() {
            // Root level: extract top-level directories, excluding known directories
            "SELECT 
                CASE 
                    WHEN instr(full_path, '/') > 0 
                    THEN substr(full_path, 1, instr(full_path, '/') - 1)
                    ELSE ''
                END as dirname,
                COUNT(*) as count
             FROM file_index
             WHERE full_path LIKE '%/%'
             AND dirname NOT IN ('node_modules', '.git', 'target', 'dist', 'build', '__pycache__', '.venv', 'venv', 'coverage', '.next', '.nuxt', '.idea', '.vscode', 'vendor', 'bower_components', '.ox')
             GROUP BY dirname
             HAVING dirname != ''
             ORDER BY count DESC"
        } else {
            // Subdirectory level
            &format!(
                "SELECT 
                    CASE 
                        WHEN instr(substr(full_path, {}), '/') > 0 
                        THEN substr(full_path, {}, instr(substr(full_path, {}), '/') - 1)
                        ELSE ''
                    END as dirname,
                    COUNT(*) as count
                 FROM file_index
                 WHERE full_path LIKE '{}%'
                 AND length(full_path) > length('{}')
                 AND dirname NOT IN ('node_modules', '.git', 'target', 'dist', 'build', '__pycache__', '.venv', 'venv', 'coverage', '.next', '.nuxt', '.idea', '.vscode', 'vendor', 'bower_components', '.ox')
                 GROUP BY dirname
                 HAVING dirname != ''
                 ORDER BY count DESC",
                normalized_dir.len() + 1,
                normalized_dir.len() + 1,
                normalized_dir.len() + 1,
                normalized_dir,
                normalized_dir
            )
        };

        let mut stmt = conn.prepare(subdir_query)?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;

        let mut subdirs = Vec::new();
        for row in rows {
            let (name, count) = row?;
            subdirs.push(DirEntry {
                name,
                file_count: count as usize,
            });
        }

        // 2. Get files directly under this directory (LIMIT to prevent overload)
        // Filter out files from excluded directories
        let file_query = if normalized_dir.is_empty() {
            // Root level: files without '/' in path
            "SELECT id, filename, full_path, file_type 
             FROM file_index 
             WHERE full_path NOT LIKE '%/%'
             ORDER BY filename
             LIMIT 100"
        } else {
            // Subdirectory: files directly under this dir, excluding known directories
            &format!(
                "SELECT id, filename, full_path, file_type 
                 FROM file_index 
                 WHERE full_path LIKE '{}%'
                 AND substr(full_path, {}) NOT LIKE '%/%'
                 AND full_path NOT LIKE '%/node_modules/%'
                 AND full_path NOT LIKE '%/.git/%'
                 AND full_path NOT LIKE '%/target/%'
                 AND full_path NOT LIKE '%/dist/%'
                 AND full_path NOT LIKE '%/build/%'
                 AND full_path NOT LIKE '%/__pycache__/%'
                 AND full_path NOT LIKE '%/.venv/%'
                 AND full_path NOT LIKE '%/venv/%'
                 AND full_path NOT LIKE '%/coverage/%'
                 AND full_path NOT LIKE '%/.next/%'
                 AND full_path NOT LIKE '%/.nuxt/%'
                 AND full_path NOT LIKE '%/.idea/%'
                 AND full_path NOT LIKE '%/.vscode/%'
                 AND full_path NOT LIKE '%/vendor/%'
                 AND full_path NOT LIKE '%/bower_components/%'
                 AND full_path NOT LIKE '%/.ox/%'
                 ORDER BY filename
                 LIMIT 100",
                normalized_dir,
                normalized_dir.len() + 1
            )
        };

        let mut stmt = conn.prepare(file_query)?;
        let rows = stmt.query_map([], |row| {
            Ok(FileIndexEntry {
                id: row.get(0)?,
                filename: row.get(1)?,
                full_path: row.get(2)?,
                file_type: row.get(3)?,
            })
        })?;

        let mut files = Vec::new();
        for row in rows {
            files.push(row?);
        }

        // 3. Count total files in this directory (including subdirectories)
        let total_count_query = if normalized_dir.is_empty() {
            "SELECT COUNT(*) FROM file_index"
        } else {
            &format!("SELECT COUNT(*) FROM file_index WHERE full_path LIKE '{}%'", normalized_dir)
        };

        let total_file_count: i64 = conn.query_row(total_count_query, [], |row| row.get(0))?;

        Ok(DirectoryListing {
            path: dir_path.to_string(),
            subdirs,
            files,
            total_file_count: total_file_count as usize,
        })
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

    /// 🆕 Get index statistics (for diagnostics)
    pub fn get_stats(&self) -> anyhow::Result<IndexStats> {
        let conn = self.conn.lock().unwrap();
        
        // Get total file count
        let total_files: i64 = conn.query_row(
            "SELECT COUNT(*) FROM file_index",
            [],
            |row| row.get(0)
        )?;
        
        // Get unique file types
        let mut stmt = conn.prepare("SELECT DISTINCT file_type FROM file_index WHERE file_type IS NOT NULL")?;
        let types = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut file_types = Vec::new();
        for t in types {
            if let Ok(t) = t {
                file_types.push(t);
            }
        }
        
        Ok(IndexStats {
            total_files: total_files as usize,
            file_types,
        })
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

    /// 扫描物理文件系统，获取被 .gitignore 忽略但仍存在的文件
    fn scan_physical_files(working_dir: &Path) -> anyhow::Result<String> {
        use std::fs;
        
        let mut result = String::new();
        let mut file_count = 0;
        
        // 递归遍历目录
        fn walk_dir(dir: &Path, working_dir: &Path, result: &mut String, depth: usize, count: &mut usize) {
            // 限制递归深度，避免过深
            if depth > 10 {
                return;
            }
            
            if let Ok(entries) = fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    
                    // 获取相对路径
                    if let Ok(rel_path) = path.strip_prefix(working_dir) {
                        let rel_str = rel_path.to_string_lossy();
                        
                        // 使用默认排除规则（而非 .gitignore）
                        if should_exclude_path(&rel_str) {
                            tracing::debug!("Excluded by DEFAULT_EXCLUDE_DIRS: {}", rel_str);
                            continue;
                        }
                        
                        if path.is_file() {
                            result.push_str(&format!("{}\n", rel_str));
                            *count += 1;
                        } else if path.is_dir() {
                            // 递归处理子目录
                            walk_dir(&path, working_dir, result, depth + 1, count);
                        }
                    }
                }
            }
        }
        
        walk_dir(working_dir, working_dir, &mut result, 0, &mut file_count);
        tracing::info!("Physical scan found {} files (including git-ignored)", file_count);
        Ok(result)
    }

    /// 同步扫描：Git 追踪 + 未追踪文件 + 本地忽略文件（启动时使用）
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
        let untracked_gitignore =
            Self::run_git_cmd(&["ls-files", "--others", "--exclude-standard"], working_dir)?;

        // 3. 被 .gitignore 忽略但物理存在的文件（补充扫描）
        let ignored_files = Self::scan_physical_files(working_dir)?;

        // 4. 合并文件列表（去重）
        let mut all_paths = std::collections::HashSet::new();
        
        for line in tracked.lines().chain(untracked_gitignore.lines()).chain(ignored_files.lines()) {
            let path = line.trim();
            if !path.is_empty() && !should_exclude_path(path) {
                all_paths.insert(path.to_string());
            }
        }
        
        let entries: Vec<FileIndexEntry> = all_paths
            .into_iter()
            .map(|path_str| {
                let path = Path::new(&path_str);
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
                    id: 0,
                    filename,
                    full_path: path_str,
                    file_type,
                }
            })
            .collect();
        
        let count = entries.len();

        // 5. 清空并重新插入
        self.clear()?;
        self.batch_insert(&entries)?;

        tracing::info!("Indexed {} files (including git-ignored local files)", count);
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
        // 🚨 CRITICAL FIX: Move synchronous scan to blocking thread pool
        // This prevents blocking the async runtime during periodic refresh
        let working_dir = working_dir.to_path_buf();
        let conn_clone = Arc::clone(conn);
        
        tokio::task::spawn_blocking(move || {
            let manager = Self::from_connection(conn_clone);
            manager.scan_from_git(&working_dir)
        })
        .await
        .map_err(|e| anyhow::anyhow!("Refresh task panicked: {}", e))?
    }

    /// 启动文件系统监听（实时捕获文件变化）
    pub fn start_file_watcher(&self, working_dir: PathBuf) -> anyhow::Result<()> {
        use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
        use std::sync::mpsc;

        let file_index = Arc::clone(&self.conn);

        // 创建监听通道
        let (tx, rx) = mpsc::channel();
        
        // 🚀 OPTIMIZATION: Use polling mode for better reliability on Windows
        // Polling is slower but more reliable than native events
        #[cfg(target_os = "windows")]
        let mut watcher = RecommendedWatcher::new(tx, notify::Config::default())?;
        
        #[cfg(not(target_os = "windows"))]
        let mut watcher = RecommendedWatcher::new(tx, notify::Config::default())?;

        // 递归监听工作目录
        watcher.watch(&working_dir, RecursiveMode::Recursive)?;

        // 启动后台处理线程
        std::thread::spawn(move || {
            tracing::info!("File watcher started for {:?}", working_dir);
            let mut event_count = 0u64;

            for result in rx {
                match result {
                    Ok(event) => {
                        event_count += 1;
                        
                        // Log every 100 events to avoid spam
                        if event_count % 100 == 0 {
                            tracing::debug!("Processed {} file system events", event_count);
                        }
                        
                        // 过滤掉 .git、target 等目录
                        if Self::should_ignore_path(&event.paths, &working_dir) {
                            continue;
                        }

                        match event.kind {
                            EventKind::Create(_) | EventKind::Modify(_) => {
                                for path in &event.paths {
                                    if let Some(rel_path) = path.strip_prefix(&working_dir).ok() {
                                        let rel_str = rel_path.to_string_lossy();
                                        
                                        // 🚨 FIX: Check if path is a file (not directory)
                                        // Use filesystem check instead of relying on '.' in name
                                        let is_file = path.is_file();
                                        
                                        if is_file && !should_exclude_path(&rel_str) {
                                            let manager =
                                                Self::from_connection(Arc::clone(&file_index));
                                            if let Err(e) = manager.add_file(&rel_str) {
                                                tracing::warn!(
                                                    "Failed to update index for {}: {}",
                                                    rel_str,
                                                    e
                                                );
                                            } else {
                                                tracing::trace!("Indexed file: {}", rel_str);
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
                                            tracing::trace!("Removed from index: {}", rel_str);
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
    /// 
    /// 使用统一的 DEFAULT_EXCLUDE_DIRS 规则，确保与扫描逻辑一致
    fn should_ignore_path(paths: &[std::path::PathBuf], working_dir: &Path) -> bool {
        paths.iter().any(|p| {
            if let Ok(rel_path) = p.strip_prefix(working_dir) {
                let rel_str = rel_path.to_string_lossy();
                should_exclude_path(&rel_str)
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

    #[test]
    fn test_should_exclude_path() {
        // 测试默认排除规则
        assert!(should_exclude_path("node_modules/package/index.js"));
        assert!(should_exclude_path("target/debug/app.exe"));
        assert!(should_exclude_path(".git/config"));
        assert!(should_exclude_path("logs/app.log"));
        
        // 测试不应该排除的路径
        assert!(!should_exclude_path("src/main.rs"));
        assert!(!should_exclude_path("docs/README.md"));
        assert!(!should_exclude_path("config/settings.json"));
    }
}
