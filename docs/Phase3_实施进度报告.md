# Phase 3: 统一意图识别系统 - 实施进度报告

**日期**: 2026-05-11  
**状态**: 🔄 部分完成（80%）  
**下一步**: 修复编译错误并完成集成

---

## ✅ 已完成的工作

### 1. 核心数据结构设计

**文件**: `crates/ox-core/src/tools/intent_classifier.rs`

✅ **统一的 IntentInfo 结构**：
```rust
pub struct IntentInfo {
    pub intent: QuestionType,
    pub confidence: f32,
    pub keywords: Vec<String>,
    pub suggested_tools: Vec<String>,
    
    // === 新增：记忆检索决策 ===
    pub should_search_memory: bool,
    pub memory_query: Option<String>,
    pub memory_scope: MemoryScope,
}
```

✅ **MemoryScope 枚举**：
```rust
pub enum MemoryScope {
    Project,  // 仅当前项目
    Global,   // 全局知识
    Both,     // 两者都搜索
}
```

✅ **QuestionType 增强**：
- 添加了 `Eq` 和 `Hash` trait（用于 HashMap 键）

---

### 2. 规则分类器实现

✅ **多语言动词映射表**：
- 支持中英日韩西法 6 种语言
- 覆盖 read/write/search/debug/refactor/explore 6 类动词

✅ **记忆检索决策逻辑**：
```rust
fn decide_memory_search(
    &self,
    user_message: &str,
    intent: &QuestionType,
    keywords: &[String],
) -> (bool, Option<String>, MemoryScope)
```

**决策规则**：
1. 用户明确提到历史内容（"之前"、"before"等）→ Project 范围
2. 涉及项目特定知识（架构、规范等）→ Project 范围
3. 复杂任务（CodeWriting/Refactoring + 多个关键词）→ Both 范围
4. Debugging 任务 → Both 范围（查找类似问题解决方案）

---

### 3. System Prompt 更新

**文件**: `crates/ox-core/src/context/system_prompt.rs`

✅ **添加意图分类指令**：
- 要求 LLM 输出完整的意图 JSON 块
- 包含 6 个字段：intent, confidence, keywords, suggested_tools, should_search_memory, memory_query, memory_scope
- 提供 3 个详细示例

⚠️ **待修复**：反引号导致的编译错误（需要将 ```json 改为 ```text）

---

### 4. 工具推荐逻辑优化

✅ **统一添加 memory_search**：
- 所有意图类型都会在推荐工具列表中包含 `memory_search`
- LLM 可以根据需要决定是否使用

---

### 5. 测试用例

✅ **新增 4 个测试用例**：
1. `test_memory_search_with_history_reference` - 历史引用场景
2. `test_memory_search_with_architecture_mention` - 架构查询场景
3. `test_no_memory_search_for_simple_task` - 简单任务场景
4. `test_memory_search_for_debugging` - Debug 场景

---

## ⚠️ 待完成的工作

### 1. 修复编译错误（紧急）

**问题 1**: System Prompt 中的反引号导致 Rust 编译错误

**位置**: `crates/ox-core/src/context/system_prompt.rs`

**错误信息**：
```
error: prefix `status` is unknown
error: prefix `patterns` is unknown
```

**原因**: Rust 字符串字面量中的反引号会被解析为宏调用

**解决方案**：
将所有 ```json 改为 ```text，或者转义反引号

**待修复的行**：
- Line 227: Git operations 示例
- Line 337-346: Output Format 示例
- Line 376-385: Example 1
- Line 395-404: Example 2
- Line 414-423: Example 3

---

**问题 2**: Module 导入问题

**错误信息**：
```
error[E0432]: unresolved import `crate::tools::intent_classifier::MemoryScope`
```

**可能原因**: 测试模块中的导入路径问题

**解决方案**: 检查并修正测试模块的导入语句

---

### 2. 集成到 Agent 主流程（重要）

**文件**: `crates/ox-core/src/agent/mod.rs`

**待实现**：

```rust
impl Agent {
    async fn process_user_message(&mut self, user_message: String) -> Result<()> {
        // 1. 调用 LLM
        let response = self.llm_client.chat(...).await?;
        
        // 2. 提取意图（Free Mode）
        if self.mode == AgentMode::Free {
            if let Some(intent) = extract_intent_from_llm_response(&response) {
                self.current_intent = Some(intent.clone());
                
                // 3. 如果需要检索记忆
                if intent.should_search_memory {
                    if let Some(query) = &intent.memory_query {
                        let memory_results = self.memory_system.search(
                            query,
                            &self.runtime_env.project_id,
                            5
                        ).await?;
                        
                        self.context.add_memory_context(&memory_results);
                    }
                }
                
                // 4. 基于意图过滤工具
                let filtered_tools = self.tool_filter.filter_by_intent(
                    &self.tool_registry.get_all_tools(),
                    &intent
                );
                
                // 5. 清理响应中的 JSON 块
                let clean_response = remove_intent_json(&response);
                
                // ... 继续处理
            }
        }
        
        Ok(())
    }
}
```

---

### 3. 创建 SmartToolFilter

**文件**: `crates/ox-core/src/tools/filter.rs`（新建）

**待实现**：

```rust
pub struct SmartToolFilter {
    rule_classifier: RuleBasedClassifier,
}

impl SmartToolFilter {
    pub fn filter_by_intent(
        &self,
        all_tools: &[&dyn Tool],
        intent: &IntentInfo,
    ) -> Vec<&dyn Tool> {
        // 为每个工具计算分数
        let mut scored_tools: Vec<(&dyn Tool, u32)> = all_tools
            .iter()
            .map(|tool| {
                let score = self.calculate_score(tool, intent);
                (*tool, score)
            })
            .collect();
        
        // 排序并取 Top 7
        scored_tools.sort_by_key(|(_, score)| std::cmp::Reverse(*score));
        scored_tools.truncate(7);
        
        scored_tools.into_iter().map(|(tool, _)| tool).collect()
    }
    
    fn calculate_score(&self, tool: &dyn Tool, intent: &IntentInfo) -> u32 {
        // 1. 意图类型匹配（40%）
        // 2. 关键词匹配（30%）
        // 3. LLM 推荐加分（20%）
        // 4. 最近使用连续性（10%）
    }
}
```

---

### 4. 更新 mod.rs 导出

**文件**: `crates/ox-core/src/tools/mod.rs`

**待添加**：
```rust
pub use intent_classifier::{IntentInfo, QuestionType, MemoryScope, RuleBasedClassifier};
```

---

## 📊 当前状态总结

| 模块 | 完成度 | 状态 |
|------|--------|------|
| 数据结构设计 | 100% | ✅ 完成 |
| 规则分类器 | 100% | ✅ 完成 |
| System Prompt | 90% | ⚠️ 待修复编译错误 |
| 测试用例 | 100% | ✅ 完成 |
| Agent 集成 | 0% | ❌ 待实施 |
| SmartToolFilter | 0% | ❌ 待实施 |
| 模块导出 | 0% | ❌ 待实施 |

**总体进度**: 80%（核心逻辑完成，集成待完成）

---

## 🎯 下一步行动

### 立即执行（今天）

1. **修复编译错误**
   - 修正 system_prompt.rs 中的反引号
   - 验证 `cargo build --package ox-core` 通过

2. **运行测试**
   - `cargo test --package ox-core intent_classifier`
   - 确保所有测试通过

### 短期计划（1-2天）

3. **创建 SmartToolFilter**
   - 实现工具评分算法
   - 集成到工具系统

4. **集成到 Agent**
   - 修改 `agent/mod.rs`
   - 添加工具过滤逻辑
   - 实现记忆检索触发

### 中期计划（3-5天）

5. **实际场景测试**
   - 在 Free Mode 下测试意图识别准确性
   - 收集用户反馈
   - 调整评分权重

6. **性能优化**
   - 确保过滤算法在毫秒级完成
   - 优化关键词提取

---

## 💡 关键设计决策

### 1. 统一意图识别

**决策**: 将工具过滤和记忆检索决策合并到一个 IntentInfo 结构

**理由**:
- 避免重复分类
- 保证一致性（如果需要记忆检索，memory_search 应该在工具列表中）
- 简化 LLM 输出（一个 JSON 块服务多个目的）

### 2. 三层混合策略

**决策**: LLM 提取 → 规则匹配 → 默认回退

**理由**:
- 80% 场景零成本（LLM 已分类）
- 15% 场景低成本（规则匹配）
- 5% 场景兜底（默认工具集）
- 平衡准确性和性能

### 3. 始终包含 memory_search

**决策**: 在所有意图的工具推荐中都包含 memory_search

**理由**:
- LLM 可以自主决定是否需要检索记忆
- 避免误判导致无法访问记忆系统
- memory_search 是 Safe 工具，无副作用

---

## 📝 技术债务

1. **多语言动词映射不完整**
   - 当前只支持 6 种语言
   - 需要根据实际使用情况扩展

2. **关键词提取过于简单**
   - 当前只是简单的分词
   - 未来可以引入 NLP 库进行更智能的提取

3. **评分权重未调优**
   - 当前的 40%/30%/20%/10% 是经验值
   - 需要实际数据来优化

---

**文档版本**: 0.8  
**最后更新**: 2026-05-11  
**维护者**: Ox Team
