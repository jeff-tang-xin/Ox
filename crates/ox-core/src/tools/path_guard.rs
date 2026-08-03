//! PathGuard — 工具层路径纠偏中间件。
//!
//! 哲学：模型猜路径，harness 兜底。
//! - 错路径能唯一/高置信匹配 → 静默改写参数 + 回写纠正说明（模型本会话学会真实布局）
//! - 无法置信匹配 → 不执行，直接返回错误 + 候选列表（省一轮往返，且教会模型）
//! - 新文件写入 → 父目录错了也纠（basename 不在索引里时退化为目录匹配）

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 与 gather_dir_recursive 的排除列表保持一致 — 抽成共享 const 防止两处漂移。
pub const EXCLUDE: &[&str] = &[
    "node_modules",
    ".git",
    "target",
    "dist",
    "build",
    "__pycache__",
    ".venv",
    ".ox",
    ".idea",
];
const MAX_FILES: usize = 20_000;
const TTL: Duration = Duration::from_secs(60);

/// 全局静态纠偏历史存储 — 跨 PathGuard 实例共享
///
/// 设计决策：使用全局静态存储是为了让 system prompt 组装层（pre_turn.rs）
/// 能够获取纠偏历史，而不需要传递 PathGuard 引用。
/// 后续可重构为通过 RuntimeEnvironment 或 AgentSession 传递。
static GLOBAL_CORRECTIONS: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

/// 获取全局纠偏历史的格式化文本（用于 system prompt 注入）
pub fn get_global_corrections_for_prompt() -> Option<String> {
    let corrections = GLOBAL_CORRECTIONS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    if corrections.len() < 2 {
        return None;
    }
    let mut s = String::from("【本会话路径纠正记录】\n");
    for (wrong, right) in &corrections {
        s.push_str(&format!("  - {} → {}\n", wrong, right));
    }
    Some(s)
}

/// 清空全局纠偏历史（新会话开始时调用）
pub fn clear_global_corrections() {
    let mut corrections = GLOBAL_CORRECTIONS
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    corrections.clear();
}

pub struct PathGuard {
    root: PathBuf,
    index: Mutex<Option<(Instant, Arc<FileIndex>)>>,
}

#[derive(Default)]
struct FileIndex {
    files: HashMap<String, Vec<PathBuf>>, // basename -> 相对路径
    dirs: HashMap<String, Vec<PathBuf>>,
}

#[derive(Debug)]
pub enum GuardAction {
    /// 继续执行；args 可能已被就地改写，note 需追加到工具结果末尾
    Proceed { note: Option<String> },
    /// 不执行，直接作为工具错误结果返回
    Abort(String),
}

impl PathGuard {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            index: Mutex::new(None),
        }
    }

    /// 写工具执行后调用，让新文件立刻可被纠偏命中
    pub fn invalidate(&self) {
        *self.index.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }

    fn record_correction(&self, wrong: String, right: String) {
        let mut corrections = GLOBAL_CORRECTIONS
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        // 避免重复记录相同的纠偏
        let is_duplicate = corrections.iter().any(|(w, r)| w == &wrong && r == &right);
        if !is_duplicate {
            corrections.push((wrong, right));
        }
        // 最多保留 10 条
        if corrections.len() > 10 {
            let excess = corrections.len() - 10;
            corrections.drain(..excess);
        }
    }

    /// 在工具执行前检查路径。
    /// - `tool`: 工具名（file_read / edit_file / file_write / delete_range / file_list）
    /// - `req`: LLM 请求的原始路径字符串
    /// - `args`: 工具参数，可能被就地改写（path 字段）
    pub fn check(&self, tool: &str, req: &str, args: &mut serde_json::Value) -> GuardAction {
        let norm = normalize(req);
        let abs = self.root.join(&norm);

        let guard = match tool {
            "file_list" => {
                if abs.is_dir() {
                    None
                } else {
                    self.resolve(&norm, true)
                }
            }
            "file_write" => {
                if abs.is_file() {
                    None
                } else {
                    self.check_new_file(&norm)
                }
            }
            _ => {
                // file_read / edit_file / delete_range
                if abs.is_file() {
                    None
                } else {
                    self.resolve(&norm, false)
                }
            }
        };

        match guard {
            None => {
                // 路径存在，顺带统一分隔符/前缀
                if let Some(obj) = args.as_object_mut()
                    && obj.contains_key("path")
                {
                    obj.insert(
                        "path".to_string(),
                        serde_json::json!(norm),
                    );
                }
                GuardAction::Proceed { note: None }
            }
            Some(Ok(real)) => {
                if let Some(obj) = args.as_object_mut()
                    && obj.contains_key("path")
                {
                    obj.insert(
                        "path".to_string(),
                        serde_json::json!(real.clone()),
                    );
                }
                // 📌 记录纠偏历史：用模型自己的错误教模型
                // 下次 system prompt 会注入【本会话路径纠正记录】
                self.record_correction(req.to_string(), real.clone());
                GuardAction::Proceed {
                    note: Some(format!(
                        "\n[PATH_GUARD] 你请求的 `{req}` 不存在，已纠正为 `{real}`。后续调用直接使用纠正后的路径。"
                    )),
                }
            }
            Some(Err((requested, candidates, is_dir))) => {
                GuardAction::Abort(format_blocked(&requested, &candidates, is_dir))
            }
        }
    }

    /// 已有文件/目录：basename 索引 + 后缀评分
    fn resolve(&self, norm: &str, is_dir: bool) -> Option<Result<String, (String, Vec<String>, bool)>> {
        let idx = self.index();
        let table = if is_dir { &idx.dirs } else { &idx.files };
        let hits = table
            .get(base_name(norm))
            .map(|v| v.as_slice())
            .unwrap_or_default();
        match pick(hits, norm) {
            Some(real) => Some(Ok(disp(&real))),
            None => Some(Err((
                norm.to_string(),
                hits.iter().take(5).map(|p| disp(p)).collect(),
                is_dir,
            ))),
        }
    }

    /// 新文件：basename 不在索引里 → 改为纠偏父目录
    fn check_new_file(
        &self,
        norm: &str,
    ) -> Option<Result<String, (String, Vec<String>, bool)>> {
        let (parent_str, fname) = match norm.rsplit_once('/') {
            Some((p, f)) if !p.is_empty() => (p, f),
            _ => return None, // 根级新文件，放行
        };
        if self.root.join(parent_str).is_dir() {
            return None; // 父目录真实存在，放行
        }
        let idx = self.index();
        let pname = parent_str.rsplit('/').next().unwrap_or_default();
        let hits = idx
            .dirs
            .get(pname)
            .map(|v| v.as_slice())
            .unwrap_or_default();
        match pick(hits, parent_str) {
            Some(dir) => Some(Ok(format!("{}/{}", disp(&dir), fname))),
            None => Some(Err((
                norm.to_string(),
                hits.iter().take(5).map(|p| disp(p)).collect(),
                true,
            ))),
        }
    }

    fn index(&self) -> Arc<FileIndex> {
        let mut slot = self.index.lock().unwrap_or_else(|p| p.into_inner());
        if slot.as_ref().map_or(true, |(t, _)| t.elapsed() >= TTL) {
            *slot = Some((Instant::now(), Arc::new(build(&self.root))));
        }
        slot.as_ref().unwrap().1.clone()
    }
}

// ── 评分与匹配 ─────────────────────────────────────────────

/// 唯一命中：无条件纠正。多命中：score = 尾部连续匹配×2 + 组件交集，
/// 要求 best ≥ 4 且严格大于次优，否则返回候选不冒险。
fn pick(hits: &[PathBuf], req: &str) -> Option<PathBuf> {
    let rc = components_of(req);
    let mut scored: Vec<(i32, &PathBuf)> = hits
        .iter()
        .map(|h| {
            let disp_h = disp(h);
            let hc = components_of(&disp_h);
            (trailing(&rc, &hc) * 2 + overlap(&rc, &hc), h)
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    match scored.as_slice() {
        [] => None,
        [(_, h)] => Some((*h).clone()),
        [(s1, h), (s2, _), ..] if *s1 >= 4 && s1 > s2 => Some((*h).clone()),
        _ => None,
    }
}

fn trailing(a: &[&str], b: &[&str]) -> i32 {
    let mut n = 0;
    for (x, y) in a.iter().rev().zip(b.iter().rev()) {
        if x == y {
            n += 1
        } else {
            break
        }
    }
    n
}

fn overlap(a: &[&str], b: &[&str]) -> i32 {
    b.iter().filter(|c| a.contains(c)).count() as i32
}

// ── 小工具 ─────────────────────────────────────────────

fn normalize(req: &str) -> String {
    req.replace('\\', "/")
        .trim_start_matches("./")
        .trim_start_matches('/')
        .trim()
        .to_string()
}

fn base_name(s: &str) -> &str {
    s.rsplit('/').next().unwrap_or(s)
}

fn components_of(s: &str) -> Vec<&str> {
    s.split('/').filter(|c| !c.is_empty()).collect()
}

fn disp(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

fn format_blocked(req: &str, cands: &[String], is_dir: bool) -> String {
    let kind = if is_dir { "目录" } else { "文件" };
    let mut s = format!("错误: {kind} `{req}` 不存在。\n");
    if !cands.is_empty() {
        s.push_str(&format!("项目中的相似{kind}:\n"));
        for c in cands {
            s.push_str(&format!("  - {c}\n"));
        }
    }
    s.push_str("请用上述路径重试，或先用 file_search / file_list 定位。");
    s
}

fn build(root: &Path) -> FileIndex {
    let mut idx = FileIndex::default();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    let mut count = 0;
    while let Some((dir, depth)) = stack.pop() {
        if depth > 6 || count >= MAX_FILES {
            continue;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.filter_map(|e| e.ok()) {
            let name = e.file_name().to_string_lossy().to_string();
            if EXCLUDE.contains(&name.as_str()) {
                continue;
            }
            let Ok(ft) = e.file_type() else {
                continue;
            };
            let rel = e.path().strip_prefix(root).unwrap().to_path_buf();
            if ft.is_dir() {
                idx.dirs.entry(name).or_default().push(rel);
                stack.push((e.path(), depth + 1));
            } else if ft.is_file() {
                idx.files.entry(name).or_default().push(rel);
                count += 1;
            }
        }
    }
    idx
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup_test_tree() -> TempDir {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        // crates/ox-core/src/llm/openai.rs
        fs::create_dir_all(root.join("crates/ox-core/src/llm")).unwrap();
        fs::write(root.join("crates/ox-core/src/llm/openai.rs"), "").unwrap();
        fs::write(root.join("crates/ox-core/src/llm/mod.rs"), "").unwrap();
        // crates/ox-core/src/tools/mod.rs
        fs::create_dir_all(root.join("crates/ox-core/src/tools")).unwrap();
        fs::write(root.join("crates/ox-core/src/tools/mod.rs"), "").unwrap();
        // crates/memory/src/anchor.rs
        fs::create_dir_all(root.join("crates/memory/src")).unwrap();
        fs::write(root.join("crates/memory/src/anchor.rs"), "").unwrap();
        // crates/ox-core/src/anchor.rs
        fs::write(root.join("crates/ox-core/src/anchor.rs"), "").unwrap();
        // 根级 Cargo.toml
        fs::write(root.join("Cargo.toml"), "").unwrap();
        tmp
    }

    #[test]
    fn correct_path_proceeds_silently() {
        let tmp = setup_test_tree();
        let guard = PathGuard::new(tmp.path().to_path_buf());
        let mut args = serde_json::json!({"path": "crates/ox-core/src/llm/openai.rs"});
        match guard.check("file_read", "crates/ox-core/src/llm/openai.rs", &mut args) {
            GuardAction::Proceed { note: None } => {}
            other => panic!("expected Proceed without note, got {:?}", other),
        }
    }

    #[test]
    fn short_basename_resolves_to_full_path() {
        let tmp = setup_test_tree();
        let guard = PathGuard::new(tmp.path().to_path_buf());
        let mut args = serde_json::json!({"path": "openai.rs"});
        match guard.check("file_read", "openai.rs", &mut args) {
            GuardAction::Proceed { note: Some(n) } => {
                assert!(n.contains("openai.rs"));
                assert!(n.contains("crates/ox-core/src/llm/openai.rs"));
            }
            other => panic!("expected Proceed with note, got Abort"),
        }
        // args should be rewritten
        assert_eq!(
            args["path"],
            "crates/ox-core/src/llm/openai.rs"
        );
    }

    #[test]
    fn ambiguous_path_returns_candidates_not_guess() {
        let tmp = setup_test_tree();
        let guard = PathGuard::new(tmp.path().to_path_buf());
        // anchor.rs exists in two places — should NOT auto-correct
        let mut args = serde_json::json!({"path": "anchor.rs"});
        match guard.check("file_read", "anchor.rs", &mut args) {
            GuardAction::Abort(msg) => {
                assert!(msg.contains("anchor.rs"));
                assert!(msg.contains("crates/memory/src/anchor.rs"));
                assert!(msg.contains("crates/ox-core/src/anchor.rs"));
            }
            GuardAction::Proceed { .. } => {
                panic!("ambiguous path should Abort, not Proceed")
            }
        }
    }

    #[test]
    fn wrong_prefix_resolves_by_trailing_match() {
        let tmp = setup_test_tree();
        let guard = PathGuard::new(tmp.path().to_path_buf());
        // src/llm/openai.rs — missing crates/ox-core/ prefix
        let mut args = serde_json::json!({"path": "src/llm/openai.rs"});
        match guard.check("file_read", "src/llm/openai.rs", &mut args) {
            GuardAction::Proceed { note: Some(_) } => {
                assert_eq!(
                    args["path"],
                    "crates/ox-core/src/llm/openai.rs"
                );
            }
            other => panic!("expected Proceed with correction, got {:?}", other),
        }
    }

    #[test]
    fn nonexistent_file_with_no_candidates_aborts() {
        let tmp = setup_test_tree();
        let guard = PathGuard::new(tmp.path().to_path_buf());
        let mut args = serde_json::json!({"path": "nonexistent_file.rs"});
        match guard.check("file_read", "nonexistent_file.rs", &mut args) {
            GuardAction::Abort(msg) => {
                assert!(msg.contains("不存在"));
            }
            GuardAction::Proceed { .. } => {
                panic!("nonexistent file should Abort")
            }
        }
    }

    #[test]
    fn file_write_corrects_parent_dir() {
        let tmp = setup_test_tree();
        let guard = PathGuard::new(tmp.path().to_path_buf());
        // LLM writes to src/llm/new.rs — parent dir "src/llm" doesn't exist at root
        // but crates/ox-core/src/llm does — should correct parent
        let mut args = serde_json::json!({"path": "src/llm/new.rs", "content": "test"});
        match guard.check("file_write", "src/llm/new.rs", &mut args) {
            GuardAction::Proceed { note: Some(_) } => {
                assert_eq!(
                    args["path"],
                    "crates/ox-core/src/llm/new.rs"
                );
            }
            other => panic!("expected Proceed with parent dir correction, got {:?}", other),
        }
    }

    #[test]
    fn backslash_paths_normalized() {
        let tmp = setup_test_tree();
        let guard = PathGuard::new(tmp.path().to_path_buf());
        let mut args = serde_json::json!({"path": "crates\\ox-core\\src\\llm\\openai.rs"});
        match guard.check(
            "file_read",
            "crates\\ox-core\\src\\llm\\openai.rs",
            &mut args,
        ) {
            GuardAction::Proceed { note: None } => {
                assert_eq!(
                    args["path"],
                    "crates/ox-core/src/llm/openai.rs"
                );
            }
            other => panic!("expected Proceed after normalization, got {:?}", other),
        }
    }

    #[test]
    fn invalidate_refreshes_index_after_write() {
        let tmp = setup_test_tree();
        let guard = PathGuard::new(tmp.path().to_path_buf());

        // new_file.rs doesn't exist yet — short path can't resolve via index
        let mut args = serde_json::json!({"path": "new_file.rs"});
        assert!(matches!(
            guard.check("file_read", "new_file.rs", &mut args),
            GuardAction::Abort(_)
        ));

        // Create the file at its real location
        fs::write(
            tmp.path().join("crates/ox-core/src/llm/new_file.rs"),
            "",
        )
        .unwrap();

        // Index still cached — short path still can't resolve (abs.is_file() is
        // false because new_file.rs is NOT at root, it's in crates/ox-core/src/llm/)
        let mut args = serde_json::json!({"path": "new_file.rs"});
        assert!(matches!(
            guard.check("file_read", "new_file.rs", &mut args),
            GuardAction::Abort(_)
        ));

        // Invalidate → index rebuilt → short path now resolves to full path
        guard.invalidate();
        let mut args = serde_json::json!({"path": "new_file.rs"});
        match guard.check("file_read", "new_file.rs", &mut args) {
            GuardAction::Proceed { note: Some(_) } => {
                assert_eq!(
                    args["path"],
                    "crates/ox-core/src/llm/new_file.rs"
                );
            }
            other => panic!("expected Proceed with note after invalidate, got {:?}", other),
        }
    }
}
