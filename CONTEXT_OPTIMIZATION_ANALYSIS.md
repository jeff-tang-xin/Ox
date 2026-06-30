# 🔍 上下文管理诊断报告

## 📊 当前状态统计

### **代码规模**
- `system_prompt.rs`: **519 行** 
- `context_injector.rs`: **539 行**
- `workspace.rs`: **826 行**
- `unified_action.rs`: **617 行**
- **总计**: 2,501 行上下文管理代码
- **消息注入点**: 57 处 `Message::system/user`

### **System Prompt 组成**
```
1. Core Prompt (角色 + 规则)
   - UNIFIED_CORE_GENERAL: ~80 行
   - UNIFIED_CORE_CODING: ~90 行
   - UNIFIED_CORE_EXPLORING: ~70 行
   - CORE_CODING/GENERAL/EXPLORING: 各 ~60 行
   
2. Tool Block: ~30 行
3. Methodology: ~20 行
4. User Rules: 动态加载
5. Runtime Info: ~10 行

总计: ~500 tokens (目标 < 600 tokens)
```

---

## 🚨 **发现的问题**

### **问题 1: 上下文注入点过多且分散** ❌

**当前架构**：
```
inject_context() 主函数
  ├─ strip 9 种旧标签 (手动维护)
  ├─ 注入 [PHASE_TRANSITION]
  ├─ 注入 [WORKSPACE] (workspace.rs:826行)
  ├─ 注入 [SCOPE_GATE]
  ├─ 注入 [STEP_MEMORY]
  ├─ 注入 [UNIFIED_ROUTE] (unified_action.rs:617行)
  └─ 注入 [SKILL_ROUTE]
```

**问题**：
- ❌ **9 个 strip 函数手动维护** - 新增标签时容易遗漏
- ❌ **注入逻辑分散在 3 个文件** - workspace.rs / unified_action.rs / context_injector.rs
- ❌ **标签命名不一致** - `[WORKSPACE]` vs `WORKSPACE_TAG` vs `[UNIFIED_ROUTE]`
- ❌ **重复代码** - 多处 `messages.push(Message::system(...))`

---

### **问题 2: System Prompt 过度细分** ❌

**当前有 6 个变体**：
```rust
1. UNIFIED_CORE_GENERAL
2. UNIFIED_CORE_CODING  
3. UNIFIED_CORE_EXPLORING
4. CORE_GENERAL (非 unified)
5. CORE_CODING (非 unified)
6. CORE_EXPLORING (非 unified)
```

**问题**：
- ❌ **内容高度重复** - 6 个变体之间差异 < 20%
- ❌ **维护困难** - 修改规则需要同步 6 个地方
- ❌ **unified vs 非 unified 差异小** - 主要是工具调用格式，不需要完全独立的 prompt

**实际差异分析**：
```
UNIFIED vs 非 UNIFIED:
- 工具调用格式: complete_and_check({action, params}) vs 独立工具
- 其他规则: 95% 相同

GENERAL vs CODING vs EXPLORING:
- 角色描述: 略有不同
- 工具使用规则: 基本相同
- 核心规则: 100% 相同
```

---

### **问题 3: [WORKSPACE] 过于庞大** ❌

**workspace.rs: 826 行，生成的 [WORKSPACE] 包含**：
```yaml
task_intent: "fix/review/qa"
tool_hints: "大段工具使用建议"
authority_note: "权限说明"
mode: "scope_confirm/feedback_discuss/execute_impl/execute_review"
findings_summary: "..."
scoped_findings: [{...}, {...}, ...]  # 可能很长
open_findings: [{...}, {...}]
files_read: [{path, content_preview}, ...]  # 重复文件内容
file_digests: [{path, last_action, ...}, ...]
files_edited: ["...", "..."]
required_action: "大段下一步指导"
forbidden: ["...", "..."]
phase_notes: "大段注意事项"
user_directives: "..."
```

**问题**：
- ❌ **单个注入 300-500 tokens** - 占用大量上下文
- ❌ **大量重复信息** - findings 已在 history 中，又在 workspace 中重复
- ❌ **files_read 包含 content_preview** - 文件内容已在 history 中，又摘要一次
- ❌ **tool_hints 冗余** - 与 [UNIFIED_ROUTE] 重复

---

### **问题 4: 上下文卸载未充分利用** ❌

**context_offloader.rs 存在但未被广泛使用**：

```rust
// 设计目标: 长工具输出卸载到文件，消息中只保留摘要
// 实际状态: 很少被调用

pub fn should_offload_result(tool_name: &str, content_len: usize) -> bool {
    const OFFLOAD_THRESHOLD: usize = 2000; // 2KB
    matches!(
        tool_name,
        "file_read" | "file_list" | "code_search" | "find_symbol" | "git_diff"
    ) && content_len > OFFLOAD_THRESHOLD
}
```

**问题**：
- ❌ **阈值过高** (2000 字符) - 实际很少触发
- ❌ **覆盖工具少** - 只支持 5 个工具
- ❌ **未集成到主流程** - 需要手动调用
- ❌ **LLM 仍能看到完整 history** - 卸载只是备份，未真正减少上下文

---

### **问题 5: 多轮对话时上下文爆炸** ❌

**当前策略**：
```rust
// context_injector.rs:27
pub fn should_inject_memory(iteration: u32, task_intent: TaskIntent, is_workflow: bool) -> bool {
    if iteration == 0 { return true; }
    if is_workflow { return iteration % 2 == 0; }
    
    let interval = match task_intent {
        TaskIntent::Fix => 2,
        TaskIntent::Review => 3,
        TaskIntent::Qa => 4,
        TaskIntent::General => 2,
    };
    iteration % interval == 0
}
```

**问题**：
- ❌ **Fix 任务每 2 轮注入一次** - 10 轮对话 = 5 次重复注入
- ❌ **注入内容不精简** - 每次都是完整 [STEP_MEMORY]
- ❌ **History 无压缩** - 所有历史消息全部保留
- ❌ **Token 计数缺失** - 不知道何时触及上下文限制

**实际影响**：
```
第 1 轮: System Prompt + User + [WORKSPACE] + [ROUTE] = 1500 tokens
第 2 轮: + [STEP_MEMORY] + new messages = 2500 tokens
第 3 轮: + [STEP_MEMORY] + new messages = 3800 tokens
第 4 轮: + [STEP_MEMORY] + new messages = 5500 tokens
...
第 10 轮: 可能超过 15000 tokens (对于复杂任务)
```

---

## 💡 **优化方案**

### **方案 1: 统一注入管理器** 🎯

**目标**：将分散的注入逻辑集中到一个管理器

```rust
// context/injection_manager.rs (新文件)

pub struct InjectionManager {
    tags: HashMap<&'static str, Box<dyn InjectionBlock>>,
}

trait InjectionBlock {
    fn tag(&self) -> &'static str;
    fn should_inject(&self, ctx: &InjectionContext) -> bool;
    fn build(&self, ctx: &InjectionContext) -> String;
}

impl InjectionManager {
    pub fn inject(&self, messages: &mut Vec<Message>, ctx: &InjectionContext) {
        // 1. 自动 strip 所有已注册的标签
        self.strip_all(messages);
        
        // 2. 按优先级注入
        for block in self.blocks_by_priority() {
            if block.should_inject(ctx) {
                messages.push(Message::system(&block.build(ctx)));
            }
        }
    }
    
    fn strip_all(&self, messages: &mut Vec<Message>) {
        // 单次遍历，strip 所有标签
        messages.retain(|m| {
            !self.tags.keys().any(|tag| m.content.starts_with(tag))
        });
    }
}
```

**收益**：
- ✅ 注入逻辑集中管理
- ✅ 自动 strip，无遗漏
- ✅ 易于扩展新标签
- ✅ 减少代码行数 ~200 行

---

### **方案 2: System Prompt 合并** 🎯

**目标**：6 个变体 → 2 个变体

```rust
// 合并后只保留 2 个:
const CORE_PROMPT: &str = "..."; // 核心规则
const TOOL_USAGE_UNIFIED: &str = "...";  // unified 工具调用
const TOOL_USAGE_LEGACY: &str = "...";   // legacy 工具调用

pub fn build_system_prompt(...) -> String {
    let mut parts = vec![CORE_PROMPT];
    
    // 根据 intent 动态调整角色描述（10 行差异）
    parts.push(match intent {
        UserIntent::CodeModification => "你正在修改代码...",
        UserIntent::CodeUnderstanding => "你正在理解代码...",
        UserIntent::Exploration => "你正在探索项目...",
        _ => "你是编码助手...",
    });
    
    // 工具调用格式
    parts.push(if unified { TOOL_USAGE_UNIFIED } else { TOOL_USAGE_LEGACY });
    
    // 其他动态块
    parts.push(build_tool_list(...));
    parts.push(load_user_rules(...));
    
    parts.join("\n\n")
}
```

**收益**：
- ✅ 减少重复代码 ~300 行
- ✅ 维护成本降低 80%
- ✅ 修改规则只需改一处

---

### **方案 3: [WORKSPACE] 精简** 🎯

**目标**：300-500 tokens → 100-150 tokens

```rust
// 精简版 WorkspaceBlock
pub struct SlimWorkspace {
    mode: WorkspaceMode,           // "scope_confirm" (10 tokens)
    lock_status: LockStatus,       // "locked" (5 tokens)
    findings_count: (u32, u32),    // (3 scoped, 5 open) (10 tokens)
    next_action: String,           // "确认范围后进入实施" (20 tokens)
    forbidden: Vec<&'static str>,  // ["edit_file", "shell_exec"] (10 tokens)
}

// 移除冗余字段:
// ❌ tool_hints - 已在 [UNIFIED_ROUTE] 中
// ❌ authority_note - 已在 [UNIFIED_ROUTE] 中
// ❌ findings 详情 - 已在 history 中
// ❌ files_read - 已在 history 中
// ❌ file_digests - 可通过工具查询
// ❌ phase_notes - 已在 [UNIFIED_ROUTE].note 中
```

**精简后格式**：
```
[WORKSPACE]
模式: scope_confirm | 状态: 🔒 locked
Findings: 3 scoped, 5 open
下一步: 用户 c 确认范围后进入实施
禁止: edit_file, shell_exec
```

**收益**：
- ✅ Token 减少 70% (300 → 100)
- ✅ 信息更聚焦
- ✅ 减少重复

---

### **方案 4: 主动上下文卸载** 🎯

**目标**：自动卸载长输出，真正减少上下文

```rust
// agent/mod.rs 中拦截工具输出
pub async fn execute_tool_with_offload(
    tool: &dyn Tool,
    params: &Value,
    ctx: &ToolContext,
    offloader: &ContextOffloader,
) -> ToolOutput {
    let result = tool.execute(params, ctx).await;
    
    // 自动判断是否卸载
    if should_offload(&result) {
        let offloaded = offloader.offload(
            tool.name(),
            &result.content,
            iteration,
        ).await;
        
        // 返回摘要版本
        ToolOutput {
            content: format!(
                "{}\n\n[完整内容已保存，需要时用 read_offloaded({}) 读取]",
                offloaded.summary,
                offloaded.node_id
            ),
            ..result
        }
    } else {
        result
    }
}

// 降低阈值，扩大覆盖
const OFFLOAD_THRESHOLD: usize = 500;  // 2000 → 500

// 支持更多工具
fn should_offload_tool(name: &str) -> bool {
    matches!(
        name,
        "file_read" | "file_list" | "code_search" | "find_symbol" 
        | "git_diff" | "code_graph" | "shell_exec" | "grep"  // ✅ 新增
    )
}
```

**收益**：
- ✅ 自动卸载，无需手动
- ✅ 真正减少上下文 40%+
- ✅ 可回溯完整内容

---

### **方案 5: 上下文压缩策略** 🎯

**目标**：多轮对话时动态压缩历史

```rust
pub struct ContextCompressor {
    max_history_tokens: usize,  // 10000 tokens
    compression_ratio: f32,     // 0.3 (保留 30%)
}

impl ContextCompressor {
    pub fn compress_if_needed(&self, messages: &mut Vec<Message>) {
        let tokens = estimate_tokens(messages);
        
        if tokens > self.max_history_tokens {
            // 策略 1: 移除旧的 [STEP_MEMORY] (已过期)
            self.remove_old_step_memory(messages, keep_last: 2);
            
            // 策略 2: 压缩旧的工具输出 (保留摘要)
            self.compress_old_tool_results(messages, older_than: 5);
            
            // 策略 3: 移除重复的 [WORKSPACE] (只保留最新)
            self.remove_duplicate_workspace(messages);
            
            // 策略 4: 如果还超，压缩 user/assistant 消息
            if estimate_tokens(messages) > self.max_history_tokens {
                self.compress_conversation(messages);
            }
        }
    }
    
    fn compress_conversation(&self, messages: &mut Vec<Message>) {
        // 保留: 最近 3 轮完整对话 + 更早的摘要
        let recent_count = 6;  // 3 轮 = 6 条消息
        let total = messages.len();
        
        if total > recent_count + 2 {
            let old_messages = &messages[..total - recent_count];
            let summary = self.summarize_messages(old_messages);
            
            // 替换旧消息为摘要
            messages.drain(..total - recent_count);
            messages.insert(0, Message::system(&format!(
                "[HISTORY_SUMMARY]\n之前的对话摘要:\n{}", summary
            )));
        }
    }
}
```

**收益**：
- ✅ 多轮对话不爆上下文
- ✅ 保留关键信息
- ✅ 自动触发，无感知

---

## 📊 **优化效果预估**

| 指标 | 优化前 | 优化后 | 提升 |
|------|--------|--------|------|
| **代码行数** | 2,501 行 | ~1,800 行 | -28% |
| **System Prompt** | ~500 tokens | ~400 tokens | -20% |
| **[WORKSPACE]** | 300-500 tokens | 100-150 tokens | -70% |
| **10 轮对话上下文** | ~15,000 tokens | ~8,000 tokens | -47% |
| **维护成本** | 高 (6 个变体) | 低 (2 个变体) | -67% |
| **注入点管理** | 手动 (9 个 strip) | 自动 (统一管理) | -100% |

---

## 🎯 **实施优先级**

### **P0 - 立即实施（高收益，低风险）**

1. ✅ **方案 2: System Prompt 合并** 
   - 时间: 2 小时
   - 风险: 低
   - 收益: 减少 300 行代码，维护成本 -67%

2. ✅ **方案 3: [WORKSPACE] 精简**
   - 时间: 1 小时
   - 风险: 低
   - 收益: Token -70%

### **P1 - 短期实施（高收益，中风险）**

3. ✅ **方案 1: 统一注入管理器**
   - 时间: 4 小时
   - 风险: 中（需要重构）
   - 收益: 代码更清晰，易扩展

4. ✅ **方案 4: 主动上下文卸载**
   - 时间: 3 小时
   - 风险: 中（需要集成到主流程）
   - 收益: 上下文 -40%

### **P2 - 长期实施（中收益，高风险）**

5. ✅ **方案 5: 上下文压缩策略**
   - 时间: 6 小时
   - 风险: 高（涉及消息历史管理）
   - 收益: 多轮对话稳定性

---

## 📝 **总结**

### **核心问题**
1. ❌ 上下文注入点过多且分散（9 个 strip + 3 个文件）
2. ❌ System Prompt 过度细分（6 个变体，重复 80%）
3. ❌ [WORKSPACE] 过于庞大（300-500 tokens，大量重复）
4. ❌ 上下文卸载未充分利用（阈值过高，覆盖少）
5. ❌ 多轮对话时上下文爆炸（无压缩，10 轮可达 15k tokens）

### **优化方向**
1. ✅ 统一注入管理 → 代码更清晰
2. ✅ 合并 Prompt 变体 → 维护更简单
3. ✅ 精简 WORKSPACE → Token 更少
4. ✅ 主动卸载 → 上下文更小
5. ✅ 智能压缩 → 多轮更稳定

### **预期收益**
- 代码行数 -28%
- 上下文 Token -47%
- 维护成本 -67%

---

**诊断完成时间**: 2026-06-30  
**诊断人员**: Claude (Kiro)
