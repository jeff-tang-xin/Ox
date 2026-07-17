//! Session lifecycle handlers.

use crate::terminal::app::App;
use ox_core::message::Session;
use ox_core::runtime::RuntimeEnvironment;

/// Rebuild the session sidebar list from disk.
pub fn rebuild_sidebar(
    app: &mut App,
    sessions_root: &std::path::Path,
    active_project_id: &str,
    active_session_display_name: &str,
) {
    use crate::terminal::app::SessionEntry;

    app.sessions.clear();

    let project_dir = sessions_root.join(active_project_id);
    if !project_dir.exists() {
        return;
    }

    if let Ok(entries) = std::fs::read_dir(&project_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                let id = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                let info = id.clone();
                app.sessions.push(SessionEntry {
                    id,
                    project_id: active_project_id.to_string(),
                    info,
                    is_active: false,
                });
            }
        }
    }

    app.sessions.push(SessionEntry {
        id: "session.jsonl".to_string(),
        project_id: active_project_id.to_string(),
        info: active_session_display_name.to_string(),
        is_active: true,
    });
}

/// Handle SessionAction::New — archive current session, create a new one.
pub fn handle_session_new(
    app: &mut App,
    session: &mut Session,
    rt_env: &RuntimeEnvironment,
) -> Result<(), String> {
    let session_dir = rt_env.ox_home_dir.join("sessions").join(&rt_env.project_id);

    // Knowledge consolidation removed — sessions are now archived to disk directly.

    if let Err(e) = session.archive(&session_dir) {
        tracing::warn!("Failed to archive current session: {e}");
    }

    let default_wd = rt_env.working_dir.to_string_lossy().to_string();
    let mut new_s = Session::new(&session_dir, &rt_env.project_id)
        .map_err(|e| format!("Failed to create session: {e}"))?;
    if let Err(e) = new_s.update_working_dir(&default_wd) {
        tracing::warn!("Failed to set default working dir: {e}");
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

    let target = app
        .sessions
        .iter()
        .find(|s| s.id == filename || s.display_name().contains(filename))
        .ok_or_else(|| format!("Session '{filename}' not found."))?;
    let (entry_id, entry_project_id) = (target.id.clone(), target.project_id.clone());

    let session_path = std::path::PathBuf::from(&sessions_root)
        .join(&entry_project_id)
        .join(&entry_id);
    let parent_dir = session_path
        .parent()
        .ok_or_else(|| "Invalid session path".to_string())?;

    let archived = Session::load_archived(parent_dir, &entry_id)
        .map_err(|e| format!("Failed to load session: {e}"))?
        .ok_or_else(|| format!("Session '{filename}' not found."))?;

    *session = archived;

    if let Some(wd) = session.meta.working_dir.as_ref() {
        if let Ok(canonical) = std::path::Path::new(wd).canonicalize() {
            rt_env.working_dir = canonical;
        }
    }

    app.output.clear();
    app.output
        .push_system(&format!("Resumed session: {filename}"));
    crate::helpers::refresh_header_info(app, rt_env, has_provider);
    app.message_count = session.messages.len();
    Ok(())
}
