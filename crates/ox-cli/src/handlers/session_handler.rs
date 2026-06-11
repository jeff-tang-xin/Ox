//! Session management — unified session switching and sidebar rebuild.
//!
//! Previously, `SessionAction::New`, `SessionAction::Resume`, and
//! `SessionAction::SwitchNext` each contained ~100 lines of nearly identical
//! sidebar rebuild logic. This module provides a single `rebuild_sidebar`
//! function and clean handler functions for each session action.

use std::path::Path;
use std::sync::Arc;

use ox_core::message::Session;
use ox_core::runtime::RuntimeEnvironment;

use crate::terminal::app::{App, SessionEntry};

/// Rebuild the sidebar session list from disk.
///
/// Called exactly once — shared by New/Resume/SwitchNext and initial loading.
/// Scans all project directories under `sessions_root` and populates `app.sessions`.
pub fn rebuild_sidebar(
    app: &mut App,
    sessions_root: &Path,
    active_project_id: &str,
    active_session_display_name: &str,
) {
    app.sessions.clear();
    if !sessions_root.exists() {
        return;
    }

    let Ok(project_dirs) = std::fs::read_dir(sessions_root) else {
        return;
    };

    for project_entry in project_dirs.flatten() {
        let project_path = project_entry.path();
        if !project_path.is_dir() {
            continue;
        }

        let project_id = project_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());

        let archived = Session::list_archived(&project_path);
        for (filename, info) in archived {
            app.sessions.push(SessionEntry {
                id: filename,
                project_id: project_id.clone(),
                info,
                is_active: false,
            });
        }
    }

    // Insert current active session at the top
    app.sessions.insert(
        0,
        SessionEntry {
            id: "session.jsonl".to_string(),
            project_id: active_project_id.to_string(),
            info: active_session_display_name.to_string(),
            is_active: true,
        },
    );
}

/// Handle SessionAction::New — archive current session, create a new one.
///
/// Returns the new session (replaces the old one). If agent is running,
/// the current session becomes the background session.
pub fn handle_session_new(
    app: &mut App,
    session: &mut Session,
    rt_env: &RuntimeEnvironment,
    memory: &Arc<ox_core::memory::MemoryManager>,
) -> Result<(), String> {
    let session_dir = rt_env.ox_home_dir.join("sessions").join(&rt_env.project_id);

    // Trigger memory promotion for meaningful sessions (10+ messages)
    if session.messages.len() >= 10 {
        tracing::info!(
            "🚀 Triggering memory promotion for session with {} messages",
            session.messages.len()
        );
        if let Some(result) = memory.run_promotion_pipeline(
            &rt_env.project_id,
            &rt_env
                .working_dir
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "unknown".to_string()),
        ) {
            match result {
                Ok(report) => {
                    app.output
                        .push_system(&format!("\n🧠 Memory Promotion Complete:\n{}", report));
                }
                Err(e) => {
                    tracing::error!("Memory promotion failed: {}", e);
                }
            }
        }
    }

    // Archive current session
    if let Err(e) = session.archive(&session_dir) {
        tracing::warn!("Failed to archive current session: {}", e);
    }

    // Create new session with default working directory
    let default_wd = rt_env.working_dir.to_string_lossy().to_string();
    let mut new_s = Session::new(&session_dir, &rt_env.project_id)
        .map_err(|e| format!("Failed to create session: {}", e))?;
    if let Err(e) = new_s.update_working_dir(&default_wd) {
        tracing::warn!("Failed to set default working dir: {}", e);
    }

    *session = new_s;
    app.output.clear();
    app.output.push_system("New session started.");
    crate::helpers::refresh_header_info(app, rt_env, true);
    app.message_count = 0;
    Ok(())
}

/// Handle SessionAction::Resume — load an archived session by filename or display name.
pub fn handle_session_resume(
    app: &mut App,
    session: &mut Session,
    rt_env: &mut RuntimeEnvironment,
    filename: &str,
    has_provider: bool,
) -> Result<(), String> {
    let sessions_root = rt_env.ox_home_dir.join("sessions");

    // Find session entry by ID or display name
    let target = app
        .sessions
        .iter()
        .find(|s| s.id == filename || s.display_name().contains(filename))
        .ok_or_else(|| format!("Session '{}' not found.", filename))?;
    let (entry_id, entry_project_id) = (target.id.clone(), target.project_id.clone());

    let session_path = std::path::PathBuf::from(&sessions_root)
        .join(&entry_project_id)
        .join(&entry_id);
    let parent_dir = session_path
        .parent()
        .ok_or_else(|| "Invalid session path".to_string())?;

    let archived = Session::load_archived(parent_dir, &entry_id)
        .map_err(|e| format!("Failed to load session: {}", e))?
        .ok_or_else(|| format!("Session '{}' not found.", filename))?;

    *session = archived;

    // Restore working directory
    if let Some(ref wd) = session.meta.working_dir {
        if let Ok(path) = std::path::PathBuf::from(wd).canonicalize() {
            if let Err(e) = std::env::set_current_dir(&path) {
                tracing::warn!("Failed to restore working dir: {}", e);
            } else {
                rt_env.working_dir = path.clone();
                app.working_dir = path.display().to_string();
                app.output.push_system(&format!(
                    "Restored working directory: {}",
                    path.display()
                ));
            }
        }
    }

    crate::helpers::replay_session_history(app, &session.messages, rt_env, has_provider);
    app.output.push_system(&format!(
        "Session restored: {} messages from {}",
        session.messages.len(),
        filename
    ));
    app.dirty = true;
    app.scroll_to_bottom();
    Ok(())
}
