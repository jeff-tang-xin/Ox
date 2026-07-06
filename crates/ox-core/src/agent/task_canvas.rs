/// Mermaid Task Canvas - Symbolic short-term memory for long-horizon tasks
///
/// Inspired by TencentDB-Agent-Memory's approach:
/// - Encodes task state in high-density Mermaid syntax
/// - Cuts token usage while preserving full traceability via node_id
/// - Provides Agent with a "task map" to avoid getting lost in long tasks
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Task node status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum NodeStatus {
    Todo,
    Doing,
    Done,
    Failed,
}

impl NodeStatus {
    pub fn as_str(&self) -> &str {
        match self {
            NodeStatus::Todo => "todo",
            NodeStatus::Doing => "doing",
            NodeStatus::Done => "done",
            NodeStatus::Failed => "failed",
        }
    }

    pub fn emoji(&self) -> &str {
        match self {
            NodeStatus::Todo => "⬜",
            NodeStatus::Doing => "🔄",
            NodeStatus::Done => "✅",
            NodeStatus::Failed => "❌",
        }
    }
}

/// A single task node in the canvas
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskNode {
    /// Unique identifier for tracing (e.g., "step_001")
    pub node_id: String,
    /// Display label
    pub label: String,
    /// Current status
    pub status: NodeStatus,
    /// Optional reference to external file (for context offloading)
    pub ref_path: Option<String>,
    /// Brief description
    pub description: Option<String>,
}

impl TaskNode {
    pub fn new(node_id: &str, label: &str) -> Self {
        Self {
            node_id: node_id.to_string(),
            label: label.to_string(),
            status: NodeStatus::Todo,
            ref_path: None,
            description: None,
        }
    }

    pub fn with_status(mut self, status: NodeStatus) -> Self {
        self.status = status;
        self
    }

    pub fn with_ref(mut self, ref_path: &str) -> Self {
        self.ref_path = Some(ref_path.to_string());
        self
    }

    pub fn with_description(mut self, desc: &str) -> Self {
        self.description = Some(desc.to_string());
        self
    }

    /// Generate Mermaid node syntax
    pub fn to_mermaid_node(&self) -> String {
        let status_emoji = self.status.emoji();
        let ref_info = if let Some(ref path) = self.ref_path {
            format!("<br/>📄 <code>{}</code>", path)
        } else {
            String::new()
        };
        let desc_info = if let Some(ref desc) = self.description {
            format!("<br/>{}", desc)
        } else {
            String::new()
        };

        format!(
            "    {}[\"{} {}<br/>{ref}{desc}\"]",
            self.node_id, status_emoji, self.label, ref=ref_info, desc=desc_info
        )
    }
}

/// Dependency edge between nodes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskEdge {
    pub from: String,
    pub to: String,
    pub label: Option<String>,
}

impl TaskEdge {
    pub fn new(from: &str, to: &str) -> Self {
        Self {
            from: from.to_string(),
            to: to.to_string(),
            label: None,
        }
    }

    pub fn with_label(mut self, label: &str) -> Self {
        self.label = Some(label.to_string());
        self
    }

    /// Generate Mermaid edge syntax
    pub fn to_mermaid_edge(&self) -> String {
        if let Some(ref label) = self.label {
            format!("    {} -->|{}| {}", self.from, label, self.to)
        } else {
            format!("    {} --> {}", self.from, self.to)
        }
    }
}

/// Complete task canvas
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskCanvas {
    /// Canvas title
    pub title: String,
    /// All task nodes
    pub nodes: Vec<TaskNode>,
    /// Dependency edges
    pub edges: Vec<TaskEdge>,
    /// Current active step (node_id)
    pub current_step: Option<String>,
    /// Metadata
    pub metadata: HashMap<String, String>,
}

impl TaskCanvas {
    pub fn new(title: &str) -> Self {
        Self {
            title: title.to_string(),
            nodes: Vec::new(),
            edges: Vec::new(),
            current_step: None,
            metadata: HashMap::new(),
        }
    }

    /// Add a task node
    pub fn add_node(&mut self, node: TaskNode) {
        self.nodes.push(node);
    }

    /// Add an edge
    pub fn add_edge(&mut self, edge: TaskEdge) {
        self.edges.push(edge);
    }

    /// Update node status
    pub fn update_node_status(&mut self, node_id: &str, status: NodeStatus) {
        if let Some(node) = self.nodes.iter_mut().find(|n| n.node_id == node_id) {
            node.status = status;
        }
    }

    /// Set current step
    pub fn set_current_step(&mut self, node_id: &str) {
        self.current_step = Some(node_id.to_string());
    }

    /// Add metadata
    pub fn add_metadata(&mut self, key: &str, value: &str) {
        self.metadata.insert(key.to_string(), value.to_string());
    }

    /// Generate complete Mermaid diagram
    pub fn to_mermaid(&self) -> String {
        let mut output = String::new();

        // Header
        output.push_str(&format!("---\ntitle: {}\n---\n\n", self.title));
        output.push_str("graph TB\n");

        // Subgraph for current step highlighting
        if let Some(ref current) = self.current_step {
            output.push_str("    subgraph current [\"🎯 Current Step\"]\n");
            output.push_str("        direction TB\n");
            output.push_str(&format!("        {}\n", current));
            output.push_str("    end\n\n");
        }

        // Nodes
        output.push_str("    %% Task Nodes\n");
        for node in &self.nodes {
            output.push_str(&node.to_mermaid_node());
            output.push('\n');
        }

        output.push('\n');

        // Edges
        output.push_str("    %% Dependencies\n");
        for edge in &self.edges {
            output.push_str(&edge.to_mermaid_edge());
            output.push('\n');
        }

        // Styling
        output.push_str("\n    %% Status styling\n");
        output.push_str("    classDef done fill:#d4edda,stroke:#28a745,stroke-width:2px\n");
        output.push_str("    classDef doing fill:#fff3cd,stroke:#ffc107,stroke-width:2px\n");
        output.push_str("    classDef todo fill:#f8f9fa,stroke:#6c757d,stroke-width:1px\n");
        output.push_str("    classDef failed fill:#f8d7da,stroke:#dc3545,stroke-width:2px\n");
        output.push_str("    classDef current fill:#cce5ff,stroke:#007bff,stroke-width:3px\n\n");

        // Apply classes
        for node in &self.nodes {
            let class = match node.status {
                NodeStatus::Done => "done",
                NodeStatus::Doing => "doing",
                NodeStatus::Todo => "todo",
                NodeStatus::Failed => "failed",
            };
            output.push_str(&format!("    class {} {}\n", node.node_id, class));
        }

        // Highlight current step
        if let Some(ref current) = self.current_step {
            output.push_str(&format!("    class {} current\n", current));
        }

        output
    }

    /// Generate compact version for System Prompt injection (~200-400 tokens)
    pub fn to_compact_mermaid(&self) -> String {
        let mut output = String::new();
        output.push_str("```mermaid\ngraph LR\n");

        // Only show node_id and status emoji for compactness
        for node in &self.nodes {
            let emoji = node.status.emoji();
            output.push_str(&format!(
                "    {}[\"{} {}\"]\n",
                node.node_id, emoji, node.label
            ));
        }

        output.push('\n');

        for edge in &self.edges {
            output.push_str(&edge.to_mermaid_edge());
            output.push('\n');
        }

        output.push_str("```\n");
        output
    }

    /// Find node by ID (for traceability)
    pub fn find_node(&self, node_id: &str) -> Option<&TaskNode> {
        self.nodes.iter().find(|n| n.node_id == node_id)
    }

    /// Get all nodes with specific status
    pub fn nodes_by_status(&self, status: NodeStatus) -> Vec<&TaskNode> {
        self.nodes.iter().filter(|n| n.status == status).collect()
    }

    /// Count nodes by status
    pub fn status_summary(&self) -> HashMap<NodeStatus, usize> {
        let mut summary = HashMap::new();
        for node in &self.nodes {
            *summary.entry(node.status).or_insert(0) += 1;
        }
        summary
    }
}

/// Builder for TaskCanvas
pub struct TaskCanvasBuilder {
    canvas: TaskCanvas,
}

impl TaskCanvasBuilder {
    pub fn new(title: &str) -> Self {
        Self {
            canvas: TaskCanvas::new(title),
        }
    }

    pub fn add_step(mut self, node_id: &str, label: &str) -> Self {
        self.canvas.add_node(TaskNode::new(node_id, label));
        self
    }

    pub fn add_step_with_ref(mut self, node_id: &str, label: &str, ref_path: &str) -> Self {
        self.canvas
            .add_node(TaskNode::new(node_id, label).with_ref(ref_path));
        self
    }

    pub fn add_dependency(mut self, from: &str, to: &str) -> Self {
        self.canvas.add_edge(TaskEdge::new(from, to));
        self
    }

    pub fn add_dependency_with_label(mut self, from: &str, to: &str, label: &str) -> Self {
        self.canvas
            .add_edge(TaskEdge::new(from, to).with_label(label));
        self
    }

    pub fn set_current_step(mut self, node_id: &str) -> Self {
        self.canvas.set_current_step(node_id);
        self
    }

    pub fn build(self) -> TaskCanvas {
        self.canvas
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_canvas() {
        let canvas = TaskCanvasBuilder::new("Test Workflow")
            .add_step("step_1", "Analyze Requirements")
            .add_step("step_2", "Design Solution")
            .add_step("step_3", "Implement Code")
            .add_dependency("step_1", "step_2")
            .add_dependency("step_2", "step_3")
            .set_current_step("step_2")
            .build();

        let mermaid = canvas.to_mermaid();
        assert!(mermaid.contains("graph TB"));
        assert!(mermaid.contains("step_1"));
        assert!(mermaid.contains("step_2"));
        assert!(mermaid.contains("step_3"));
        assert!(mermaid.contains("current"));
    }

    #[test]
    fn test_compact_output() {
        let canvas = TaskCanvasBuilder::new("Compact Test")
            .add_step("s1", "First")
            .add_step("s2", "Second")
            .add_dependency("s1", "s2")
            .build();

        let compact = canvas.to_compact_mermaid();
        assert!(compact.contains("```mermaid"));
        assert!(compact.contains("s1"));
        assert!(compact.contains("s2"));
    }

    #[test]
    fn test_status_tracking() {
        let mut canvas = TaskCanvas::new("Status Test");
        canvas.add_node(TaskNode::new("n1", "Task 1").with_status(NodeStatus::Done));
        canvas.add_node(TaskNode::new("n2", "Task 2").with_status(NodeStatus::Doing));
        canvas.add_node(TaskNode::new("n3", "Task 3").with_status(NodeStatus::Todo));

        let summary = canvas.status_summary();
        assert_eq!(summary.get(&NodeStatus::Done), Some(&1));
        assert_eq!(summary.get(&NodeStatus::Doing), Some(&1));
        assert_eq!(summary.get(&NodeStatus::Todo), Some(&1));
    }

    #[test]
    fn test_node_traceability() {
        let canvas = TaskCanvasBuilder::new("Trace Test")
            .add_step_with_ref("step_1", "Research", ".ox/refs/step_001.md")
            .build();

        let node = canvas.find_node("step_1").unwrap();
        assert_eq!(node.ref_path, Some(".ox/refs/step_001.md".to_string()));
    }
}
