# Spec Mode 方案 B - 混合模式使用指南

## 📋 功能概述

方案 B 实现了智能的需求命名和管理系统，支持三种创建模式：

1. **快速创建**（默认）- 自动从内容提取名称
2. **手动指定** - 用户自定义需求名称
3. **智能命名**（预留）- LLM 分析生成名称（待实现）

---

## 🚀 使用方法

### 1️⃣ 快速创建（自动命名）

```bash
/spec 实现订单优化功能，支持批量处理和性能监控
```

**行为**：
- 自动提取前 2-3 个词作为名称：`实现-订单-优化`
- 创建目录：`.ox/spec/实现-订单-优化/`
- 初始化工作流到 Step 1

**适用场景**：快速开始新任务，不需要精确控制名称

---

### 2️⃣ 手动指定名称

```bash
/spec order-optimization: 实现订单优化功能，支持批量处理和性能监控
```

**语法**：`/spec <name>: <content>`

**要求**：
- 名称部分不能包含空格
- 名称长度 ≤ 50 字符
- 内容部分 > 10 字符（确保是真实需求而非简单查询）

**行为**：
- 使用指定的名称：`order-optimization`
- 自动清理特殊字符（转换为连字符）
- 创建目录：`.ox/spec/order-optimization/`

**适用场景**：需要规范的英文名称、团队协作、跨平台兼容

---

### 3️⃣ 激活已有需求

```bash
/spec order-optimization
```

**行为**：
- 检测是否存在同名需求
- 加载 `.ox/spec/order-optimization/progress.json`
- 恢复工作流进度到上次离开的步骤
- 显示文件状态（spec.md/task.md 是否存在）

**输出示例**：
```
✅ Resumed requirement: order-optimization (Step 4/6)
📄 Both spec.md and task.md exist
```

**适用场景**：继续之前未完成的任务

---

### 4️⃣ 智能命名（已实现）

```bash
/spec --smart 实现一个分布式缓存系统，支持 Redis 集群和故障转移
```

**行为**：
- 调用 LLM 分析完整需求内容
- 生成语义化、简洁的需求名称
- 例如：`distributed-cache-system`

**工作流程**：
1. 用户输入 `/spec --smart <content>`
2. 系统显示 "🧠 Analyzing content to generate optimal requirement name..."
3. 异步调用 LLM 生成名称
4. 后台任务完成后，使用生成的名称创建需求
5. 如果 LLM 调用失败，回退到自动提取模式

**适用场景**：
- 复杂需求，需要更准确的命名
- 希望名称更具语义化和专业性
- 不急于立即开始，愿意等待几秒生成时间

---

## 📁 目录结构

```
.ox/
└── spec/
    ├── order-optimization/
    │   ├── spec.md          # 需求规格文档
    │   ├── task.md          # 任务分解文档
    │   └── progress.json    # 进度跟踪文件
    ├── user-auth/
    │   ├── spec.md
    │   └── progress.json
    └── session.jsonl        # 所有需求的会话日志（共享）
```

### progress.json 格式

```json
{
  "requirement_name": "order-optimization",
  "workflow_mode": "spec",
  "workflow_id": "spec_workflow",
  "workflow_step_index": 3,
  "last_updated": "2026-05-07T10:30:00Z",
  "session_file": ".ox/spec/order-optimization/session.jsonl"
}
```

---

## 🎯 启动时提示

应用启动时会自动扫描 `.ox/spec/` 目录，显示未完成的任务列表：

```
📋 Incomplete Spec Mode Tasks:
──────────────────────────────────────────────────
  🔵 order-optimization - Step 1/6 (2026-05-07 10:30)
  🟡 user-auth - Step 3/6 (2026-05-06 15:45)
  🟢 payment-integration - Step 5/6 (2026-05-05 09:20)
──────────────────────────────────────────────────
Use /spec <name> to resume a task
```

**状态图标说明**：
- 🔵 Step 1：刚开始
- 🟡 Step 2-3：进行中
- 🟢 Step 4-5：接近完成
- ⚪ Step 6+：其他状态

---

## 💡 最佳实践

### 命名规范

1. **英文优先**（推荐）：
   ```bash
   /spec order-optimization: ...
   /spec user-authentication: ...
   ```

2. **中文自动转换**：
   ```bash
   /spec 订单优化功能
   # → 自动转换为：订单-优化-功能
   ```

3. **避免特殊字符**：
   - ✅ `order-optimization-v2`
   - ❌ `order_optimization@2026`

### 工作流程

```bash
# 1. 创建新需求
/spec order-optimization: 实现订单系统的性能优化...

# 2. AI 自动生成 spec.md（Phase 1）
# 等待 AI 完成...

# 3. 用户确认继续（/Y）
/Y

# 4. AI 生成 task.md（Phase 2）
# 等待 AI 完成...

# 5. 退出应用
/exit

# 6. 下次启动时看到提示
📋 Incomplete Spec Mode Tasks:
  🟡 order-optimization - Step 3/6

# 7. 恢复任务
/spec order-optimization
```

---

## 🔧 技术细节

### 名称提取算法（自动模式）

1. 取第一行文本
2. 按空白字符分割
3. 过滤掉单字符词（如 "a", "I"）
4. 取前 3 个词
5. 转换为小写
6. 用连字符连接
7. 移除非字母数字字符（保留连字符）
8. 去除首尾连字符

**示例**：
```
输入："实现订单优化功能，支持批量处理"
→ ["实现", "订单", "优化"]
→ "实现-订单-优化"
→ "实现-订单-优化"（已清理）
```

### 名称清理算法（手动模式）

1. 转换为小写
2. 非字母数字字符替换为连字符
3. 合并连续连字符
4. 去除首尾连字符

**示例**：
```
输入："Order_Optimization @2026!"
→ "order_optimization_@2026!"
→ "order-optimization--2026-"
→ "order-optimization-2026"
```

---

## ⚠️ 注意事项

1. **名称冲突**：如果手动指定的名称已存在，会直接激活该需求
2. **内容长度**：手动模式下，内容必须 > 10 字符，否则视为普通文本
3. **路径限制**：需求名称不能包含 `/`、`\`、`:` 等路径非法字符
4. **最大长度**：名称限制为 50 字符，超长会被截断

---

## 🐛 故障排查

### 问题：无法激活需求

**检查**：
```bash
# 查看 .ox/spec/ 目录下是否有对应的文件夹
ls .ox/spec/

# 检查 progress.json 是否存在
cat .ox/spec/order-optimization/progress.json
```

### 问题：启动时没有显示未完成任务

**原因**：
- `.ox/spec/` 目录为空
- 所有任务的 `progress.json` 已被删除

**解决**：正常现象，表示没有未完成的任务

---

## 📝 示例场景

### 场景 1：快速原型开发

```bash
/spec 创建用户注册页面，包含邮箱验证和密码强度检查
# → 自动命名：创建-用户-注册
# → 快速开始，无需关心名称
```

### 场景 2：正式项目（团队协作者）

```bash
/spec user-registration: 创建用户注册页面，包含邮箱验证和密码强度检查
# → 使用规范的英文名称
# → 便于团队成员理解和协作
```

### 场景 3：多任务并行

```bash
# 任务 1
/spec order-optimization: ...
# 工作中...
/exit

# 任务 2（几天后）
/spec payment-integration: ...
# 工作中...
/exit

# 重新启动
# → 看到两个未完成任务
/spec order-optimization  # 恢复任务 1
```

---

## 🎨 未来增强计划

1. **智能命名**（--smart 标志）
   - LLM 分析完整需求
   - 生成语义化名称
   - 支持多语言智能识别

2. **名称建议**
   - 检测到重复名称时提供备选
   - 自动添加版本号（如 `-v2`）

3. **任务分组**
   - 支持标签系统（如 `#backend`、`#frontend`）
   - 按标签过滤和显示

4. **进度可视化**
   - 图形化进度条
   - 预计完成时间

---

## 📚 相关文档

- [Spec Mode 目录结构与激活机制](../docs/Spec_Mode_目录结构与激活机制.md)
- [工作流阶段与步骤推进机制](../docs/工作流阶段与步骤推进机制.md)
- [Workflow Engine 完整调用流程](../docs/Workflow_Engine_完整调用流程.md)
