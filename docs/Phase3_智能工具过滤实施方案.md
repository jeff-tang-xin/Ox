# Phase 3: 智能工具过滤实施方案

**日期**: 2026-05-11  
**状态**: 📋 设计完成，待实施  
**目标**: 基于工作流步骤和意图分类，动态过滤工具列表

---

## 🎯 核心设计

### 分层过滤策略

```
┌─────────────────────────────────────────┐
│   Spec/Council Mode (有工作流)           │
│   → 基于工作流步骤严格限制可用工具        │
└─────────────────────────────────────────┘
                    ↓
┌─────────────────────────────────────────┐
│   Free Mode (自由模式)                   │
│   → 基于意图分类动态评分和过滤            │
│     ├─ Layer 1: LLM 响应提取（零成本）    │
│     ├─ Layer 2: 规则匹配（低成本）        │
│     └─ Layer 3: 小模型兜底（可选）        │
└─────────────────────────────────────────┘
```

---

## 📊 实现方案

### 1. 工作流模式的工具过滤

**原理**：每个工作流步骤明确定义允许使用的工具

```rust
/// 工作流步骤定义
pub struct WorkflowStep {
    pub id: String,
    pub name: String,
    pub description: String,
    
    /// ✅ 该步骤允许使用的工具白名单
    pub allowed_tools: Vec<String>,
    
    /// 该步骤的预期输出
    pub expected_output: String,
    
    /// 是否需要用户确认
    pub requires_confirmation: bool,
}

/// 示例：Spec Mode 的工作流定义
impl WorkflowDefinition {
    pub fn spec_mode_workflow() -> Self {
        Self {
            steps: vec![
                WorkflowStep {
                    id: "requirements".to_string(),
                    name: "需求分析".to_string(),
                    description: "理解用户需求，分析功能点".to_string(),
                    allowed_tools: vec![
                        "memory_search".to_string(),  // 查找相关知识
                        "file_read".to_string(),       // 查看现有代码
                        "code_search".to_string(),     // 搜索相关实现
                    ],
                    expected_output: "需求分析报告".to_string(),
                    requires_confirmation: true,
                },
                WorkflowStep {
                    id: "design".to_string(),
                    name: "方案设计".to_string(),
                    description: "设计技术方案，确定实现思路".to_string(),
                    allowed_tools: vec![
                        "file_read".to_string(),
                        "code_search".to_string(),
                        "memory_search".to_string(),
                        "web_fetch".to_string(),      // 查阅外部文档
                    ],
                    expected_output: "技术设计方案".to_string(),
                    requires_confirmation: true,
                },
                WorkflowStep {
                    id: "implementation".to_string(),
                    name: "编码实现".to_string(),
                    description: "根据设计方案实现代码".to_string(),
                    allowed_tools: vec![
                        "file_read".to_string(),
                        "file_write".to_string(),
                        "file_patch".to_string(),
                        "shell_exec".to_string(),     // 运行测试
                    ],
                    expected_output: "实现的功能代码".to_string(),
                    requires_confirmation: true,
                },
                WorkflowStep {
                    id: "testing".to_string(),
                    name: "测试验证".to_string(),
                    description: "测试功能是否正常工作".to_string(),
                    allowed_tools: vec![
                        "shell_exec".to_string(),     // 运行测试
                        "file_read".to_string(),       // 查看测试结果
                        "file_patch".to_string(),     // 修复问题
                    ],
                    expected_output: "测试通过的功能".to_string(),
                    requires_confirmation: false,
                },
            ],
        }
    }
}
```

**过滤逻辑**：

```rust
impl SmartToolFilter {
    /// 工作流模式：基于步骤白名单过滤
    pub fn filter_by_workflow_step(
        &self,
        all_tools: &[&dyn Tool],
        current_step: &WorkflowStep,
    ) -> Vec<&dyn Tool> {
        let allowed_names: HashSet<&str> = current_step
            .allowed_tools
            .iter()
            .map(|s| s.as_str())
            .collect();
        
        all_tools
            .iter()
            .filter(|tool| allowed_names.contains(tool.name()))
            .copied()
            .collect()
    }
}
```

---

### 2. 自由模式的工具过滤

**原理**：基于意图分类和上下文，动态计算工具相关性分数

```rust
impl SmartToolFilter {
    /// 自由模式：基于意图的动态过滤
    pub fn filter_for_free_mode(
        &self,
        all_tools: &[&dyn Tool],
        user_message: &str,
        context: &FilterContext,
    ) -> Vec<&dyn Tool> {
        // 1. 尝试从 LLM 响应中提取意图（零成本）
        if let Some(llm_response) = context.llm_response {
            if let Some(intent) = extract_intent_from_llm_response(llm_response) {
                if intent.confidence > 0.8 {
                    return self.score_and_filter(all_tools, &intent, context);
                }
            }
        }
        
        // 2. 使用规则分类器（低成本）
        let rule_intent = self.rule_classifier.classify(user_message);
        if rule_intent.confidence > 0.7 {
            return self.score_and_filter(all_tools, &rule_intent, context);
        }
        
        // 3. 如果置信度都很低，返回默认工具集
        self.get_default_tools(all_tools)
    }
    
    /// 基于意图评分并过滤
    fn score_and_filter(
        &self,
        all_tools: &[&dyn Tool],
        intent: &IntentInfo,
        context: &FilterContext,
    ) -> Vec<&dyn Tool> {
        // 为每个工具计算分数
        let mut scored_tools: Vec<(&dyn Tool, u32)> = all_tools
            .iter()
            .map(|tool| {
                let score = self.calculate_tool_score(tool, intent, context);
                (*tool, score)
            })
            .collect();
        
        // 按分数排序
        scored_tools.sort_by_key(|(_, score)| std::cmp::Reverse(*score));
        
        // 取前 N 个工具（默认 7 个）
        let max_tools = context.max_tools.unwrap_or(7);
        scored_tools.truncate(max_tools);
        
        tracing::info!(
            "Filtered tools for intent {:?}: {:?}",
            intent.intent,
            scored_tools.iter().map(|(t, _)| t.name()).collect::<Vec<_>>()
        );
        
        scored_tools.into_iter().map(|(tool, _)| tool).collect()
    }
    
    /// 计算工具的综合分数
    fn calculate_tool_score(
        &self,
        tool: &dyn Tool,
        intent: &IntentInfo,
        context: &FilterContext,
    ) -> u32 {
        let mut score = 0;
        
        // 1. 意图类型匹配（40% 权重）
        score += self.intent_type_match(tool, &intent.intent) * 40 / 100;
        
        // 2. 关键词匹配（30% 权重）
        score += self.keyword_relevance(tool, &intent.keywords) * 30 / 100;
        
        // 3. LLM 推荐加分（20% 权重）
        if intent.suggested_tools.contains(&tool.name().to_string()) {
            score += 20;
        }
        
        // 4. 最近使用连续性（10% 权重）
        score += self.recency_bonus(tool, &context.recent_tools) * 10 / 100;
        
        score
    }
    
    /// 意图类型匹配分数
    fn intent_type_match(&self, tool: &dyn Tool, intent: &QuestionType) -> u32 {
        match intent {
            QuestionType::CodeReading => {
                match tool.name() {
                    "file_read" => 100,
                    "code_search" => 90,
                    "file_list" => 70,
                    "memory_search" => 60,
                    _ => 30,
                }
            }
            QuestionType::CodeWriting => {
                match tool.name() {
                    "file_write" => 100,
                    "file_patch" => 95,
                    "file_read" => 80,  // 写之前通常要读
                    "project_detect" => 50,
                    _ => 30,
                }
            }
            QuestionType::Debugging => {
                match tool.name() {
                    "file_read" => 90,
                    "code_search" => 85,
                    "shell_exec" => 80,  // 运行测试
                    "memory_search" => 70,  // 查找类似问题
                    "file_patch" => 75,
                    _ => 30,
                }
            }
            QuestionType::Refactoring => {
                match tool.name() {
                    "file_read" => 90,
                    "code_search" => 85,
                    "file_patch" => 80,
                    "memory_search" => 60,
                    _ => 30,
                }
            }
            QuestionType::Exploration => {
                match tool.name() {
                    "file_list" => 100,
                    "project_detect" => 95,
                    "file_search" => 85,
                    "code_search" => 75,
                    "memory_search" => 60,
                    _ => 30,
                }
            }
            QuestionType::GeneralQuestion => {
                match tool.name() {
                    "memory_search" => 70,
                    "web_fetch" => 65,
                    "file_read" => 50,
                    _ => 30,
                }
            }
        }
    }
    
    /// 关键词相关性
    fn keyword_relevance(&self, tool: &dyn Tool, keywords: &[String]) -> u32 {
        let tool_name = tool.name().to_lowercase();
        let tool_desc = tool.description().to_lowercase();
        
        let mut matches = 0;
        
        for keyword in keywords {
            let kw_lower = keyword.to_lowercase();
            
            // 检查关键词是否出现在工具名称或描述中
            if tool_name.contains(&kw_lower) || tool_desc.contains(&kw_lower) {
                matches += 1;
            }
            
            // 特殊处理：项目/结构相关关键词
            if kw_lower.contains("项目") || kw_lower.contains("project") 
                || kw_lower.contains("结构") || kw_lower.contains("structure") {
                if tool_name == "file_list" || tool_name == "project_detect" {
                    matches += 2;
                }
            }
            
            // 文档相关关键词
            if kw_lower.contains("文档") || kw_lower.contains("doc")
                || kw_lower.contains("报告") || kw_lower.contains("report") {
                if tool_name == "file_write" {
                    matches += 2;
                }
            }
        }
        
        (matches * 20).min(100)
    }
    
    /// 最近使用工具的连续性加分
    fn recency_bonus(&self, tool: &dyn Tool, recent_tools: &[String]) -> u32 {
        let tool_name = tool.name();
        
        for (i, recent) in recent_tools.iter().enumerate() {
            if recent == tool_name {
                // 越近的工具分数越高
                return match i {
                    0 => 100,  // 刚刚用过
                    1 => 70,
                    2 => 40,
                    _ => 20,
                };
            }
        }
        
        0
    }
    
    /// 获取默认工具集（当无法确定意图时）
    fn get_default_tools<'a>(&self, all_tools: &'a [&dyn Tool]) -> Vec<&'a dyn Tool> {
        let default_names = [
            "file_read",
            "file_write",
            "file_patch",
            "code_search",
            "file_list",
            "memory_search",
            "shell_exec",
        ];
        
        all_tools
            .iter()
            .filter(|tool| default_names.contains(&tool.name()))
            .copied()
            .collect()
    }
}
```

---

## 🔧 数据结构定义

```rust
/// 过滤上下文
pub struct FilterContext {
    /// Agent 模式
    pub mode: AgentMode,
    
    /// 当前工作流步骤（仅 Spec/Council Mode）
    pub workflow_step: Option<&WorkflowStep>,
    
    /// LLM 响应内容（用于提取意图）
    pub llm_response: Option<String>,
    
    /// 用户消息
    pub user_message: String,
    
    /// 最近使用的工具列表（LIFO）
    pub recent_tools: Vec<String>,
    
    /// 最大返回工具数
    pub max_tools: Option<usize>,
    
    /// 项目上下文信息
    pub project_context: ProjectContext,
}

/// Agent 模式
#[derive(Debug, Clone, PartialEq)]
pub enum AgentMode {
    Spec,
    Council,
    Free,
}

/// 项目上下文
pub struct ProjectContext {
    pub project_type: Option<String>,
    pub language: Option<String>,
    pub framework: Option<String>,
}
```

---

## 📈 集成到 Agent 主流程

### 修改 `agent/mod.rs`

```rust
use crate::tools::{SmartToolFilter, FilterContext};

pub struct Agent {
    // ... existing fields ...
    
    /// 智能工具过滤器
    tool_filter: SmartToolFilter,
    
    /// 当前意图信息
    pub current_intent: Option<IntentInfo>,
    
    /// 最近使用的工具列表
    recent_tools: VecDeque<String>,
}

impl Agent {
    pub fn new(...) -> Self {
        Self {
            // ... existing initialization ...
            tool_filter: SmartToolFilter::new(),
            current_intent: None,
            recent_tools: VecDeque::with_capacity(10),
        }
    }
    
    async fn process_user_message(&mut self, user_message: String) -> Result<()> {
        // 1. 确定当前模式
        let mode = self.current_mode();
        
        // 2. 构建过滤上下文
        let filter_context = FilterContext {
            mode: mode.clone(),
            workflow_step: self.current_workflow_step.as_ref(),
            llm_response: None,  // 稍后填充
            user_message: user_message.clone(),
            recent_tools: self.recent_tools.iter().cloned().collect(),
            max_tools: Some(7),
            project_context: self.project_context.clone(),
        };
        
        // 3. 过滤工具
        let filtered_tools = self.tool_filter.filter(
            &self.tool_registry.get_all_tools(),
            &filter_context,
        );
        
        // 4. 构建 system prompt（包含意图分类指令，如果是 Free Mode）
        let system_prompt = build_system_prompt(
            &self.runtime_env,
            &filtered_tools,  // ← 传入过滤后的工具
            self.persona.as_deref(),
            self.behavior_rules.as_ref(),
            self.spec_content.as_deref(),
        );
        
        // 5. 调用 LLM
        let mut full_response = String::new();
        let mut stream = self.llm_client.chat_stream(system_prompt, user_message).await?;
        
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            full_response.push_str(&chunk);
            // ... UI updates ...
        }
        
        // 6. 如果是 Free Mode，提取意图信息
        if mode == AgentMode::Free {
            if let Some(intent) = extract_intent_from_llm_response(&full_response) {
                self.current_intent = Some(intent.clone());
                
                // 清理响应中的 JSON 块
                let clean_response = remove_intent_json(&full_response);
                full_response = clean_response;
            }
        }
        
        // 7. 记录使用的工具（用于下一次过滤的连续性判断）
        self.extract_and_record_used_tools(&full_response);
        
        Ok(())
    }
    
    /// 提取并记录本次使用的工具
    fn extract_and_record_used_tools(&mut self, response: &str) {
        // 从响应中提取工具调用
        // 这里可以解析 tool_calls 或者从文本中提取
        
        // 假设我们有一个工具调用列表
        let used_tools = self.extract_tool_calls_from_response(response);
        
        for tool_name in used_tools {
            // 添加到历史记录（最多保留 10 个）
            if self.recent_tools.len() >= 10 {
                self.recent_tools.pop_back();
            }
            self.recent_tools.push_front(tool_name);
        }
    }
}
```

---

## 🎯 预期效果

### 工作流模式

**场景**：用户在 Spec Mode 的"需求分析"步骤

**可用工具**：
- ✅ `memory_search` - 查找相关知识
- ✅ `file_read` - 查看现有代码
- ✅ `code_search` - 搜索相关实现
- ❌ `file_write` - **不允许**（还没到实现阶段）
- ❌ `file_patch` - **不允许**
- ❌ `shell_exec` - **不允许**

**优势**：
- 防止 LLM 跳过步骤直接写代码
- 强制遵循工作流
- 减少工具选择困惑

### 自由模式

**场景 1**：用户问"帮我实现一个登录功能"

**检测结果**：
- 意图：`CodeWriting`
- 关键词：`["实现", "登录", "authentication"]`

**Top 7 工具**：
1. `file_write` (100 分)
2. `file_patch` (95 分)
3. `file_read` (80 分)
4. `code_search` (70 分)
5. `project_detect` (50 分)
6. `memory_search` (45 分)
7. `file_search` (30 分)

**场景 2**：用户问"分析一下这个项目"

**检测结果**：
- 意图：`Exploration`
- 关键词：`["分析", "项目", "project", "structure"]`

**Top 7 工具**：
1. `file_list` (100 分)
2. `project_detect` (95 分)
3. `file_search` (85 分 + 关键词加分)
4. `code_search` (75 分)
5. `file_read` (70 分)
6. `memory_search` (60 分)
7. `web_fetch` (30 分)

---

## 📊 实施计划

### Phase 3.1：基础框架（1-2天）

- [ ] 实现 `SmartToolFilter` 结构体
- [ ] 实现 `RuleBasedClassifier`
- [ ] 实现工作流模式的白名单过滤
- [ ] 在 `ToolRegistry` 中添加 `get_all_tools()` 方法

### Phase 3.2：自由模式过滤（2-3天）

- [ ] 实现意图评分算法
- [ ] 实现关键词匹配
- [ ] 实现连续性加分
- [ ] 集成到 Agent 主流程

### Phase 3.3：优化与测试（1-2天）

- [ ] 调整评分权重
- [ ] 添加单元测试
- [ ] 实际场景测试
- [ ] 收集反馈并优化

---

## 💡 关键优势

1. **零额外成本**：复用 LLM 已有的输出
2. **语言无关**：支持任意语言的输入
3. **渐进增强**：三层策略确保鲁棒性
4. **可解释性强**：每一步都有明确的分数来源
5. **易于调试**：可以日志记录每个工具的得分
6. **灵活配置**：可以根据项目特点调整权重

---

## ⚠️ 注意事项

1. **性能考虑**：
   - 过滤算法应该在毫秒级完成
   - 避免复杂的字符串操作
   - 使用 HashSet 进行快速查找

2. **边界情况**：
   - 用户意图不明确时使用默认工具集
   - 置信度低于阈值时降级到规则匹配
   - 始终保留基础工具（file_read, file_write）

3. **用户体验**：
   - 过滤不应该导致必要工具不可用
   - 提供配置项可以禁用过滤（调试用）
   - 日志记录过滤结果便于排查问题

---

**下一步**：开始实施 Phase 3.1，构建基础框架。
