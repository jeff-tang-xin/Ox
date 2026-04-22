/// Ox configuration — placeholder for M2.
/// For now, only provides defaults needed by the terminal.
pub struct OxConfig {
    pub terminal: TerminalConfig,
}

pub struct TerminalConfig {
    pub output_ratio: u16,
}

impl Default for OxConfig {
    fn default() -> Self {
        Self {
            terminal: TerminalConfig { output_ratio: 85 },
        }
    }
}
