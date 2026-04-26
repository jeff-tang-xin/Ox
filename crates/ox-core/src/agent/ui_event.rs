/// UI sent to Agent events (bidirectional channel: UI→Agent).
#[derive(Debug, Clone)]
pub enum UiToAgentEvent {
    /// User confirmed or denied a tool execution.
    ToolConfirmation {
        tool_call_id: String,
        decision: ConfirmationDecision,
    },
    /// User injected an interjection message during agent run.
    Interjection(String),
}

/// User's decision on a tool confirmation request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmationDecision {
    /// Allow this tool execution.
    Allow,
    /// Deny this tool execution.
    Deny,
    /// Allow and add to trust list (skip future confirmations for this tool).
    TrustAlways,
}
