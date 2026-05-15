use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing;

use super::FileIndexManager;

/// 文件索引注册表，管理多个工作目录的索引
pub struct FileIndexRegistry {
    indices: HashMap<PathBuf, Arc<FileIndexManager>>,
    db_base_dir: PathBuf,
}

impl FileIndexRegistry {
    /// 创建新的索引注册表
    pub fn new(db_base_dir: PathBuf) -> Self {
        // 确保数据库目录存在
        if let Err(e) = std::fs::create_dir_all(&db_base_dir) {
            tracing::warn!("Failed to create db directory: {}", e);
        }

        Self {
            indices: HashMap::new(),
            db_base_dir,
        }
    }

    /// 获取或创建指定目录的文件索引管理器
    pub fn get_or_create(&mut self, dir: &Path) -> anyhow::Result<Arc<FileIndexManager>> {
        // 规范化路径（使用 dunce 处理 Windows UNC 路径）
        let canonical = dunce::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());

        // 如果已有索引，直接返回
        if let Some(manager) = self.indices.get(&canonical) {
            tracing::debug!("Using cached file index for: {:?}", canonical);
            return Ok(Arc::clone(manager));
        }

        // 创建新索引
        tracing::info!("Creating new file index for: {:?}", canonical);
        let db_path = self
            .db_base_dir
            .join(format!("file_index_{}.db", Self::dir_hash(&canonical)));

        let manager = Arc::new(FileIndexManager::new(&db_path)?);

        // 同步扫描 Git 仓库
        match manager.scan_from_git(&canonical) {
            Ok(count) => {
                tracing::info!("Indexed {} files for {:?}", count, canonical);
            }
            Err(e) => {
                tracing::warn!("Failed to scan git repo at {:?}: {}", canonical, e);
            }
        }

        // 启动后台定期刷新
        let manager_clone = Arc::clone(&manager);
        let canonical_clone = canonical.clone();
        tokio::spawn(async move {
            manager_clone
                .start_periodic_refresh(canonical_clone, 120)
                .await;
        });

        // 缓存索引
        self.indices.insert(canonical, Arc::clone(&manager));

        Ok(manager)
    }

    /// 预加载指定目录的索引（异步，不阻塞）
    pub async fn preload(&mut self, dir: &Path) -> anyhow::Result<()> {
        let _ = self.get_or_create(dir)?;
        Ok(())
    }

    /// 清理长时间未使用的索引（可选优化）
    pub fn cleanup_unused(&mut self, keep_recent: usize) {
        if self.indices.len() <= keep_recent {
            return;
        }

        // 简单策略：保留最近的 N 个索引
        // TODO: 可以实现 LRU 缓存策略
        let to_remove = self.indices.len() - keep_recent;
        let mut keys: Vec<_> = self.indices.keys().cloned().collect();
        keys.sort(); // 确定性顺序

        for key in keys.into_iter().take(to_remove) {
            self.indices.remove(&key);
            tracing::debug!("Removed cached file index for: {:?}", key);
        }
    }

    /// 计算目录的唯一哈希值（用于数据库文件名）
    fn dir_hash(dir: &Path) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        dir.hash(&mut hasher);
        format!("{:x}", hasher.finish())
    }

    /// 获取当前缓存的索引数量
    pub fn cached_count(&self) -> usize {
        self.indices.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_registry_creation() {
        let temp_dir = TempDir::new().unwrap();
        let db_dir = temp_dir.path().join("db");

        let mut registry = FileIndexRegistry::new(db_dir.clone());
        assert_eq!(registry.cached_count(), 0);

        // 数据库目录应该被创建
        assert!(db_dir.exists());
    }

    #[tokio::test]
    async fn test_get_or_create_same_dir() {
        let temp_dir = TempDir::new().unwrap();
        let db_dir = temp_dir.path().join("db");

        let mut registry = FileIndexRegistry::new(db_dir);

        // 第一次创建
        let manager1 = registry.get_or_create(temp_dir.path()).unwrap();
        assert_eq!(registry.cached_count(), 1);

        // 第二次应该复用
        let manager2 = registry.get_or_create(temp_dir.path()).unwrap();
        assert_eq!(registry.cached_count(), 1);

        // 应该是同一个实例
        assert!(Arc::ptr_eq(&manager1, &manager2));
    }
}
