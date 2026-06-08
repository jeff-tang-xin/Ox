// Parsed slash command.
#[derive(Debug, Clone)]
pub enum SlashCommand {
    Help {
        topic: Option<String>,
    },
    Exit,
    Cancel,
    New,
    Clear,
    Clean,
    Cost,
    Plan,
    Trust {
        tools: Vec<String>,
        all: bool,
    },
    Untrust,
    Model {
        name: Option<String>,
    },
    Cd {
        path: Option<String>,
    },
    Debug,
    Init,
    Sessions,
    Resume {
        filename: String,
    },
    Remember {
        content: String,
    },
    Forget {
        keyword: String,
    },
    Memory,
    Feedback {
        category: String,
    },
    Reload,
    Spec {
        /// Subcommand: status, show, on, off, edit, clear, or inline content
        action: String,
    },
    Free,
    // Workflow confirmation commands
    Approve, // /Y - Approve and proceed
    Reject,  // /N - Reject and abort
    Revise,  // /O - Provide feedback for revision
    Unknown {
        cmd: String,
    },
}

/// Parse a slash command string into a structured command.
pub fn parse_slash_command(cmd: &str, args: &str) -> SlashCommand {
    match cmd {
        "help" | "h" => SlashCommand::Help {
            topic: if args.is_empty() {
                None
            } else {
                Some(args.to_string())
            },
        },
        "exit" | "quit" | "q" => SlashCommand::Exit,
        "cancel" => SlashCommand::Cancel,
        "new" => SlashCommand::New,
        "clear" | "cls" => SlashCommand::Clear,
        "clean" => SlashCommand::Clean,
        "cost" => SlashCommand::Cost,
        "plan" => SlashCommand::Plan,
        "trust" => {
            if args == "--all" {
                SlashCommand::Trust {
                    tools: Vec::new(),
                    all: true,
                }
            } else {
                let tools: Vec<String> = args.split_whitespace().map(|s| s.to_string()).collect();
                SlashCommand::Trust { tools, all: false }
            }
        }
        "untrust" => SlashCommand::Untrust,
        "model" => SlashCommand::Model {
            name: if args.is_empty() {
                None
            } else {
                Some(args.to_string())
            },
        },
        "cd" => SlashCommand::Cd {
            path: if args.is_empty() {
                None
            } else {
                Some(args.to_string())
            },
        },
        "debug" => SlashCommand::Debug,
        "init" => SlashCommand::Init,
        "sessions" => SlashCommand::Sessions,
        "resume" => SlashCommand::Resume {
            filename: args.to_string(),
        },
        "remember" => SlashCommand::Remember {
            content: args.to_string(),
        },
        "forget" => SlashCommand::Forget {
            keyword: args.to_string(),
        },
        "memory" => SlashCommand::Memory,
        "feedback" => SlashCommand::Feedback {
            category: args.to_string(),
        },
        "reload" => SlashCommand::Reload,
        "spec" => SlashCommand::Spec {
            action: args.to_string(),
        },
        "free" => SlashCommand::Free,
        // Workflow confirmation commands
        "y" | "Y" => SlashCommand::Approve,
        "n" | "N" => SlashCommand::Reject,
        "o" | "O" => SlashCommand::Revise,
        _ => SlashCommand::Unknown {
            cmd: cmd.to_string(),
        },
    }
}

/// Generate help text for all slash commands.
pub fn help_text(topic: Option<&str>) -> String {
    match topic {
        Some("trust") => "\
/trust <tool>       Trust a tool for this session (skip confirmation)
/trust --all        Trust all non-dangerous tools
/untrust            Revoke all trust"
            .to_string(),
        Some("cost") => "\
/cost               Show token usage and cost breakdown"
            .to_string(),
        Some("plan") => "\
/plan               Show current task plan"
            .to_string(),
        Some("spec") => "\
/spec               Show spec status
/spec status       Show spec status
/spec show         Display current spec content
/spec on           Activate spec mode (load from file or create new)
/spec off          Deactivate spec mode
/spec edit         Enter edit mode (next input becomes spec)
/spec clear        Clear spec and delete file
/spec <content>    Set spec content directly"
            .to_string(),
        Some(other) => format!("Unknown help topic: {other}. Type /help for all commands."),
        None => "\
Commands:
  /help [topic]     Show help (topics: trust, cost, plan, spec)
  /exit             Exit Ox
  /cancel           Cancel current operation (e.g., spec edit mode)
  /new              Start a new session (archives current)
  /clean            Clear all messages in current session
  /clear            Clear the screen
  /cost             Show token usage and cost summary
  /plan             Show current task plan
  /sessions         List archived sessions
  /resume <file>    Resume an archived session
  /trust <tool>     Trust a tool (skip confirmation this session)
  /trust --all      Trust all non-dangerous tools
  /untrust          Revoke all trust
  /model [name]     Show or switch model
  /cd [path]        Show or change working directory
  /init             Create default config (~/.ox/config.toml)
  /debug            Show debug info
  /spec <action>    Spec mode (on/off/edit/status) - structured workflow
  /free             Switch to free mode (deactivate any workflow)
  /reload           Reload session from disk (JSONL)

  /y                Approve and proceed to next phase (workflow confirmation)
  /n                Reject and abort workflow
  /o                Provide feedback for revision"
            .to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_help() {
        let cmd = parse_slash_command("help", "");
        assert!(matches!(cmd, SlashCommand::Help { topic: None }));
    }

    #[test]
    fn parse_trust_all() {
        let cmd = parse_slash_command("trust", "--all");
        assert!(matches!(cmd, SlashCommand::Trust { all: true, .. }));
    }

    #[test]
    fn parse_trust_specific() {
        let cmd = parse_slash_command("trust", "file_write edit_file");
        if let SlashCommand::Trust { tools, all } = cmd {
            assert!(!all);
            assert_eq!(tools, vec!["file_write", "edit_file"]);
        } else {
            panic!("Expected Trust");
        }
    }

    #[test]
    fn parse_unknown() {
        let cmd = parse_slash_command("foobar", "");
        assert!(matches!(cmd, SlashCommand::Unknown { .. }));
    }

    #[test]
    fn parse_init() {
        let cmd = parse_slash_command("init", "");
        assert!(matches!(cmd, SlashCommand::Init));
    }

    #[test]
    fn parse_spec_status() {
        let cmd = parse_slash_command("spec", "status");
        if let SlashCommand::Spec { action } = cmd {
            assert_eq!(action, "status");
        } else {
            panic!("Expected Spec");
        }
    }

    #[test]
    fn parse_spec_show() {
        let cmd = parse_slash_command("spec", "show");
        if let SlashCommand::Spec { action } = cmd {
            assert_eq!(action, "show");
        } else {
            panic!("Expected Spec");
        }
    }

    #[test]
    fn parse_spec_inline_content() {
        let cmd = parse_slash_command("spec", "Build a REST API with auth");
        if let SlashCommand::Spec { action } = cmd {
            assert_eq!(action, "Build a REST API with auth");
        } else {
            panic!("Expected Spec");
        }
    }

    #[test]
    fn parse_cancel() {
        let cmd = parse_slash_command("cancel", "");
        assert!(matches!(cmd, SlashCommand::Cancel));
    }
}
