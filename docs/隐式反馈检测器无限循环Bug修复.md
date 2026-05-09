# 隐式反馈检测器无限循环 Bug 修复

## 问题描述

大模型输出完毕、文档输出完毕后，日志一直刷屏：

```
INFO ox::middleware::feedback: [IMPLICIT FEEDBACK] Major rewrite: "\\\\?\\F:\\code\\ecovacs-platform\\docs\\ORDER_MODULE.md" (48.9%)
INFO ox::middleware::feedback: [IMPLICIT FEEDBACK] Major rewrite: "\\\\?\\F:\\code\\ecovacs-platform\\docs\\ORDER_MODULE.md" (48.9%)
INFO ox::middleware::feedback: [IMPLICIT FEEDBACK] Major rewrite: "\\\\?\\F:\\code\\ecovacs-platform\\docs\\ORDER_MODULE.md" (48.9%)
...
```

同一个文件被反复报告为"重大重写"，每 ~50ms 一次，导致日志无限刷屏。

---

## 根本原因

**文件**: `crates/ox-core/src/feedback/override_detector.rs`

**Bug 位置**: `detect_overrides()` 函数（第 53-108 行）

### 问题代码

```rust
pub fn detect_overrides(&mut self) -> Vec<OverrideSignal> {
    let mut signals = vec![];
    let mut to_remove = vec![];

    for (path, record) in &self.recent_writes {
        // ... 检测逻辑 ...
        
        if current_hash == record.content_hash {
            // No changes - accepted
            continue;  // ❌ 没有标记为移除
        }

        // Calculate change ratio
        let change_ratio = calculate_diff_ratio(...);

        signals.push(OverrideSignal {
            path: path.clone(),
            change_ratio,
            time_elapsed: record.timestamp.elapsed(),
        });
        // ❌ 检测到改动后，也没有标记为移除！
    }

    // Clean up expired records
    for path in to_remove {
        self.recent_writes.remove(&path);
    }

    signals
}
```

### 问题分析

1. **检测到文件修改后，没有将记录从 `recent_writes` 中移除**
2. 下次调用 `detect_overrides()` 时（通常是用户输入前），**又会检测到同一个文件的同一次修改**
3. 因为文件内容确实被用户修改了（48.9% 的改动），所以每次都会报告
4. 但记录从未被清除，导致**无限循环报告**

### 触发条件

- 用户修改了 AI 生成的文件
- 修改比例 > 30%（触发 `StrongNegative` 信号）
- 在检测窗口内（默认 5 分钟）
- 每次用户输入前都会调用 `detect_overrides()`

---

## 修复方案

**核心思路**: 检测到改动（或无改动）后，立即将记录从追踪列表中移除，避免重复报告。

### 修复后的代码

```rust
pub fn detect_overrides(&mut self) -> Vec<OverrideSignal> {
    let mut signals = vec![];
    let mut to_remove = vec![];

    for (path, record) in &self.recent_writes {
        // Skip if outside detection window
        if record.timestamp.elapsed() > self.detection_window {
            to_remove.push(path.clone());
            continue;
        }

        // Check if file still exists
        if !path.exists() {
            // File was deleted - strong negative signal
            signals.push(OverrideSignal {
                path: path.clone(),
                change_ratio: 1.0,
                time_elapsed: record.timestamp.elapsed(),
            });
            to_remove.push(path.clone());
            continue;
        }

        // Compare current content with Ox's version
        if let Ok(current_content) = std::fs::read_to_string(path) {
            let current_hash = hash_content(&current_content);

            if current_hash == record.content_hash {
                // No changes - accepted, remove from tracking
                to_remove.push(path.clone());  // ✅ 添加：标记为移除
                continue;
            }

            // Calculate change ratio
            let current_lines = current_content.lines().count();
            let change_ratio = calculate_diff_ratio(
                record.content_hash,
                record.line_count,
                current_hash,
                current_lines,
            );

            signals.push(OverrideSignal {
                path: path.clone(),
                change_ratio,
                time_elapsed: record.timestamp.elapsed(),
            });
            
            // ✅ IMPORTANT: Remove from tracking after detecting override
            // This prevents reporting the same override multiple times
            to_remove.push(path.clone());  // ✅ 添加：标记为移除
        }
    }

    // Clean up processed and expired records
    for path in to_remove {
        self.recent_writes.remove(&path);
    }

    signals
}
```

### 关键修改

1. **无改动时移除记录**（第 83 行）：
   ```rust
   if current_hash == record.content_hash {
       // No changes - accepted, remove from tracking
       to_remove.push(path.clone());  // ✅ 新增
       continue;
   }
   ```

2. **检测到改动后移除记录**（第 103 行）：
   ```rust
   signals.push(OverrideSignal { ... });
   
   // ✅ IMPORTANT: Remove from tracking after detecting override
   // This prevents reporting the same override multiple times
   to_remove.push(path.clone());  // ✅ 新增
   ```

---

## 修复效果

### 修复前

```
[IMPLICIT FEEDBACK] Major rewrite: "docs/ORDER_MODULE.md" (48.9%)
[IMPLICIT FEEDBACK] Major rewrite: "docs/ORDER_MODULE.md" (48.9%)
[IMPLICIT FEEDBACK] Major rewrite: "docs/ORDER_MODULE.md" (48.9%)
[IMPLICIT FEEDBACK] Major rewrite: "docs/ORDER_MODULE.md" (48.9%)
... (无限循环)
```

### 修复后

```
[IMPLICIT FEEDBACK] Major rewrite: "docs/ORDER_MODULE.md" (48.9%)
(只报告一次，然后该文件从追踪列表中移除)
```

---

## 设计原理

### 为什么需要移除记录？

`CodeOverrideDetector` 的设计目标是：
1. **追踪 AI 写入的文件**
2. **在用户下次输入前检测是否被修改**
3. **报告修改情况作为隐式反馈**
4. **每个文件的每次写入只报告一次**

如果不移除记录：
- 同一个修改会被反复报告
- 无法区分"新的修改"和"旧的修改"
- 日志刷屏，用户体验差

### 什么时候应该移除记录？

1. **文件未被修改**：说明用户接受了 AI 的输出
2. **文件被修改**：已经报告了修改信号，不需要再报告
3. **文件被删除**：已经报告了删除信号
4. **超出检测窗口**：时间太久，不再相关

---

## 测试建议

### 测试场景 1: 文件被修改

1. AI 生成一个文件 `test.md`
2. 用户修改该文件（改动 > 30%）
3. 用户输入下一个问题
4. **预期**: 看到一次 `[IMPLICIT FEEDBACK] Major rewrite` 日志
5. 再次输入问题
6. **预期**: 不再看到该文件的反馈日志

### 测试场景 2: 文件未被修改

1. AI 生成一个文件 `test.md`
2. 用户不修改该文件
3. 用户输入下一个问题
4. **预期**: 看到 `[IMPLICIT FEEDBACK] Accepted` 日志（debug 级别）
5. 该文件从追踪列表中移除

### 测试场景 3: 多次写入同一文件

1. AI 第一次写入 `test.md`
2. 用户修改
3. 用户输入 → 报告修改
4. AI 第二次写入 `test.md`（覆盖）
5. 用户再次修改
6. 用户输入 → **应该报告第二次的修改**

**注意**: 第 4 步时，`register_write()` 会更新记录，所以第 6 步能正确检测到新的修改。

---

## 相关文件

- **实现**: `crates/ox-core/src/feedback/override_detector.rs`
- **中间件**: `crates/ox-cli/src/middleware/feedback.rs`
- **主程序调用**: `crates/ox-cli/src/main.rs` (在用户输入前调用 `detect_overrides()`)

---

## 相关配置

检测窗口可在配置文件中调整：

```toml
[feedback]
detection_window_secs = 300  # 默认 5 分钟
```

- **较短的窗口**（如 60s）：更快清除记录，减少误报
- **较长的窗口**（如 600s）：有更多时间检测修改，但可能漏掉快速修改

---

## 总结

这是一个典型的**状态管理 bug**：
- **症状**: 无限循环报告
- **原因**: 检测到事件后未清除状态
- **修复**: 在处理完事件后立即清除状态
- **教训**: 追踪类功能必须有明确的"生命周期"，处理完后要清理
