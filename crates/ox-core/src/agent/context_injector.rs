//! Context injector — task anchoring and periodic knowledge re-injection.
//!
//! In long turns, the LLM's attention to the system prompt fades. This module
//! injects task reminders and refreshed knowledge context into the message stream.

use crate::message::Message;
use crate::tools::ToolContext;
use std::sync::Arc;

/// Inject task anchoring and periodic knowledge refresh into messages.
///
/// Called at the start of each LLM iteration (after iteration 0).
/// - Every iteration: injects a task reminder with the original user request
/// - Every 3 iterations: refreshes knowledge context from KnowledgeEngine
pub fn inject_context(
    messages: &mut Vec<Message>,
    user_task: &Option<String>,
    iteration: u32,
    tool_ctx: &Arc<ToolContext>,
) {
    if iteration == 0 {
        return;
    }

    if let Some(task) = user_task {
        let anchor = if task.chars().count() > 200 {
            format!("{}...", task.chars().take(200).collect::<String>())
        } else {
            task.clone()
        };

        let mut reminder = format!(
            "📋 **Current task** (reminder): {}\nStay focused. Do NOT deviate.",
            anchor
        );

        if iteration % 3 == 0 {
            let knowledge = Arc::clone(&tool_ctx.knowledge);
            let task_owned = task.clone();
            // Use try_read — if background indexing holds the lock, skip re-injection
            if let Ok(engine) = knowledge.try_read() {
                if let Ok(hits) = engine.retrieve_for_context(&task_owned, "", 5) {
                    if !hits.is_empty() {
                        reminder.push_str("\n\n📚 **Refreshed Knowledge Context**:\n");
                        for hit in hits.iter().take(5) {
                            let preview: String =
                                hit.entity.content.chars().take(120).collect();
                            reminder.push_str(&format!(
                                "- [{}] {}\n",
                                hit.entity.kind.as_str(),
                                preview
                            ));
                        }
                        // Early iterations: strongly discourage re-search
                        if iteration < 4 {
                            reminder.push_str(
                                "→ Use file_read if you need more detail. Avoid calling memory_search/find_symbol.\n",
                            );
                        }
                    }
                }
            }
        }

        messages.push(Message::system(&reminder));
    }
}
