//! First-time project onboarding — generate project Skill files.
//!
//! Language/stack agnostic: Rust, Java, Python, JS/TS, React, Vue, Go, etc.
//!
//! Deliverables:
//! 1. `project-conventions.md` — 项目规范（工程/编码/构建）
//! 2. `project-business-guide.md` — 业务指导（领域、流程、模块职责）

use crate::message::Message;
use crate::runtime::{ensure_ox_project_scaffold, has_project_markers};
use std::path::{Path, PathBuf};

const ONBOARDING_MARKER: &str = "【首次进入 — 创建项目 Skill】";

pub const SKILL_CONVENTIONS: &str = "project-conventions.md";
pub const SKILL_BUSINESS: &str = "project-business-guide.md";
pub const SKILL_ARCHITECTURE_LEGACY: &str = "project-architecture.md";

pub fn skills_dir(project_root: &Path) -> PathBuf {
    project_root.join(".ox").join("skills")
}

pub fn conventions_path(project_root: &Path) -> PathBuf {
    skills_dir(project_root).join(SKILL_CONVENTIONS)
}

pub fn business_guide_path(project_root: &Path) -> PathBuf {
    skills_dir(project_root).join(SKILL_BUSINESS)
}

pub fn legacy_architecture_path(project_root: &Path) -> PathBuf {
    skills_dir(project_root).join(SKILL_ARCHITECTURE_LEGACY)
}

/// True when either deliverable is missing (legacy architecture counts as business guide).
pub fn needs_project_onboarding(project_root: &Path) -> bool {
    let dir = skills_dir(project_root);
    let has_conventions = dir.join(SKILL_CONVENTIONS).is_file();
    let has_business =
        dir.join(SKILL_BUSINESS).is_file() || dir.join(SKILL_ARCHITECTURE_LEGACY).is_file();
    !has_conventions || !has_business
}

/// No recognizable stack markers yet (empty or pre-init directory).
pub fn is_greenfield_project(project_root: &Path) -> bool {
    !has_project_markers(project_root)
}

/// Prepare `.ox/skills/` (and `.oxroot` marker when greenfield).
pub fn prepare_project_for_onboarding(project_root: &Path) -> std::io::Result<()> {
    ensure_ox_project_scaffold(project_root)
}

/// Both onboarding deliverables exist on disk.
pub fn onboarding_files_complete(project_root: &Path) -> bool {
    conventions_path(project_root).is_file()
        && (business_guide_path(project_root).is_file()
            || legacy_architecture_path(project_root).is_file())
}

/// Human-readable list of missing onboarding files.
pub fn missing_onboarding_files(project_root: &Path) -> Vec<String> {
    let mut missing = Vec::new();
    if !conventions_path(project_root).is_file() {
        missing.push(format!(".ox/skills/{SKILL_CONVENTIONS}"));
    }
    if !business_guide_path(project_root).is_file()
        && !legacy_architecture_path(project_root).is_file()
    {
        missing.push(format!(".ox/skills/{SKILL_BUSINESS}"));
    }
    missing
}

/// True when this turn was prepared for first-time project onboarding.
pub fn is_onboarding_turn(messages: &[Message]) -> bool {
    messages.iter().any(|m| {
        if let Message::System { content } = m {
            content.contains(ONBOARDING_MARKER)
        } else {
            false
        }
    })
}

/// True when assistant output in this batch signals onboarding completion.
pub fn turn_signals_onboarding_done(new_messages: &[Message]) -> bool {
    new_messages.iter().any(|m| {
        matches!(m, Message::Assistant { content, .. }
            if crate::agent::engine::WorkflowEngine::text_signals_done(content))
    })
}

/// Task text for skill reflect / round archive after onboarding.
pub fn extract_onboarding_task(messages: &[Message]) -> String {
    messages
        .iter()
        .find_map(|m| {
            if let Message::User { content } = m {
                if content.contains("首次进入") || content.contains("project-conventions") {
                    Some(content.clone())
                } else {
                    None
                }
            } else {
                None
            }
        })
        .unwrap_or_else(|| "首次进入 — 创建项目 Skill".to_string())
}

/// Reset CLI workflow engine after onboarding (agent ran without workflow).
pub fn finalize_cli_workflow_after_onboarding(
    engine: &mut crate::agent::engine::WorkflowEngine,
    session: &mut crate::message::Session,
    task: &str,
) -> anyhow::Result<()> {
    engine.reset_workflow();
    engine.set_variable("_current_user_request", task.to_string());
    crate::agent::user_round::set_turn_user_input(engine, task);
    engine.set_variable(crate::agent::user_round::ROUND_FINALIZED_KEY, "1".into());
    crate::agent::post_edit_verification::clear_verify_state(engine);
    crate::agent::workflow_phases::clear_phase(engine);
    let wf_id = crate::agent::workflow::DEFAULT_WORKFLOW_ID;
    session.persist_workflow_state("pipeline", wf_id, 0, None)?;
    Ok(())
}

/// System directive injected before the user onboarding task (no 4-step workflow).
pub fn onboarding_system_directive(greenfield: bool) -> String {
    let greenfield_note = if greenfield {
        "\n\n**空项目/未初始化**：project_detect 可能无结果。仍须写两份 Skill，\
         基于 file_list 实际内容；缺失信息用「待补充」标注，禁止编造技术栈或业务。\n"
    } else {
        ""
    };
    format!(
        "【首次进入 — 创建项目 Skill】\n\
         \n\
         目标：为**后续每次对话**准备两份固定参考手册，适用于**任意语言/框架**（Java、Python、JS/TS、React、Vue、Go、Rust…）。\n\
         \n\
         | 文件 | 回答的问题 | 禁止写成 |\n\
         |------|------------|----------|\n\
         | project-conventions.md | 在本项目里代码怎么写、怎么构建、怎么验证？ | 业务介绍、模块职责 |\n\
         | project-business-guide.md | 项目做什么？术语/流程？改功能看哪？ | 构建命令、格式化工具 |\n\
         \n\
         **流程**：所有动作都走 `complete_and_check({{\"action\":\"...\",\"params\":{{...}}}})`：project_detect → file_list（单层逐层）→ file_read 关键配置/入口 → **分别** file_write → finish(content=\"## Done...\")。\n\
         **技术栈**：只写 project_detect 与配置文件**实际检测到**的内容，不要默认某一语言。\n\
         **禁止**：四步工作流 JSON；通用编程常识；臆造路径/命令；两篇混写。\n\
         **篇幅**：每个 Skill ≤1500 字；大仓库用要点，避免 file_write 被模型截断。{greenfield_note}"
    )
}

/// User task message for the onboarding agent turn.
pub fn build_onboarding_user_prompt(project_root: &Path) -> String {
    let root = project_root.display();
    let greenfield = is_greenfield_project(project_root);
    let mode = if greenfield {
        "【模式：空项目 / 未检测到工程标记】"
    } else {
        "【模式：已有工程】"
    };

    format!(
        r####"{mode}
首次进入目录 `{root}`。请分析**当前实际文件**并创建两份 Skill（任意技术栈通用）。

## 核心区分

- **项目规范** = 工程手册：语言/框架、目录、构建与测试命令、代码风格
- **业务指导** = 领域手册：产品定位、术语、业务流程、模块/包职责

## 技术栈识别（按 project_detect 结果选用，勿臆测）

| 生态 | 常见标记文件 | 典型 build / test 命令（写入规范时须与项目一致） |
|------|--------------|--------------------------------------------------|
| Java/Kotlin | pom.xml, build.gradle* | `mvn package`, `mvn test`, `./gradlew build` |
| Python | pyproject.toml, requirements.txt | `pip install -e .`, `pytest`, `python -m unittest` |
| Node / TS | package.json | `npm install`, `npm run build`, `npm test` |
| React / Vue | package.json + src/ | 同上 + `npm run dev`（若 scripts 中有） |
| Go | go.mod | `go build ./...`, `go test ./...` |
| Rust | Cargo.toml | `cargo build`, `cargo test` |
| 其他 | CMakeLists.txt, Makefile, … | 以 README / 配置文件中的命令为准 |

❌ 错误：未检测到 package.json 却写 npm 命令；两份文档内容重复  
✅ 正确：规范写**本项目**真实命令；业务写「用户登录流程 → `src/auth/`」

## 交付物（必须 file_write，缺一不可）

### 1) `.ox/skills/project-conventions.md`

```yaml
---
name: project-conventions
description: 本项目的编码与工程规范
scope: project
---
```

正文（中文，150–400 字）：
- **技术栈**：project_detect + 实际配置文件（无则写「待初始化」）
- **目录约定**：file_list 结果中的关键路径
- **命名与风格**：项目内真实工具（eslint、prettier、checkstyle、black、rustfmt…，有则写）
- **常用命令**：完整 shell 命令（至少 build + test；无则写「待补充」并说明原因）
- **MUST / MUST NOT**：3–6 条，附真实路径

### 2) `.ox/skills/project-business-guide.md`

```yaml
---
name: project-business-guide
description: 本项目的业务领域与模块职责指南
scope: project
---
```

正文（中文，200–500 字）：
- **项目定位**（一句话；无 README 时根据目录名/已有文件合理描述，标注「待确认」）
- **核心术语**（3–8 个；来自 README、包名、注释；不足可少写）
- **主业务流程**（2–4 条：场景 → 目录/模块）
- **模块职责表**（包/module/目录 → 职责）
- **Agent 指引**：加功能 / 修 bug 时优先打开的路径

## 探索顺序（唯一工具出口）
每一步都必须调用 `complete_and_check`，不要输出裸工具名或旧式函数调用。
1. `complete_and_check({{"action":"project_detect","params":{{}}}})` — 记录检测到的语言与工具
2. `complete_and_check({{"action":"file_list","params":{{"path":"."}}}})`，再按实际目录单层逐层 list
3. `complete_and_check({{"action":"file_read","params":{{"path":"README.md","offset":0,"limit":120}}}})`：README、构建配置（如 pom.xml / package.json / pyproject.toml / go.mod / Cargo.toml）、应用入口
4. 按需 `complete_and_check({{"action":"code_search","params":{{"query":"..."}}}})` / `complete_and_check({{"action":"find_symbol","params":{{"name":"..."}}}})`
5. 分别 `complete_and_check({{"action":"file_write","params":{{"path":".ox/skills/project-conventions.md","content":"..."}}}})` 和 `complete_and_check({{"action":"file_write","params":{{"path":".ox/skills/project-business-guide.md","content":"..."}}}})`

## 空项目特别说明
若几乎无源码：规范中写明当前状态、建议的初始化步骤占位；业务中写明「新项目，业务待定义」及未来模块规划占位。**禁止编造不存在的文件或命令。**

## 完成
两个文件写入后调用 `complete_and_check({{"action":"finish","params":{{"content":"## Done\n已创建 .ox/skills/project-conventions.md 与 .ox/skills/project-business-guide.md"}}}})` 并列出路径。

## 大仓库注意（Java 多模块等）
- 每个 Skill **控制在 1500 字以内**；用要点，不要贴大段代码
- 探索优先 `file_list` + `file_read` README/pom.xml/主模块入口；**不要**对整仓 `file_search **/*`
- `file_search` 必须带 pattern（如 `*.java`），且 path 缩小到单模块目录
- 若 file_write 报 JSON Truncation：缩短内容后重试，或分两次写两个文件

## 去重（禁止另建同义 Skill）
- 只允许上述两个固定文件名；不要创建 project-coding-standards、project-architecture-patterns 等别名
- 后续更新用 edit_file，或 file_write 同一文件并设 `"merge": true`"####
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn needs_onboarding_when_empty() {
        let tmp = std::env::temp_dir().join(format!("ox_onboard_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(skills_dir(&tmp)).unwrap();
        assert!(needs_project_onboarding(&tmp));
        assert!(is_greenfield_project(&tmp));
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn prepare_creates_oxroot_on_greenfield() {
        let tmp = std::env::temp_dir().join(format!("ox_scaffold_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        prepare_project_for_onboarding(&tmp).unwrap();
        assert!(tmp.join(".oxroot").is_file());
        assert!(skills_dir(&tmp).is_dir());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn legacy_architecture_satisfies_business_slot() {
        let tmp = std::env::temp_dir().join(format!("ox_onboard_legacy_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(skills_dir(&tmp)).unwrap();
        fs::write(conventions_path(&tmp), "# ok").unwrap();
        fs::write(legacy_architecture_path(&tmp), "# ok").unwrap();
        assert!(!needs_project_onboarding(&tmp));
        assert!(onboarding_files_complete(&tmp));
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn onboarding_files_complete_requires_both() {
        let tmp = std::env::temp_dir().join(format!("ox_onboard_both_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(skills_dir(&tmp)).unwrap();
        fs::write(conventions_path(&tmp), "# ok").unwrap();
        assert!(!onboarding_files_complete(&tmp));
        fs::write(business_guide_path(&tmp), "# ok").unwrap();
        assert!(onboarding_files_complete(&tmp));
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn is_onboarding_turn_detects_directive() {
        let msgs = vec![
            Message::system("hello"),
            Message::system(&onboarding_system_directive(false)),
        ];
        assert!(is_onboarding_turn(&msgs));
        assert!(!is_onboarding_turn(&[Message::user("hi")]));
    }

    #[test]
    fn onboarding_prompt_uses_unified_tool_calls() {
        let dir = std::env::temp_dir().join(format!("ox_onboard_prompt_{}", std::process::id()));
        let prompt = build_onboarding_user_prompt(&dir);
        let directive = onboarding_system_directive(false);

        assert!(prompt.contains("complete_and_check"));
        assert!(directive.contains("complete_and_check"));
        assert!(prompt.contains("\"action\":\"project_detect\""));
        assert!(prompt.contains("\"action\":\"file_write\""));
        assert!(prompt.contains("\"action\":\"finish\""));
        assert!(!prompt.contains("project_detect()"));
    }
    #[test]
    fn turn_signals_onboarding_done_detects_assistant() {
        let msgs = vec![Message::Assistant {
            content: "## Done\n\nskills written".into(),
            tool_calls: vec![],
            reasoning_content: None,
        }];
        assert!(turn_signals_onboarding_done(&msgs));
    }
}
