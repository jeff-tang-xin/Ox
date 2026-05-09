# LLM 响应中断问题诊断指南

## 问题描述

大模型运行过程中突然终止，没有回复也没有后续，程序似乎"卡住"了。

## 可能的原因

### 1. **LLM API 连接问题**
- API 请求超时
- 网络连接中断
- LLM 服务端错误
- 防火墙/代理阻止

### 2. **程序 Panic**
- UTF-8 字符串切片越界（已修复）
- 其他未捕获的 panic
- 内存不足

### 3. **异步任务问题**
- CancellationToken 被意外触发
- channel 发送/接收阻塞
- tokio 任务被取消

### 4. **资源耗尽**
- 文件描述符耗尽
- 线程池饱和
- 内存泄漏

---

## 诊断步骤

### 第一步：启用详细日志

运行程序时设置详细的日志级别：

```bash
RUST_LOG=debug RUST_BACKTRACE=1 cargo run
```

或只查看关键信息：

```bash
RUST_LOG=info cargo run
```

### 第二步：观察日志输出

#### 正常流程应该看到：

1. **LLM 流式响应开始**
   ```
   [LLM STREAM] Starting stream to https://api.openai.com (model: gpt-4)
   ```

2. **接收数据块**
   ```
   [LLM STREAM] Received chunk #1: 256 bytes
   [LLM STREAM] Received chunk #11: 512 bytes
   ...
   ```

3. **LLM 流完成**
   ```
   [LLM STREAM] Stream ended: total_chunks=45, consecutive_errors=0, done_sent=true
   [AGENT] ✅ LLM stream completed (prompt: 1234, completion: 567, total: 1801)
   ```

4. **Agent Turn 完成**
   ```
   [AGENT TURN] ✅ Turn completed successfully, 2 new messages
   ```

5. **关键词提取**
   ```
   [KEYWORD EXTRACTION] ✅ Extracted 3 keywords, 2 topics, 1 files
   [SEMANTIC LEARNING] ✅ Recorded 3 keywords, 2 topics for query: '...'
   ```

6. **记忆检索**
   ```
   [MEMORY RETRIEVAL] Starting retrieval for query: '...', limit: 5, rerank: enabled
   [MEMORY RETRIEVAL] Initial retrieval returned 1 memories
   [MEMORY] Re-ranking 1 memories (target: 5)
   [MEMORY] Re-ranking complete, returned 1 memories
   ```

7. **满意度评估**
   ```
   [SATISFACTION] explicit=0.50, tool=1.00, code_accept=0.00, overall=0.35
   ```

#### 异常情况：

**情况 A: LLM 流突然中断**
```
[LLM STREAM] Received chunk #23: 128 bytes
[LLM STREAM] ⚠️ Chunk error (consecutive: 1/3): connection reset by peer
[LLM STREAM] ⚠️ Chunk error (consecutive: 2/3): connection reset by peer
[LLM STREAM] ⚠️ Chunk error (consecutive: 3/3): connection reset by peer
[LLM STREAM] Stream ended: total_chunks=23, consecutive_errors=3, done_sent=false
```

**说明**: 网络连接不稳定，连续 3 次错误后中断。

**解决**: 
- 检查网络连接
- 检查防火墙/代理设置
- 稍后重试

---

**情况 B: Cancellation Token 触发**
```
[AGENT] ⚠️ Cancellation token triggered, stopping LLM stream
```

**说明**: 用户按下了中断键（Ctrl+C），或程序内部触发了取消。

**解决**: 
- 如果不是用户主动中断，检查是否有代码意外调用了 `cancel_token.cancel()`
- 检查是否有超时机制被触发

---

**情况 C: LLM API 错误**
```
OpenAI API error 429: { "error": { "message": "Rate limit exceeded" } }
```

**说明**: API 速率限制、认证失败或其他 API 错误。

**解决**:
- 检查 API 密钥是否正确
- 检查是否超出速率限制
- 查看完整的错误消息

---

**情况 D: 程序 Panic**
```
thread 'main' panicked at 'index out of bounds: the len is 10 but the index is 10'
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
```

**说明**: 程序发生了 panic，但没有正确捕获。

**解决**:
- 使用 `RUST_BACKTRACE=1` 运行，查看完整的堆栈跟踪
- 根据堆栈定位问题代码
- 添加错误处理

---

**情况 E: 日志突然停止**

如果日志在某个点突然停止，没有任何错误或完成消息：

```
[LLM STREAM] Starting stream to https://api.openai.com (model: gpt-4)
[LLM STREAM] Received chunk #1: 256 bytes
[LLM STREAM] Received chunk #11: 512 bytes
...
(然后就没有了)
```

**可能原因**:
1. **程序死锁**: 某个地方发生了死锁，线程无法继续执行
2. **异步任务挂起**: `tokio::spawn` 的任务被挂起
3. **Channel 阻塞**: sender/receiver 一端关闭，另一端阻塞
4. **资源耗尽**: 系统资源不足，程序无法继续

**诊断方法**:

1. **检查 CPU 和内存使用**
   ```bash
   # Windows PowerShell
   Get-Process ox | Select-Object CPU,WorkingSet
   
   # Linux/Mac
   top -p $(pgrep ox)
   ```

2. **检查是否有僵尸进程**
   ```bash
   ps aux | grep ox
   ```

3. **使用 strace/ltrace 跟踪系统调用** (Linux)
   ```bash
   strace -p <pid>
   ```

4. **检查线程状态**
   ```bash
   # Linux
   ps -T -p <pid>
   
   # 或使用 gdb
   gdb -p <pid>
   thread apply all bt
   ```

---

## 常见场景及解决方案

### 场景 1: 长文本生成时中断

**症状**: 生成短文本正常，但生成长文本（如完整文档）时中断

**可能原因**:
- API 超时（默认可能有 60s 或 120s 超时）
- Token 限制达到上限
- 内存不足

**解决**:
1. 检查配置中的超时设置
2. 检查 `max_tokens` 配置
3. 监控内存使用情况

---

### 场景 2: 工具调用后中断

**症状**: LLM 调用工具后，等待工具执行结果时中断

**可能原因**:
- 工具执行时间过长
- 工具执行出错但未正确返回
- Channel 通信问题

**解决**:
1. 检查工具执行的日志
2. 确认工具是否正确返回结果
3. 检查是否有超时机制

---

### 场景 3: 记忆检索后中断

**症状**: 执行记忆检索后程序卡住

**可能原因**:
- 数据库锁定
- 重排序耗时过长
- BGE 模型加载失败

**解决**:
1. 检查数据库是否正常
2. 查看重排序日志
3. 确认 BGE 模型路径是否正确

---

### 场景 4: 随机中断（无规律）

**症状**: 有时正常，有时中断，没有明显规律

**可能原因**:
- 网络不稳定
- 资源竞争（race condition）
- 内存泄漏导致 OOM

**解决**:
1. 多次运行，记录每次中断的位置
2. 检查是否有共同的特征
3. 使用 valgrind 或类似工具检查内存泄漏

---

## 调试技巧

### 1. 添加断点日志

在可疑位置添加日志：

```rust
tracing::info!("[DEBUG] Reached point A");
// ... some code ...
tracing::info!("[DEBUG] Reached point B");
```

这样可以精确定位程序在哪一行停止。

### 2. 使用 tokio-console

如果使用 tokio 运行时，可以启用 `tokio-console` 来监控异步任务：

```toml
# Cargo.toml
[dependencies]
console-subscriber = "0.1"
```

```rust
// main.rs
console_subscriber::init();
```

然后运行：
```bash
cargo run
# 在另一个终端
tokio-console
```

### 3. 检查 Channel 状态

如果怀疑是 channel 阻塞，可以添加计数器：

```rust
let mut send_count = 0;
let _ = tx.send(event);
send_count += 1;
tracing::debug!("Sent {} events", send_count);
```

### 4. 超时检测

为长时间运行的操作添加超时：

```rust
use tokio::time::{timeout, Duration};

match timeout(Duration::from_secs(60), some_async_operation()).await {
    Ok(result) => { /* success */ }
    Err(_) => {
        tracing::error!("Operation timed out after 60 seconds");
    }
}
```

---

## 收集诊断信息

如果问题持续存在，请收集以下信息：

1. **完整的日志输出**（从启动到中断）
2. **RUST_BACKTRACE=1 的输出**（如果有 panic）
3. **系统信息**：
   - 操作系统版本
   - Rust 版本 (`rustc --version`)
   - 内存使用情况
4. **配置文件**（隐藏敏感信息如 API 密钥）
5. **复现步骤**：如何稳定复现这个问题

---

## 已知的修复

### 修复 1: UTF-8 字符串切片 panic

**问题**: `split_with_overlap` 函数使用字节索引切片，遇到中文字符时 panic

**修复**: 改用字符迭代器

**文件**: `crates/ox-core/src/memory/mod.rs`

---

### 修复 2: 添加详细日志

**改进**: 在关键位置添加了详细的日志输出

**位置**:
- `agent/mod.rs`: Agent Turn 完成、Cancellation Token 触发
- `llm/openai.rs`: LLM 流式响应开始、结束、错误
- `memory/mod.rs`: 记忆检索各阶段
- `keyword_extraction.rs`: 关键词提取结果

---

## 下一步

1. **运行程序并观察日志**
2. **根据日志输出判断问题类型**
3. **参考上述场景找到对应的解决方案**
4. **如果问题仍未解决，收集诊断信息并报告**

---

## 联系支持

如果以上方法都无法解决问题，请提供：
- 完整的日志输出
- 复现步骤
- 系统环境信息

我们会进一步协助诊断。
