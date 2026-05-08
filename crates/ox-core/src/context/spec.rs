use anyhow::Result;
use std::path::Path;

/// Load spec from file, relative to project root.
/// Returns the content if file exists, empty string if not.
pub fn load_spec(project_root: &Path, file_path: &str) -> Result<String> {
    let spec_path = project_root.join(file_path);

    if !spec_path.exists() {
        tracing::debug!("Spec file not found: {}", spec_path.display());
        return Ok(String::new());
    }

    let content = std::fs::read_to_string(&spec_path)?;
    tracing::info!("Loaded spec from: {}", spec_path.display());
    Ok(content)
}

/// Save spec content to file, relative to project root.
pub fn save_spec(project_root: &Path, file_path: &str, content: &str) -> Result<String> {
    let spec_path = project_root.join(file_path);

    // Create parent directories if needed
    if let Some(parent) = spec_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    std::fs::write(&spec_path, content)?;
    tracing::info!("Saved spec to: {}", spec_path.display());
    Ok(spec_path.to_string_lossy().to_string())
}

/// Check if spec file exists.
pub fn spec_exists(project_root: &Path, file_path: &str) -> bool {
    project_root.join(file_path).exists()
}

/// AI 判断任务类型，决定是否需要生成规范文件。
/// 这个 prompt 片段用于在系统提示词中指导 AI 判断任务类型。
pub const TASK_TYPE_PROMPT: &str = r##"## Task Classification (MANDATORY for Spec Mode)

When in SPEC mode, you MUST analyze the user's request and determine the task type:

### Task Types & Required Artifacts

| Task Type | When to Use | Required Artifacts |
|-----------|-------------|---------------------|
| **Complex/Multi-file** | New features, refactors, cross-file changes | `spec.md` + `task.md` |
| **Simple/Quick** | Single file edits, bug fixes, small changes | Optional `task.md` |
| **Exploratory** | Questions, analysis, learning | No artifacts needed |
| **Documentation** | Docs, comments, READMEs | Optional spec update |

### Spec Mode Workflow

1. **Analyze** -> Classify task type
2. **If Complex**:
   - Generate `spec.md` (or update existing) with: goal, constraints, output files
   - Generate `task.md` with: step-by-step plan, verification criteria
   - Show both to user for confirmation before proceeding
3. **If Simple**: Proceed directly but track in `task.md`
4. **If Exploratory**: Proceed directly, no artifacts needed

### Spec File Format

```markdown
# Task Specification

## Goal
[What to achieve - one sentence]

## Constraints
- [Technical constraints, style, testing requirements]
- [Performance, compatibility requirements]

## Output Files
- `src/file1.rs` - [description]
- `tests/file1_test.rs` - [description]

## Verification
- [ ] Criterion 1
- [ ] Criterion 2
```

### Task File Format

```markdown
# Task Plan

## Steps
1. [Step] -> verify: [check]
2. [Step] -> verify: [check]

## Status
- [x] Step 1 completed
- [ ] Step 2 in progress
```"##;
