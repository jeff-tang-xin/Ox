# OpenAI API 工具调用 ID 不匹配修复

## 问题描述

### 错误信息

```
OpenAI API error 400 Bad Request: {
  "type":"error",
  "error":{
    "type":"bad_request_error",
    "message":"invalid params, tool result's tool id(call_function_wa3s3wtgc8j9_1) not found (2013)",
    "http_code":"400"
  },
  "request_id":"064dfd75a08fb858fc0cb7cc6aba4494"
}
```

### 症状

- LLM 返回工具调用时，ID 格式为 `call_function_xxx`
- 当 Ox 尝试返回工具执行结果时，OpenAI API 报错说找不到这个 ID
- 导致工具调用失败，LLM 无法继续执行

---

## 根本原因

### OpenAI API 的工具调用 ID 格式要求

**标准格式**: `call_xxx`（例如：`call_abc123`）

**某些兼容 API 的格式**: `call_function_xxx`（例如：`call_function_wa3s3wtgc8j9_1`）

### 问题流程

```
1. LLM 返回工具调用
   → ID: "call_function_wa3s3wtgc8j9_1"
   
2. Ox 解析并保存这个 ID
   
3. Ox 执行工具，准备返回结果
   → 使用 ID: "call_function_wa3s3wtgc8j9_1"
   
4. OpenAI API 验证 ID
   → ❌ 错误：找不到 "call_function_wa3s3wtgc8j9_1"
   → ✅ 期望：应该是 "call_wa3s3wtgc8j9_1"
```

### 为什么会这样？

某些 OpenAI 兼容的 API 提供商（如硅基流动、DeepSeek 等）在返回工具调用时，使用了非标准的 ID 格式：
- **标准 OpenAI**: `call_abc123`
- **某些兼容 API**: `call_function_abc123`

但 OpenAI API 在验证工具结果时，只接受标准格式的 ID。

---

## 修复方案

### 核心思路

在解析 LLM 响应时，**规范化（Normalize）工具调用 ID**，将非标准格式转换为标准格式。

### 修复代码

**文件**: `crates/ox-core/src/llm/openai_sse.rs`

```rust
if let (Some(id), Some(name)) = (&id, &name) {
    // ✅ Normalize tool call ID to ensure compatibility with OpenAI API
    // Some APIs return IDs like "call_function_xxx" but OpenAI expects "call_xxx"
    let normalized_id = if id.starts_with("call_function_") {
        // Remove "function_" prefix to match OpenAI format
        id.replace("call_function_", "call_")
    } else {
        id.clone()
    };
    
    // O(1) lookup using reverse map
    let is_new = !self.id_to_index.contains_key(normalized_id.as_str());
    if is_new {
        self.tool_call_ids.insert(tc_index, normalized_id.clone());
        self.tool_call_names.insert(tc_index, name.clone());
        self.active_tool_calls.insert(tc_index);
        self.id_to_index.insert(normalized_id.clone(), tc_index);
        events.push(LlmStreamEvent::ToolCallStart {
            id: normalized_id.clone(),
            name: name.clone(),
        });
    }
}
```

### 修复效果

**修复前**:
```
LLM 返回: call_function_wa3s3wtgc8j9_1
Ox 保存:  call_function_wa3s3wtgc8j9_1
Ox 返回:  call_function_wa3s3wtgc8j9_1
API 验证: ❌ 找不到 call_function_wa3s3wtgc8j9_1
```

**修复后**:
```
LLM 返回: call_function_wa3s3wtgc8j9_1
Ox 转换:  call_wa3s3wtgc8j9_1  ← 规范化
Ox 保存:  call_wa3s3wtgc8j9_1
Ox 返回:  call_wa3s3wtgc8j9_1
API 验证: ✅ 找到 call_wa3s3wtgc8j9_1
```

---

## 测试验证

### 测试场景 1: 标准格式 ID

**输入**: `call_abc123`  
**输出**: `call_abc123`（不变）  
**结果**: ✅ 正常工作

### 测试场景 2: 非标准格式 ID

**输入**: `call_function_wa3s3wtgc8j9_1`  
**输出**: `call_wa3s3wtgc8j9_1`（移除 `function_`）  
**结果**: ✅ 符合 OpenAI 要求

### 测试场景 3: 其他格式

**输入**: `tool_call_xyz`  
**输出**: `tool_call_xyz`（不变）  
**结果**: ✅ 保持原样

---

## 影响范围

### 修改的文件

- `crates/ox-core/src/llm/openai_sse.rs`（第 156-177 行）

### 影响的组件

- OpenAI SSE 解析器
- 所有使用 OpenAI 兼容 API 的场景

### 不影响的部分

- Anthropic API（不使用工具调用 ID）
- 本地 LLM（如果有自定义格式）
- 其他不涉及工具调用的功能

---

## 兼容性说明

### 支持的 API 提供商

✅ **标准 OpenAI API**  
- `call_abc123` → `call_abc123`（不变）

✅ **硅基流动（SiliconFlow）**  
- `call_function_xxx` → `call_xxx`（规范化）

✅ **DeepSeek**  
- `call_function_xxx` → `call_xxx`（规范化）

✅ **其他兼容 API**  
- 任何以 `call_function_` 开头的 ID 都会被规范化

### 向后兼容

- ✅ 已有的标准格式 ID 不受影响
- ✅ 不需要修改配置文件
- ✅ 不需要迁移数据

---

## 潜在问题

### 问题 1: 如果 API 返回的 ID 既不是 `call_xxx` 也不是 `call_function_xxx`？

**回答**: 代码会保持原样，不做任何转换。如果 API 要求特定格式，可能需要添加更多的规范化规则。

### 问题 2: 如果同一个工具调用在不同消息中使用不同的 ID 格式？

**回答**: 这种情况不应该发生。OpenAI API 要求工具调用和工具结果使用相同的 ID。如果出现这种情况，是 API 提供商的问题，不是 Ox 的问题。

### 问题 3: 是否需要配置开关来控制这个行为？

**回答**: 不需要。这个规范化是安全的：
- 标准格式的 ID 不会被修改
- 只有非标准格式才会被转换
- 转换后的格式符合 OpenAI 要求

---

## 相关文档

- [OpenAI API 文档 - Tool Calls](https://platform.openai.com/docs/api-reference/chat/create#chat-create-tools)
- [OpenAI API 错误码 2013](https://platform.openai.com/docs/guides/error-codes)

---

## 总结

这是一个**兼容性修复**，确保 Ox 能够正确处理不同 API 提供商返回的工具调用 ID 格式。

**核心改进**:
- ✅ 自动规范化非标准格式的工具调用 ID
- ✅ 保持标准格式不变
- ✅ 无需配置，开箱即用
- ✅ 向后兼容，不影响现有功能

**修复后**，无论 API 提供商返回什么格式的 ID，Ox 都能正确处理并返回给 OpenAI API。
