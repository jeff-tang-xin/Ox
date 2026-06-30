# 🔍 GitNexus 集成诊断报告

## 📊 当前状态

**GitNexus 版本**: 1.6.5 ✅  
**集成方式**: MCP (Model Context Protocol) over stdio  
**代码状态**: 有未提交改动（可能导致索引 dirty）

---

## 🎯 问题分析：为什么使用不达预期？

基于代码审查，发现了 **4 个主要问题**：

---

### **问题 1：LLM 不知道何时应该使用 code_graph**

#### 当前情况
- ✅ `code_graph` 工具已注册并可用
- ❌ 但 LLM 只在少数场景下被**明确提示**使用它

#### 证据

**1. 在 `unified_action.rs` 中，code_graph 被列入工具列表**
```rust
// crates/ox-core/src/agent/unified_action.rs:417-423
if spec.allowed.contains(&"code_graph") {
    out.push_str(
        "🕸 code_graph(GitNexus 代码图谱)：改动/重构前先建关系模型与影响面——\
         op=query(概念→执行流) / context(单符号谁调谁、读写) / impact(改 X 的爆炸半径) / \
         detect_changes(未提交改动影响) / api_impact(改路由前)。比 grep 更懂调用关系；拿不准影响面别盲改。\n",
    );
}
```

**问题**：这个提示太简短，只出现在 `[UNIFIED_ROUTE]` 中，而且**只强调"改动前"使用**，没有强调**理解代码时也应该用**。

**2. 在 `allowed_actions_for_phase` 中，code_graph 在多个阶段可用**
```rust
// crates/ox-core/src/agent/unified_action.rs:474-514
pub fn allowed_actions_for_phase(phase: &str) -> &'static [&'static str] {
    match phase {
        "implement" => &["file_read", "edit_file", "find_symbol", "code_graph", ...],
        "review" => &["file_read", "find_symbol", "code_graph", ...],
        _ => &["file_read", "find_symbol", "code_graph", ...],
    }
}
```

**问题**：虽然可用，但没有明确的**使用指导**和**推荐场景**。

---

### **问题 2：find_symbol 的 GitNexus 增强被默认禁用**

#### 关键发现

```rust
// crates/ox-core/src/tools/find_symbol.rs:174-176
async fn enrich_with_graph(ctx: &ToolContext, name: &str, file_path: Option<&str>) -> Option<String> {
    if !ctx.config.gitnexus.augment_find_symbol {  // ❌ 默认 false！
        return None;
    }
    // ...
}
```

**配置检查**：
```rust
// crates/ox-core/src/config/mod.rs:349
// augment_find_symbol = true  # 需要手动在 config.toml 中启用！
```

**影响**：
- `find_symbol` 只返回 tree-sitter 结果，**不会自动附加调用关系**
- LLM 看不到 "谁调用了这个函数" / "这个函数调用了谁"
- 错过了 GitNexus 最强大的能力

---

### **问题 3：code_graph 只在特定场景下被"推荐"**

#### 当前推荐逻辑

```rust
// crates/ox-core/src/agent/unified_action.rs
// 只在 Review 阶段推荐 find_symbol，但没有推荐 code_graph
"review" => {
    recommended: vec!["project_detect", "file_list", "file_read", "find_symbol"],
    // ❌ 缺少 code_graph
}
```

**问题**：
- LLM 会优先使用 `recommended` 列表中的工具
- `code_graph` 虽然在 `allowed` 中，但没有被推荐
- 导致 LLM 更倾向用 `grep` / `file_read` 而非 `code_graph`

---

### **问题 4：pre-turn 的 CODE_GRAPH_HINT 触发条件过于严格**

#### 代码分析

```rust
// crates/ox-cli/src/handlers/pre_turn.rs:310-344
async fn build_codegraph_hint(
    gitnexus: &Option<Arc<ox_core::mcp::GitNexusService>>,
    user_text: &str,
) -> Option<String> {
    let svc = gitnexus.as_ref()?;
    let q = user_text.trim();
    
    // ❌ 问题 1：太短的输入被跳过
    if q.chars().count() < 6 {
        return None;
    }
    
    // ❌ 问题 2：必须 is_ready() 且不能 is_dirty()
    if !svc.is_ready().await {
        return None;
    }
    if svc.is_dirty() {  // 有未提交改动 → 跳过
        return None;
    }
    
    // ❌ 问题 3：只有 6 秒超时
    let res = tokio::time::timeout(std::time::Duration::from_secs(6), svc.query(&params))
        .await
        .ok()?  // 超时 → 跳过
        .ok()?;
    // ...
}
```

**问题**：
- 当前项目有未提交改动（`is_dirty() = true`）→ **pre-turn hint 完全不生效**
- 用户输入太短（如 "主流程"）→ 跳过
- 超时 6 秒（GitNexus 慢时）→ 跳过

**结果**：大部分情况下，LLM 启动时**看不到代码图谱的预检索结果**。

---

## 🔧 修复方案

### **修复 1：强化 code_graph 使用指导**

在 `unified_action.rs` 中增强提示：

```rust
if spec.allowed.contains(&"code_graph") {
    out.push_str(
        "🕸 **code_graph** (GitNexus 代码图谱) — **优先使用**：\n\
         \n\
         **理解代码时必用**：\n\
         • op=query → 根据概念找执行流程（如"主流程"/"auth流程"）\n\
         • op=context → 查单个符号的 360° 视图（谁调谁、读写关系）\n\
         \n\
         **改动前必用**：\n\
         • op=impact → 改动爆炸半径分析（改 X 会影响哪些地方）\n\
         • op=detect_changes → 未提交改动的影响面\n\
         • op=api_impact → API 路由改动分析\n\
         \n\
         **比 grep/file_read 更强**：理解调用关系、执行流程、模块边界。\n\
         **默认策略**：先 code_graph query，再 file_read 深入。\n",
    );
}
```

---

### **修复 2：默认启用 find_symbol 增强**

修改配置默认值：

```rust
// crates/ox-core/src/config/mod.rs (GitNexusConfig)
pub struct GitNexusConfig {
    pub enabled: bool,
    pub augment_find_symbol: bool,  // 改为默认 true
    pub reindex_on_change: bool,
    // ...
}

impl Default for GitNexusConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            augment_find_symbol: true,  // ✅ 改为 true
            reindex_on_change: true,
            // ...
        }
    }
}
```

**同时在 config.toml 注释中说明**：
```toml
[gitnexus]
augment_find_symbol = true  # ✅ 推荐保持开启 — find_symbol 自动附加调用关系
```

---

### **修复 3：将 code_graph 加入推荐列表**

```rust
// crates/ox-core/src/agent/unified_action.rs
fn unified_route_spec(engine: &WorkflowEngine) -> RouteSpec {
    // ...
    match (phase, mode, has_findings) {
        // Review 阶段：推荐先用 code_graph 理解
        (SingleFlowPhase::Review | SingleFlowPhase::Receive, _, false) => RouteSpec {
            recommended: vec![
                "code_graph",     // ✅ 新增：优先查询代码图谱
                "find_symbol",
                "file_read",
            ],
            allowed: vec![
                "code_graph", "find_symbol", "file_read", "code_search",
                "file_list", "project_detect", "git_status", "finish",
            ],
            blocked: vec!["edit_file", "file_write", "shell_exec"],
            note: "审查(只读)：**先 code_graph 查执行流程**，再 find_symbol 定位，最后 file_read 细读。",
        },
        
        // Implement 阶段：改动前先 impact 分析
        (SingleFlowPhase::Implement, WorkspaceMode::ExecuteImpl, _) => RouteSpec {
            recommended: vec![
                "code_graph",     // ✅ 新增：改动前先分析影响面
                "find_symbol",
                "file_read",
                "edit_file",
            ],
            allowed: vec![
                "code_graph", "find_symbol", "file_read", "edit_file",
                "file_write", "shell_exec", "git_status", "finish",
            ],
            blocked: vec!["code_search", "file_list"],
            note: "实施：**改代码前先 code_graph op=impact 查影响面**；定位用 find_symbol，精读用 file_read。",
        },
        // ...
    }
}
```

---

### **修复 4：放宽 pre-turn hint 触发条件**

```rust
// crates/ox-cli/src/handlers/pre_turn.rs
async fn build_codegraph_hint(
    gitnexus: &Option<Arc<ox_core::mcp::GitNexusService>>,
    user_text: &str,
) -> Option<String> {
    let svc = gitnexus.as_ref()?;
    let q = user_text.trim();
    
    // ✅ 修复 1：降低长度限制
    if q.chars().count() < 4 {  // 6 → 4
        return None;
    }
    
    if !svc.is_ready().await {
        return None;
    }
    
    // ✅ 修复 2：dirty 时给出降级提示，而非完全跳过
    if svc.is_dirty() {
        return Some(
            "[CODE_GRAPH_HINT]\n\
             🔗 代码图谱可用但有未索引改动（可能不完全准确）。\n\
             💡 手动用 code_graph 查询会自动触发增量更新。".to_string()
        );
    }

    let mut params = ox_core::mcp::gitnexus::QueryParams::new(q);
    params.limit = Some(5);
    
    // ✅ 修复 3：增加超时到 10 秒
    let res = tokio::time::timeout(std::time::Duration::from_secs(10), svc.query(&params))
        .await
        .ok()?
        .ok()?;
    
    // ... 其余逻辑不变
}
```

---

## 📋 配置建议

### **立即生效的配置优化**

在 `~/.ox/config.toml` 中添加/修改：

```toml
[gitnexus]
enabled = true
augment_find_symbol = true          # ✅ 关键！find_symbol 自动附加调用关系
reindex_on_change = true            # code_graph 查询前自动增量更新
auto_index = true                   # 启动时自动索引（首次启动）
cli_timeout_ms = 120000             # 索引超时 2 分钟（大项目）
mcp_request_timeout_ms = 15000      # MCP 查询超时 15 秒
```

### **系统 Prompt 增强建议**

在 CLAUDE.md 中强化 GitNexus 使用指导：

```markdown
## Always Do

- **理解代码必先用 code_graph query**：不要盲目 grep/file_read，先用 `code_graph({op:"query", query:"主流程"})` 找执行流程
- **改动前必用 impact**：修改函数/类前，先 `code_graph({op:"impact", target:"函数名", direction:"upstream"})` 查影响面
- **find_symbol 会自动附加调用关系**：结果中包含 callers/callees，不需要再单独查
- **优先 code_graph，而非 grep**：GitNexus 理解代码语义、调用关系、执行流程，grep 只能做文本匹配
```

---

## 🎯 优先级修复顺序

### **P0 - 立即修复（用户可配置）**
1. ✅ **启用 `augment_find_symbol = true`** - 配置文件修改
2. ✅ **在 CLAUDE.md 中强化使用指导** - 文档修改

### **P1 - 代码修复（需要重新编译）**
3. 🔧 **修复 1：强化 code_graph 提示** - `unified_action.rs`
4. 🔧 **修复 3：code_graph 加入推荐列表** - `unified_action.rs`

### **P2 - 体验优化**
5. 🔧 **修复 2：默认启用 find_symbol 增强** - `config/mod.rs`
6. 🔧 **修复 4：放宽 pre-turn hint 条件** - `pre_turn.rs`

---

## 📊 预期效果对比

### **修复前**
```
用户: "帮我分析主流程"
↓
LLM: 
  1. grep "main" → 大量结果
  2. file_read main.rs → 读完一个文件
  3. 猜测流程 → 可能不准确
```

### **修复后**
```
用户: "帮我分析主流程"
↓
[CODE_GRAPH_HINT] 预检索：Process_text_input → ...（5 个执行流程）
↓
LLM:
  1. 看到 pre-turn hint，已经知道主要流程
  2. code_graph({op:"query", query:"主流程"}) → 300 个流程排序
  3. code_graph({op:"context", name:"process_text_input"}) → 完整调用关系
  4. file_read 关键文件 → 精确理解
  5. 输出结构化分析
```

---

## 🚀 下一步行动

### **立即执行（无需重编译）**

1. **修改配置文件** `~/.ox/config.toml`：
```bash
# 找到 [gitnexus] 部分，添加：
augment_find_symbol = true
```

2. **提交当前改动** （清除 dirty 状态）：
```bash
git add -A
git commit -m "fix: findings 清理 + GitNexus 集成分析"
```

3. **重新索引** GitNexus：
```bash
npx gitnexus analyze --force
```

4. **重启 Ox**，测试效果

---

### **代码修复（需要编译）**

运行以下命令应用修复：

```bash
# 1. 修复 unified_action.rs (强化提示 + 推荐列表)
# 2. 修复 pre_turn.rs (放宽 hint 条件)
# 3. 修复 config/mod.rs (默认启用增强)

cargo build --release
```

---

## 📝 总结

**核心问题**：GitNexus 集成完整，但 LLM **不知道何时用、怎么用**。

**关键修复**：
1. ✅ 强化 `code_graph` 使用指导（什么场景必用）
2. ✅ 将 `code_graph` 加入推荐列表（优先级提升）
3. ✅ 默认启用 `find_symbol` 增强（自动附加调用关系）
4. ✅ 放宽 pre-turn hint 条件（更多场景触发）

**预期收益**：
- LLM 会主动使用 `code_graph` 理解代码
- `find_symbol` 结果更丰富（带调用关系）
- pre-turn 预检索覆盖更多场景
- 整体理解代码的准确性 **大幅提升**

---

**诊断完成时间**：2026-06-30  
**诊断人员**：Claude (Kiro)
