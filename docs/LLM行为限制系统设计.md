# LLM 行为限制系统设计

## 问题背景

大模型操作经常导致乱码，原因包括：
1. 输出超长未限制
2. 工具调用频率过高
3. 缺少内容验证（不可打印字符）
4. 缺少行为频率限制

## 设计方案

### 核心思路

通过**多层限制**来控制 LLM 的行为：

```
Layer 1: 输出长度限制（单条消息最大字符数）
Layer 2: 内容验证（检测不可打印字符、乱码）
Layer 3: 工具调用频率限制（最小间隔时间）
Layer 4: 连续错误限制（连续 N 次失败后暂停）
Layer 5: 文件写入限制（单文件最大大小、单次最大行数）
```

### 实施方案

#### 1. 输出长度限制

**配置项**:
```toml
[llm_behavior_limits]
max_output_chars_per_message = 5000      # 单条消息最大字符数
max_output_lines_per_message = 200       # 单条消息最大行数
truncate_suffix = "...[output truncated]" # 截断后缀
```

**实现位置**: `agent/mod.rs` 中的 `TextDelta` 处理

```rust
// 在 LlmStreamEvent::TextDelta 处理中
LlmStreamEvent::TextDelta(text) => {
    // 检查输出长度限制
    if full_text.len() + text.len() > config.llm_behavior_limits.max_output_chars_per_message {
        let remaining = config.llm_behavior_limits.max_output_chars_per_message - full_text.len();
        if remaining > 0 {
            let truncated = &text[..remaining];
            let _ = ui_tx.send(AgentToUiEvent::TextChunk(truncated.to_string()));
            full_text.push_str(truncated);
        }
        // 发送截断提示
        let _ = ui_tx.send(AgentToUiEvent::TextChunk(
            config.llm_behavior_limits.truncate_suffix.to_string()
        ));
        full_text.push_str(&config.llm_behavior_limits.truncate_suffix);
        
        tracing::warn!(
            "LLM output truncated: exceeded {} chars",
            config.llm_behavior_limits.max_output_chars_per_message
        );
        return; // 停止接收更多输出
    }
    
    let _ = ui_tx.send(AgentToUiEvent::TextChunk(text.clone()));
    full_text.push_str(&text);
}
```

#### 2. 内容验证（乱码检测）

**配置项**:
```toml
[llm_behavior_limits]
enable_content_validation = true         # 启用内容验证
max_non_printable_ratio = 0.05           # 最大不可打印字符比例（5%）
detect_encoding_errors = true            # 检测编码错误
```

**实现位置**: 新增 `content_validator.rs`

```rust
use std::collections::HashMap;

pub struct ContentValidator {
    max_non_printable_ratio: f64,
}

impl ContentValidator {
    pub fn new(max_non_printable_ratio: f64) -> Self {
        Self { max_non_printable_ratio }
    }
    
    /// 验证文本内容，返回是否有效
    pub fn validate(&self, text: &str) -> ValidationResult {
        if text.is_empty() {
            return ValidationResult::Empty;
        }
        
        // 检查不可打印字符比例
        let total_chars = text.chars().count();
        let non_printable_count = text.chars()
            .filter(|c| !c.is_whitespace() && !c.is_ascii_graphic() && !c.is_ascii_punctuation())
            .count();
        
        let non_printable_ratio = non_printable_count as f64 / total_chars as f64;
        
        if non_printable_ratio > self.max_non_printable_ratio {
            return ValidationResult::TooManyNonPrintable {
                ratio: non_printable_ratio,
                count: non_printable_count,
                total: total_chars,
            };
        }
        
        // 检查常见乱码模式
        if Self::contains_garbled_text(text) {
            return ValidationResult::GarbledText;
        }
        
        ValidationResult::Valid
    }
    
    /// 检测是否包含乱码文本
    fn contains_garbled_text(text: &str) -> bool {
        // 检测连续的乱码字符（如 \x00\x00\x00）
        let null_sequences = text.matches("\x00\x00\x00").count();
        if null_sequences > 0 {
            return true;
        }
        
        // 检测大量连续的不可打印字符
        let mut consecutive_non_printable = 0;
        for c in text.chars() {
            if !c.is_whitespace() && !c.is_ascii_graphic() && !c.is_ascii_punctuation() {
                consecutive_non_printable += 1;
                if consecutive_non_printable > 10 {
                    return true;
                }
            } else {
                consecutive_non_printable = 0;
            }
        }
        
        false
    }
}

pub enum ValidationResult {
    Valid,
    Empty,
    TooManyNonPrintable { ratio: f64, count: usize, total: usize },
    GarbledText,
}
```

**集成到工具执行**:

```rust
// 在 tool 执行后验证输出
let validator = ContentValidator::new(0.05);
let validation_result = validator.validate(&output);

match validation_result {
    ValidationResult::Valid => {
        // 正常处理
    }
    ValidationResult::TooManyNonPrintable { ratio, .. } => {
        tracing::warn!(
            "Tool output contains {:.1}% non-printable characters",
            ratio * 100.0
        );
        // 清理不可打印字符
        let cleaned = output.chars()
            .filter(|c| c.is_whitespace() || c.is_ascii_graphic() || c.is_ascii_punctuation())
            .collect();
        output = cleaned;
    }
    ValidationResult::GarbledText => {
        tracing::error!("Tool output appears to be garbled/invalid");
        return ToolOutput::error("Output contains invalid characters");
    }
    ValidationResult::Empty => {
        // 允许空输出
    }
}
```

#### 3. 工具调用频率限制

**配置项**:
```toml
[llm_behavior_limits]
min_tool_call_interval_ms = 500          # 工具调用最小间隔（毫秒）
max_tools_per_turn = 50                  # 单次 turn 最大工具调用数
```

**实现位置**: `agent/mod.rs` 的 tool 执行循环

```rust
use std::time::{Instant, Duration};

// 在 agent_run 函数中
let mut last_tool_call_time = Instant::now();
let mut tool_call_count = 0u32;
let max_tools_per_turn = config.llm_behavior_limits.max_tools_per_turn;
let min_interval = Duration::from_millis(
    config.llm_behavior_limits.min_tool_call_interval_ms
);

for tc in tool_calls {
    // 检查工具调用次数限制
    if tool_call_count >= max_tools_per_turn {
        tracing::warn!(
            "Tool call limit reached: {} calls in this turn",
            tool_call_count
        );
        let _ = ui_tx.send(AgentToUiEvent::ToolResult {
            name: "system".to_string(),
            output: format!(
                "⚠️ Tool call limit reached ({} per turn). Please refine your request.",
                max_tools_per_turn
            ),
            is_error: true,
        });
        break;
    }
    
    // 检查工具调用频率
    let elapsed = last_tool_call_time.elapsed();
    if elapsed < min_interval {
        let wait_time = min_interval - elapsed;
        tokio::time::sleep(wait_time).await;
    }
    
    // 执行工具...
    last_tool_call_time = Instant::now();
    tool_call_count += 1;
}
```

#### 4. 连续错误限制

**配置项**:
```toml
[llm_behavior_limits]
max_consecutive_errors = 3               # 最大连续错误次数
error_cooldown_ms = 5000                 # 错误后冷却时间（毫秒）
```

**实现位置**: `agent/mod.rs` 的工具执行错误处理

```rust
let mut consecutive_errors = 0u32;
let max_consecutive_errors = config.llm_behavior_limits.max_consecutive_errors;

// 在工具执行循环中
match tool.execute(args, &tool_ctx).await {
    Ok(output) => {
        // 成功后重置计数器
        consecutive_errors = 0;
        // ... 正常处理
    }
    Err(e) => {
        consecutive_errors += 1;
        
        if consecutive_errors >= max_consecutive_errors {
            tracing::error!(
                "Too many consecutive tool errors ({}), pausing agent",
                consecutive_errors
            );
            
            let _ = ui_tx.send(AgentToUiEvent::ToolResult {
                name: "system".to_string(),
                output: format!(
                    "⚠️ Too many consecutive errors ({}). Agent paused for {}s.",
                    consecutive_errors,
                    config.llm_behavior_limits.error_cooldown_ms / 1000
                ),
                is_error: true,
            });
            
            // 暂停冷却
            tokio::time::sleep(Duration::from_millis(
                config.llm_behavior_limits.error_cooldown_ms
            )).await;
            
            // 重置计数器
            consecutive_errors = 0;
        }
        
        // ... 错误处理
    }
}
```

#### 5. 文件写入限制

**配置项**:
```toml
[llm_behavior_limits]
max_file_size_bytes = 1048576            # 单文件最大大小（1MB）
max_write_lines = 1000                   # 单次写入最大行数
```

**实现位置**: `tools/file_write.rs`

```rust
// 在 FileWriteTool::execute 中
// 检查文件大小
if content.len() > config.llm_behavior_limits.max_file_size_bytes {
    return ToolOutput::error(format!(
        "❌ File too large: {} bytes (max: {} bytes)\n\n\
         💡 Please split into smaller files or reduce content.",
        content.len(),
        config.llm_behavior_limits.max_file_size_bytes
    ));
}

// 检查行数
let line_count = content.lines().count();
if line_count > config.llm_behavior_limits.max_write_lines {
    return ToolOutput::error(format!(
        "❌ Too many lines: {} (max: {})\n\n\
         💡 Please write files in smaller chunks.",
        line_count,
        config.llm_behavior_limits.max_write_lines
    ));
}
```

## 配置结构

在 `config/mod.rs` 中添加：

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LlmBehaviorLimitsConfig {
    // 输出限制
    pub max_output_chars_per_message: usize,
    pub max_output_lines_per_message: usize,
    pub truncate_suffix: String,
    
    // 内容验证
    pub enable_content_validation: bool,
    pub max_non_printable_ratio: f64,
    pub detect_encoding_errors: bool,
    
    // 频率限制
    pub min_tool_call_interval_ms: u64,
    pub max_tools_per_turn: u32,
    
    // 错误限制
    pub max_consecutive_errors: u32,
    pub error_cooldown_ms: u64,
    
    // 文件限制
    pub max_file_size_bytes: usize,
    pub max_write_lines: usize,
}

impl Default for LlmBehaviorLimitsConfig {
    fn default() -> Self {
        Self {
            max_output_chars_per_message: 5000,
            max_output_lines_per_message: 200,
            truncate_suffix: "...[output truncated]".into(),
            
            enable_content_validation: true,
            max_non_printable_ratio: 0.05,
            detect_encoding_errors: true,
            
            min_tool_call_interval_ms: 500,
            max_tools_per_turn: 50,
            
            max_consecutive_errors: 3,
            error_cooldown_ms: 5000,
            
            max_file_size_bytes: 1048576, // 1MB
            max_write_lines: 1000,
        }
    }
}
```

## 预期效果

### 改进前
```
LLM: [输出 10000 字符，包含乱码]
     [连续调用 100 个工具]
     [连续 10 次错误]
     [写入 5000 行文件]
```

### 改进后
```
LLM: [输出 5000 字符后截断]
     [工具调用间隔 500ms]
     [连续 3 次错误后暂停 5s]
     [文件超过 1MB 时拒绝]
```

## 实施计划

### Phase 1: 核心限制（1-2 天）
1. ✅ 添加配置结构
2. ✅ 实施输出长度限制
3. ✅ 实施工具调用频率限制

### Phase 2: 内容验证（1 天）
1. ✅ 创建 `content_validator.rs`
2. ✅ 集成到工具输出处理
3. ✅ 添加乱码检测和清理

### Phase 3: 错误限制（1 天）
1. ✅ 实施连续错误限制
2. ✅ 添加错误冷却机制
3. ✅ 实施文件写入限制

### Phase 4: 测试与优化（1 天）
1. ✅ 单元测试
2. ✅ 集成测试
3. ✅ 性能测试

## 向后兼容性

- ✅ 所有配置项都有默认值
- ✅ 可以通过配置完全禁用限制
- ✅ 不影响现有功能
- ✅ 用户可通过配置调整限制参数
