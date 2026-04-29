/// Parsed slash command.
#[derive(Debug, Clone)]
pub enum SlashCommand {
    Help { topic: Option<String> },
    Exit,
    New,
    Clear,
    Clean,
    Cost,
    Plan,
    Trust { tools: Vec<String>, all: bool },
    Untrust,
    Model { name: Option<String> },
    Cd { path: Option<String> },
    Debug,
    Init,
    Sessions,
    Resume { filename: String },
    Remember { content: String },
    Forget { keyword: String },
    Memory,
    Feedback { category: String },
    Persona { action: String },
    Discuss { question: Option<String>, rounds: Option<u8>, verbose: bool },
    Council { action: String },
    Unknown { cmd: String },
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
                let tools: Vec<String> = args
                    .split_whitespace()
                    .map(|s| s.to_string())
                    .collect();
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
        "persona" => SlashCommand::Persona {
            action: args.to_string(),
        },
        "discuss" => {
            let mut rounds = None;
            let mut verbose = false;
            let mut question_parts = Vec::new();
            for part in args.split_whitespace() {
                if part == "--verbose" || part == "-v" {
                    verbose = true;
                } else if part == "--rounds" || part == "-r" {
                    // next part is rounds count, handled below
                    rounds = None; // placeholder
                } else if let Some(prev) = question_parts.last() {
                    if prev == "--rounds" || prev == "-r" {
                        question_parts.pop();
                        if let Ok(r) = part.parse::<u8>() {
                            rounds = Some(r);
                        }
                        continue;
                    }
                    question_parts.push(part.to_string());
                } else {
                    question_parts.push(part.to_string());
                }
            }
            let question = if question_parts.is_empty() {
                None
            } else {
                Some(question_parts.join(" "))
            };
            SlashCommand::Discuss { question, rounds, verbose }
        }
        "council" => SlashCommand::Council {
            action: args.to_string(),
        },
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
        Some(other) => format!("Unknown help topic: {other}. Type /help for all commands."),
        None => "\
Commands:
  /help [topic]     Show help (topics: trust, cost, plan)
  /exit             Exit Ox
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
  /discuss [q]      Start council debate (--rounds N, --verbose)
  /council <action> Council actions (last, stats)"
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
        let cmd = parse_slash_command("trust", "file_write file_patch");
        if let SlashCommand::Trust { tools, all } = cmd {
            assert!(!all);
            assert_eq!(tools, vec!["file_write", "file_patch"]);
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
}
