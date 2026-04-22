pub mod directory;
mod project;

pub use directory::{change_directory, DirectoryChangeResult};
pub use project::{compute_project_id, find_project_root};

use std::path::PathBuf;

/// Detected operating system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Os {
    Windows,
    Linux,
    MacOS,
    Other(String),
}

impl std::fmt::Display for Os {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Windows => write!(f, "Windows"),
            Self::Linux => write!(f, "Linux"),
            Self::MacOS => write!(f, "macOS"),
            Self::Other(s) => write!(f, "{s}"),
        }
    }
}

/// Detected shell information for tool execution.
#[derive(Debug, Clone)]
pub struct ShellInfo {
    /// Full path to the shell binary.
    pub path: PathBuf,
    /// Short name: "pwsh", "cmd", "bash", "zsh", etc.
    pub name: String,
    /// Arguments to pass before the user command, e.g. ["-Command"] for pwsh, ["/C"] for cmd, ["-c"] for sh.
    pub exec_prefix: Vec<String>,
}

/// Runtime environment detected at startup.
/// Injected into System Prompt so LLM can emit correct OS-specific commands.
#[derive(Debug, Clone)]
pub struct RuntimeEnvironment {
    pub os: Os,
    pub arch: String,
    pub shell: ShellInfo,
    pub home_dir: PathBuf,
    pub working_dir: PathBuf,
    pub project_root: Option<PathBuf>,
    pub project_id: String,
}

impl RuntimeEnvironment {
    /// Format a summary string for the startup banner.
    pub fn banner_summary(&self) -> String {
        let project_name = self
            .project_root
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "(none)".into());

        let project_type = self
            .project_root
            .as_ref()
            .map(|r| detect_project_language(r))
            .unwrap_or_default();

        let type_suffix = if project_type.is_empty() {
            String::new()
        } else {
            format!(" ({project_type})")
        };

        format!(
            "Project: {}{} | {} ({}) | {}",
            project_name,
            type_suffix,
            self.os,
            self.shell.name,
            self.working_dir.display(),
        )
    }

    /// Generate the environment block for System Prompt injection.
    pub fn system_prompt_block(&self) -> String {
        let project_root_str = self
            .project_root
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(none)".into());

        let sep = if self.os == Os::Windows { "\\" } else { "/" };

        format!(
            "## Environment\n\
             - OS: {} ({})\n\
             - Shell: {} ({})\n\
             - Working directory: {}\n\
             - Project root: {}\n\
             - Path separator: {}",
            self.os,
            self.arch,
            self.shell.name,
            self.shell.path.display(),
            self.working_dir.display(),
            project_root_str,
            sep,
        )
    }
}

/// Detect the full runtime environment. Called once at startup.
pub fn detect_runtime() -> RuntimeEnvironment {
    let os = match std::env::consts::OS {
        "windows" => Os::Windows,
        "linux" => Os::Linux,
        "macos" => Os::MacOS,
        other => Os::Other(other.to_string()),
    };

    let shell = detect_shell(&os);
    let working_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let project_root = find_project_root(&working_dir);
    let project_id = compute_project_id(&project_root, &working_dir);
    let home_dir = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));

    RuntimeEnvironment {
        os,
        arch: std::env::consts::ARCH.into(),
        shell,
        home_dir,
        working_dir,
        project_root,
        project_id,
    }
}

/// Detect the preferred shell for tool execution.
fn detect_shell(os: &Os) -> ShellInfo {
    match os {
        Os::Windows => {
            // Prefer PowerShell Core → Windows PowerShell → cmd.
            if which::which("pwsh").is_ok() {
                ShellInfo {
                    path: "pwsh.exe".into(),
                    name: "pwsh".into(),
                    exec_prefix: vec!["-Command".into()],
                }
            } else if which::which("powershell").is_ok() {
                ShellInfo {
                    path: "powershell.exe".into(),
                    name: "powershell".into(),
                    exec_prefix: vec!["-Command".into()],
                }
            } else {
                ShellInfo {
                    path: "cmd.exe".into(),
                    name: "cmd".into(),
                    exec_prefix: vec!["/C".into()],
                }
            }
        }
        _ => {
            let shell_path =
                std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
            let name = std::path::Path::new(&shell_path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "sh".into());
            ShellInfo {
                path: shell_path.into(),
                name,
                exec_prefix: vec!["-c".into()],
            }
        }
    }
}

/// Detect the primary language of a project by checking marker files.
fn detect_project_language(root: &std::path::Path) -> String {
    let markers: &[(&str, &str)] = &[
        ("Cargo.toml", "Rust"),
        ("package.json", "Node.js"),
        ("go.mod", "Go"),
        ("pyproject.toml", "Python"),
        ("setup.py", "Python"),
        ("pom.xml", "Java"),
        ("build.gradle", "Java"),
        ("*.csproj", "C#"),
        ("CMakeLists.txt", "C/C++"),
    ];

    for (marker, lang) in markers {
        if marker.contains('*') {
            // Glob pattern — check if any matching file exists.
            if let Ok(pattern) = glob::glob(&root.join(marker).to_string_lossy())
                && pattern.into_iter().any(|e| e.is_ok()) {
                    return (*lang).into();
                }
        } else if root.join(marker).exists() {
            return (*lang).into();
        }
    }

    String::new()
}
