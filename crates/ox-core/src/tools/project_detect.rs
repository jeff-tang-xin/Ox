use serde_json::{Value, json};

use super::{SafetyLevel, Tool, ToolContext, ToolOutput};

pub struct ProjectDetectTool;

#[async_trait::async_trait]
impl Tool for ProjectDetectTool {
    fn name(&self) -> &str {
        "project_detect"
    }

    fn description(&self) -> &str {
        "Detect project type/language by marker files in ONE directory (default: project root). \
         Does not scan subdirectories — for monorepos, also file_list subdirs. Call once at start of planning."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Directory to analyze. Default: working directory."
                }
            },
            "examples": [
                {},
                {"path": "my-project/"}
            ]
        })
    }

    fn safety_level(&self) -> SafetyLevel {
        SafetyLevel::Safe
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let base = if let Some(p) = args.get("path").and_then(|p| p.as_str()) {
            // Normalize path: trim whitespace and standardize separators
            let normalized_path = p.trim().replace('\\', "/");
            let resolved = ctx.working_dir.join(&normalized_path);

            match crate::safety::validate_path_within_workdir(&resolved, &ctx.working_dir) {
                Ok(validated) => validated,
                Err(e) => return ToolOutput::error(format!("Path validation failed: {e}")),
            }
        } else {
            ctx.working_dir.to_path_buf()
        };

        let mut info = Vec::new();

        let markers: &[(&str, &str, &str)] = &[
            ("Cargo.toml", "Rust", "Cargo"),
            ("package.json", "JavaScript/TypeScript", "npm/yarn/pnpm"),
            ("pnpm-workspace.yaml", "JavaScript (monorepo)", "pnpm"),
            ("go.mod", "Go", "Go Modules"),
            ("pyproject.toml", "Python", "pyproject"),
            ("setup.py", "Python", "setuptools"),
            ("requirements.txt", "Python", "pip"),
            ("Pipfile", "Python", "pipenv"),
            ("pom.xml", "Java", "Maven"),
            ("build.gradle", "Java/Kotlin", "Gradle"),
            ("build.gradle.kts", "Kotlin/Java", "Gradle"),
            ("settings.gradle", "Java/Kotlin", "Gradle"),
            ("CMakeLists.txt", "C/C++", "CMake"),
            ("Makefile", "C/C++/Mixed", "Make"),
            ("Gemfile", "Ruby", "Bundler"),
            ("composer.json", "PHP", "Composer"),
            ("*.csproj", "C#", ".NET"),
            ("global.json", "C#/.NET", ".NET SDK"),
            ("pubspec.yaml", "Dart", "Flutter/Dart"),
        ];

        for (file, lang, build) in markers {
            if file.contains('*') {
                if let Ok(pattern) = glob::glob(&base.join(file).to_string_lossy())
                    && pattern.into_iter().any(|e| e.is_ok())
                {
                    info.push(format!("Language: {lang} (build: {build})"));
                }
            } else if base.join(file).exists() {
                info.push(format!("Language: {lang} (build: {build})"));
            }
        }

        // Check for VCS.
        if base.join(".git").exists() {
            info.push("VCS: Git".to_string());
        }

        // Check for config files.
        let configs: &[(&str, &str)] = &[
            (".eslintrc.json", "ESLint"),
            (".prettierrc", "Prettier"),
            ("tsconfig.json", "TypeScript"),
            ("rustfmt.toml", "rustfmt"),
            (".clippy.toml", "Clippy"),
            ("docker-compose.yml", "Docker Compose"),
            ("Dockerfile", "Docker"),
            (".github/workflows", "GitHub Actions"),
        ];

        for (file, tool) in configs {
            if base.join(file).exists() {
                info.push(format!("Tool: {tool}"));
            }
        }

        if info.is_empty() {
            ToolOutput::success(format!(
                "No recognized project markers found in {}",
                base.display()
            ))
        } else {
            info.insert(0, format!("Project root: {}", base.display()));
            ToolOutput::success(info.join("\n"))
        }
    }
}
