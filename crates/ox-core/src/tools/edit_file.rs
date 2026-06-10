/// edit_file — the single unified file editing tool.
///
/// Two modes:
/// 1. Single edit:  path + old_string + new_string (+ optional replace_all)
/// 2. Multi edit:   path + edits: [{old_string, new_string, replace_all?}, …]
///
/// Matching cascade (inspired by Aider research):
///   a. Exact string match (fast path, zero risk)
///   b. Whitespace-normalized match (handles tab/space and indent differences)
///   c. Line-by-line fuzzy match with relative-indent normalization
///
/// When replace_all is true, every occurrence is replaced.
/// In multi mode, edits are applied sequentially — each edit sees the result
/// of the previous one — and the file is only written if ALL edits succeed.

use serde_json::{Value, json};
use std::sync::Arc;

use super::{SafetyLevel, Tool, ToolContext, ToolOutput, content_validation};

pub struct EditFileTool;

#[async_trait::async_trait]
impl Tool for EditFileTool {
    fn name(&self) -> &str {
        "edit_file"
    }

    fn description(&self) -> &str {
        "Replace text in a file. Two forms:\n\
         Single edit: {\"path\", \"old_string\", \"new_string\", \"replace_all?\"}\n\
         Multi edit:  {\"path\", \"edits\": [{\"old_string\", \"new_string\", \"replace_all?\"}, …]}\n\n\
         old_string must match exactly once (or use replace_all: true for every occurrence).\n\
         If exact match fails, fuzzy matching handles whitespace/indent differences.\n\
         Multi edits are applied atomically in order — each sees the result of the previous."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File path (relative to workspace root)."
                },
                "old_string": {
                    "type": "string",
                    "description": "Exact text to replace. Include 2-5 lines of surrounding context for uniqueness."
                },
                "new_string": {
                    "type": "string",
                    "description": "Replacement text. May be empty to delete."
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "Replace ALL occurrences instead of just one. Default false."
                },
                "edits": {
                    "type": "array",
                    "description": "Multiple ordered edits applied atomically. Each is {old_string, new_string, replace_all?}.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "old_string": {"type": "string", "description": "Exact text to find."},
                            "new_string": {"type": "string", "description": "Replacement text."},
                            "replace_all": {"type": "boolean", "description": "Replace all occurrences of this edit."}
                        },
                        "required": ["old_string", "new_string"]
                    }
                }
            },
            "required": ["path"],
            "oneOf": [
                {"required": ["old_string", "new_string"], "description": "Single edit"},
                {"required": ["edits"], "description": "Multi-edit (atomic)"}
            ]
        })
    }

    fn safety_level(&self) -> SafetyLevel {
        SafetyLevel::RequiresConfirmation
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        // ── Resolve path ──
        let path_str = match args.get("path").and_then(|p| p.as_str()) {
            Some(p) => p.trim().replace('\\', "/"),
            None => return ToolOutput::error(
                "❌ Missing required parameter: 'path'.\n\
                 Usage: {\"path\": \"<relative-path>\", \"old_string\": \"<exact text>\", \"new_string\": \"<replacement>\"}",
            ),
        };

        let resolved_path = if std::path::Path::new(&path_str).is_absolute() {
            std::path::PathBuf::from(&path_str)
        } else {
            ctx.working_dir.join(&path_str)
        };

        let path = match crate::safety::validate_path_within_workdir(&resolved_path, &ctx.working_dir) {
            Ok(p) => p,
            Err(e) => return ToolOutput::error(format!("Path validation failed: {e}")),
        };

        // ── Determine mode: single or multi ──
        let is_multi = args.get("edits").is_some();

        if is_multi {
            self.execute_multi(&path, &args, ctx).await
        } else {
            self.execute_single(&path, &args, ctx).await
        }
    }
}

// ── Single-edit mode ────────────────────────────────────────────────

impl EditFileTool {
    async fn execute_single(
        &self,
        path: &std::path::Path,
        args: &Value,
        ctx: &ToolContext,
    ) -> ToolOutput {
        let old_string = match args.get("old_string").and_then(|s| s.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return ToolOutput::error(
                "❌ Missing required parameter: 'old_string'. Must be the EXACT text to find in the file.",
            ),
        };

        let new_string = match args.get("new_string").and_then(|s| s.as_str()) {
            Some(s) => s.to_string(),
            None => return ToolOutput::error(
                "❌ Missing required parameter: 'new_string'. Use empty string \"\" to delete.",
            ),
        };

        let replace_all = args.get("replace_all")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if let Err(e) = content_validation::validate_content(&new_string) {
            return ToolOutput::error(e);
        }

        let edit = SingleEdit { old_string, new_string, replace_all };
        let edits = vec![edit];

        self.apply_edits(path, &edits, ctx).await
    }

    async fn execute_multi(
        &self,
        path: &std::path::Path,
        args: &Value,
        ctx: &ToolContext,
    ) -> ToolOutput {
        let edits_json = match args.get("edits").and_then(|e| e.as_array()) {
            Some(arr) if !arr.is_empty() => arr,
            _ => return ToolOutput::error(
                "❌ 'edits' must be a non-empty array of {{old_string, new_string, replace_all?}} objects.",
            ),
        };

        let mut edits: Vec<SingleEdit> = Vec::with_capacity(edits_json.len());
        for (i, edit_val) in edits_json.iter().enumerate() {
            let old_str = match edit_val.get("old_string").and_then(|s| s.as_str()) {
                Some(s) if !s.is_empty() => s.to_string(),
                _ => return ToolOutput::error(format!(
                    "❌ edits[{i}]: missing or empty 'old_string'."
                )),
            };
            let new_str = match edit_val.get("new_string").and_then(|s| s.as_str()) {
                Some(s) => s.to_string(),
                None => return ToolOutput::error(format!(
                    "❌ edits[{i}]: missing 'new_string'. Use \"\" to delete."
                )),
            };
            let repl_all = edit_val.get("replace_all")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            if let Err(e) = content_validation::validate_content(&new_str) {
                return ToolOutput::error(format!("edits[{i}]: {e}"));
            }
            edits.push(SingleEdit { old_string: old_str, new_string: new_str, replace_all: repl_all });
        }

        self.apply_edits(path, &edits, ctx).await
    }

    /// Core: apply a sequence of SingleEdits atomically via spawn_blocking.
    async fn apply_edits(
        &self,
        path: &std::path::Path,
        edits: &[SingleEdit],
        ctx: &ToolContext,
    ) -> ToolOutput {
        let path_clone = path.to_path_buf();
        let display_path = path.display().to_string();
        let edits_clone: Vec<SingleEdit> = edits.iter().map(|e| SingleEdit {
            old_string: e.old_string.clone(),
            new_string: e.new_string.clone(),
            replace_all: e.replace_all,
        }).collect();

        let result = tokio::task::spawn_blocking(move || {
            // Phase 1: Read file
            let content = match std::fs::read_to_string(&path_clone) {
                Ok(c) => c,
                Err(e) => return Err(format!(
                    "❌ Cannot read {}: {e}\n💡 Check the path with file_list or file_read.",
                    path_clone.display()
                )),
            };

            // Phase 2: Apply each edit sequentially
            let mut current = content;
            let total_edits = edits_clone.len();

            for (i, edit) in edits_clone.iter().enumerate() {
                let label = if total_edits > 1 {
                    format!("[{}/{}] ", i + 1, total_edits)
                } else {
                    String::new()
                };

                match apply_one_edit(&current, edit, &display_path) {
                    Ok(new_content) => {
                        current = new_content;
                    }
                    Err(e) => {
                        return Err(format!("{label}{e}"));
                    }
                }
            }

            // Phase 3: Write file
            match std::fs::write(&path_clone, &current) {
                Ok(()) => {
                    let old_lines: usize = edits_clone.iter()
                        .map(|e| e.old_string.lines().count())
                        .sum();
                    let new_lines: usize = edits_clone.iter()
                        .map(|e| e.new_string.lines().count())
                        .sum();

                    let msg = if edits_clone.len() == 1 && edits_clone[0].replace_all {
                        format!(
                            "✅ Patched {} (replaced all occurrences, {} → {} lines)",
                            path_clone.display(), old_lines, new_lines
                        )
                    } else if edits_clone.len() == 1 {
                        format!(
                            "✅ Patched {} ({} → {} lines)",
                            path_clone.display(), old_lines, new_lines
                        )
                    } else {
                        format!(
                            "✅ Patched {} ({} edits applied, {} → {} lines)",
                            path_clone.display(), edits_clone.len(), old_lines, new_lines
                        )
                    };
                    Ok(msg)
                }
                Err(e) => Err(format!(
                    "❌ Failed to write {}: {e}",
                    path_clone.display()
                )),
            }
        }).await;

        match result {
            Ok(Ok(msg)) => {
                // ── AST syntax check after edit ──
                let ast_warning = {
                    let knowledge = Arc::clone(&ctx.knowledge);
                    let check_path = path.to_path_buf();
                    tokio::spawn(async move {
                        let mut engine = knowledge.lock().await;
                        if let Ok(code) = std::fs::read_to_string(&check_path) {
                            engine.check_syntax(&check_path, &code)
                        } else {
                            None
                        }
                    }).await
                };
                let ast_suffix = match ast_warning {
                    Ok(Some(errors)) => {
                        let mut warn = format!("\n\n⚠️ AST Syntax Check: {} issue(s):", errors.len());
                        for (i, err) in errors.iter().take(5).enumerate() {
                            warn.push_str(&format!("\n   {}. {}", i + 1, err.description));
                        }
                        if errors.len() > 5 {
                            warn.push_str(&format!("\n   ... and {} more", errors.len() - 5));
                        }
                        warn.push_str("\n   💡 Fix syntax errors before proceeding.");
                        warn
                    }
                    _ => String::new(),
                };
                ToolOutput::success(format!("{}{}", msg, ast_suffix))
            }
            Ok(Err(e)) => ToolOutput::error(e),
            Err(join_err) => ToolOutput::error(format!("Edit task panicked: {join_err}")),
        }
    }
}

// ── Data types ──────────────────────────────────────────────────────

struct SingleEdit {
    old_string: String,
    new_string: String,
    replace_all: bool,
}

// ── Core matching logic ─────────────────────────────────────────────

/// Apply one edit to `content`. Returns new content or error message.
fn apply_one_edit(content: &str, edit: &SingleEdit, display_path: &str) -> Result<String, String> {
    let old = &edit.old_string;
    let new = &edit.new_string;

    if old.is_empty() {
        return Err("old_string is empty.".to_string());
    }

    // ── Step 1: Exact match ──
    if content.contains(old) {
        let count = content.matches(old).count();
        if edit.replace_all {
            return Ok(content.replace(old, new));
        }
        if count == 1 {
            return Ok(content.replacen(old, new, 1));
        }
        // Multiple matches but replace_all is false → error
        let locations = find_locations(content, old);
        return Err(format!(
            "❌ old_string matched {} times in {}.\n\n\
             {}\n\n\
             💡 Fix: add more surrounding context to make it unique, or set replace_all: true.",
            count,
            display_path,
            locations.join("\n"),
        ));
    }

    // ── Step 2: Line-based trimmed match (handles whitespace/indent diffs) ──
    // Strategy: split into lines, trim each line, skip blanks, match the sequence.
    // Once found, use the actual original lines as old_string for replacement.
    let old_lines: Vec<&str> = old.lines().collect();
    let file_lines: Vec<&str> = content.lines().collect();

    // Trim signatures — only non-blank lines
    let old_sig: Vec<&str> = old_lines.iter()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();
    let n_sig = old_sig.len();
    if n_sig == 0 {
        return Err("old_string contains only whitespace.".to_string());
    }

    let n_file = file_lines.len();
    let mut best_start = 0usize;
    let mut best_end = 0usize;
    let mut best_score = 0usize;
    let mut dupes: Vec<(usize, usize)> = Vec::new();

    for start in 0..n_file {
        let mut matched = 0usize;
        let mut si = 0usize;
        let mut fi = start;
        while si < n_sig && fi < n_file {
            if file_lines[fi].trim() == old_sig[si] {
                matched += 1;
                si += 1;
                fi += 1;
            } else if file_lines[fi].trim().is_empty() {
                // Skip blank lines in file
                fi += 1;
            } else {
                break;
            }
        }
        if si == n_sig {
            if matched > best_score {
                best_score = matched;
                best_start = start;
                best_end = fi;
                dupes.clear();
                dupes.push((start, fi));
            } else if matched == best_score {
                dupes.push((start, fi));
            }
        }
    }

    let threshold = if n_sig >= 3 { n_sig - 1 } else { n_sig };
    if best_score >= threshold {
        if !edit.replace_all && dupes.len() > 1 {
            let locations: Vec<String> = dupes.iter()
                .enumerate()
                .map(|(idx, (s, _))| {
                    let preview = file_lines[*s.min(&(n_file - 1))].trim().chars().take(80).collect::<String>();
                    format!("  {}. Line {}: {:.80}", idx + 1, s + 1, preview)
                })
                .collect();
            return Err(format!(
                "❌ Search matched {n} locations in {path} (after whitespace normalization).\n\n\
                 Options:\n{locs}\n\n\
                 💡 Fix: Add more unique context, or set replace_all: true.",
                n = dupes.len(), path = display_path, locs = locations.join("\n"),
            ));
        }

        if edit.replace_all {
            // Apply replacements in reverse order so indices stay valid
            let mut current = content.to_string();
            let mut positions: Vec<(usize, usize)> = Vec::new();
            let mut remaining = current.clone();
            loop {
                let rl: Vec<&str> = remaining.lines().collect();
                let mut found = false;
                for start in 0..rl.len() {
                    let mut si = 0usize;
                    let mut fi = start;
                    while si < n_sig && fi < rl.len() {
                        if rl[fi].trim() == old_sig[si] {
                            si += 1;
                            fi += 1;
                        } else if rl[fi].trim().is_empty() {
                            fi += 1;
                        } else {
                            break;
                        }
                    }
                    if si == n_sig {
                        positions.push((start, fi));
                        remaining = rl[fi..].join("\n");
                        found = true;
                        break;
                    }
                }
                if !found { break; }
            }

            for (start, end) in positions.into_iter().rev() {
                let rl: Vec<&str> = current.lines().collect();
                let new_lines: Vec<&str> = new.lines().collect();
                let mut out: Vec<String> = Vec::new();
                for i in 0..start { out.push(rl[i].to_string()); }
                for nl in &new_lines { out.push(nl.to_string()); }
                for i in end..rl.len() { out.push(rl[i].to_string()); }
                current = out.join("\n");
            }
            tracing::info!("[EDIT_FILE] Trimmed replace_all: applied replacements");
            Ok(current)
        } else {
            // Single replacement: replace lines best_start..best_end with new
            let new_lines: Vec<&str> = new.lines().collect();
            let mut out: Vec<String> = Vec::new();
            for i in 0..best_start { out.push(file_lines[i].to_string()); }
            for nl in &new_lines { out.push(nl.to_string()); }
            for i in best_end..n_file { out.push(file_lines[i].to_string()); }
            tracing::info!(
                "[EDIT_FILE] Trimmed match at lines {}-{} (score: {}/{}), applied replacement",
                best_start + 1, best_end, best_score, n_sig
            );
            Ok(out.join("\n"))
        }
    } else {
        // ── Step 3: Relative-indent fuzzy match (last resort) ──
        match fuzzy_relative_indent_match(content, old, new, edit.replace_all) {
            Ok(result) => Ok(result),
            Err(_) => {
                // Build diagnostic
                let search_first = old_sig.first().copied().unwrap_or("");
                let mut similar = Vec::new();
                for (i, line) in file_lines.iter().enumerate() {
                    let t = line.trim();
                    if t.contains(search_first) || search_first.contains(t) {
                        similar.push(format!("  Line {}: {}", i + 1, t.chars().take(80).collect::<String>()));
                        if similar.len() >= 5 { break; }
                    }
                }
                let hint = if similar.is_empty() {
                    "\n🔍 No similar lines found — the content may have changed.\n".to_string()
                } else {
                    format!("\n🔍 Lines containing similar text:\n{}\n", similar.join("\n"))
                };
                Err(format!(
                    "❌ old_string not found in {}.\n\n\
                     🔍 Searched for {} bytes; file is {} lines / {} bytes.\n\
                     {}\
                     💡 Fix: use file_read to get the EXACT current content, then retry.",
                    display_path,
                    old.len(),
                    n_file,
                    content.len(),
                    hint,
                ))
            }
        }
    }
}

// ── Relative-indent fuzzy match (Aider-inspired) ────────────────────

/// Attempt to match using relative-indent normalized comparison.
/// This is the Aider-inspired approach that makes indentation differences
/// irrelevant by encoding only the CHANGE from each line to the next.
fn fuzzy_relative_indent_match(
    content: &str,
    old: &str,
    new: &str,
    replace_all: bool,
) -> Result<String, String> {
    let file_lines: Vec<&str> = content.lines().collect();
    let search_lines: Vec<&str> = old.lines().collect();

    if search_lines.is_empty() {
        return Err("Search string is empty.".to_string());
    }

    let search_rel = to_relative_indent(&search_lines);
    let file_rel = to_relative_indent(&file_lines);

    let n_search = search_rel.len();
    let n_file = file_rel.len();

    let mut best_start = 0usize;
    let mut best_end = 0usize;
    let mut best_score = 0usize;
    let mut duplicate_positions: Vec<(usize, usize)> = Vec::new();

    for start in 0..n_file {
        let mut matched = 0usize;
        let mut si = 0usize;
        let mut fi = start;

        while si < n_search && fi < n_file {
            if search_rel[si] == file_rel[fi] {
                matched += 1;
                si += 1;
                fi += 1;
            } else if search_lines[si].trim().is_empty() {
                si += 1;
            } else if file_lines[fi].trim().is_empty() {
                fi += 1;
            } else {
                break;
            }
        }

        if si == n_search {
            if matched > best_score {
                best_score = matched;
                best_start = start;
                best_end = fi;
                duplicate_positions.clear();
                duplicate_positions.push((start, fi));
            } else if matched == best_score {
                duplicate_positions.push((start, fi));
            }
        }
    }

    let threshold = if n_search >= 3 { n_search - 1 } else { n_search };

    if best_score < threshold {
        return Err("Fuzzy match below threshold".to_string());
    }

    if !replace_all && duplicate_positions.len() > 1 {
        let locations: Vec<String> = duplicate_positions.iter()
            .enumerate()
            .map(|(idx, (s, _))| {
                let preview = file_lines[*s.min(&(file_lines.len() - 1))]
                    .trim()
                    .chars()
                    .take(80)
                    .collect::<String>();
                format!("  {}. Line {}: {:.80}", idx + 1, s + 1, preview)
            })
            .collect();
        return Err(format!(
            "❌ Search matched {n} locations (fuzzy).\n\n\
             Options:\n{locs}\n\n\
             💡 Fix: Add more unique context or set replace_all: true.",
            n = duplicate_positions.len(),
            locs = locations.join("\n"),
        ));
    }

    // Apply replacement(s)
    if replace_all {
        // For replace_all, apply repeatedly
        let mut current = content.to_string();
        // Collect all match positions first (working backwards)
        let mut matches: Vec<(usize, usize)> = Vec::new();
        let mut remaining = current.clone();
        loop {
            let rl: Vec<&str> = remaining.lines().collect();
            let rl_rel = to_relative_indent(&rl);
            let mut found = false;
            for start in 0..rl_rel.len() {
                let mut si = 0usize;
                let mut fi = start;
                while si < n_search && fi < rl_rel.len() {
                    if search_rel[si] == rl_rel[fi] {
                        si += 1;
                        fi += 1;
                    } else if search_lines[si].trim().is_empty() {
                        si += 1;
                    } else if rl[fi].trim().is_empty() {
                        fi += 1;
                    } else {
                        break;
                    }
                }
                if si == n_search {
                    matches.push((start, fi));
                    // Move past this match for next iteration
                    let lines: Vec<&str> = remaining.lines().collect();
                    remaining = lines[fi..].join("\n");
                    found = true;
                    break;
                }
            }
            if !found {
                break;
            }
        }

        // Apply replacements in reverse order
        let match_count = matches.len();
        for (start, end) in matches.into_iter().rev() {
            let lines: Vec<&str> = current.lines().collect();
            let replace_lines: Vec<&str> = new.lines().collect();

            let replace_rel = to_relative_indent(&replace_lines);
            let replace_abs = from_relative_indent(&replace_rel, &lines, start);

            let mut result: Vec<String> = Vec::new();
            for i in 0..start {
                result.push(lines[i].to_string());
            }
            result.extend(replace_abs);
            for i in end..lines.len() {
                result.push(lines[i].to_string());
            }
            current = result.join("\n");
        }

        tracing::info!("[EDIT_FILE] Fuzzy replace_all: {} occurrences", match_count);
        Ok(current)
    } else {
        // Single replacement
        let replace_lines: Vec<&str> = new.lines().collect();
        let replace_rel = to_relative_indent(&replace_lines);
        let replace_abs = from_relative_indent(&replace_rel, &file_lines, best_start);

        let mut result: Vec<String> = Vec::new();
        for i in 0..best_start {
            result.push(file_lines[i].to_string());
        }
        if !replace_lines.is_empty() {
            result.extend(replace_abs);
        }
        for i in best_end..n_file {
            result.push(file_lines[i].to_string());
        }

        tracing::info!(
            "[EDIT_FILE] Fuzzy match at lines {}-{} (score: {}/{}), applied replacement",
            best_start + 1, best_end, best_score, n_search
        );
        Ok(result.join("\n"))
    }
}

// ── Relative-indent helpers ─────────────────────────────────────────

fn to_relative_indent(lines: &[&str]) -> Vec<String> {
    let marker = '←';
    let mut out = Vec::with_capacity(lines.len());
    let mut prev_indent: usize = 0;
    for line in lines {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        let indent_str: String;
        if out.is_empty() {
            indent_str = " ".repeat(indent);
        } else if indent > prev_indent {
            let diff = indent - prev_indent;
            indent_str = " ".repeat(diff);
        } else if indent == prev_indent {
            indent_str = String::new();
        } else {
            let diff = prev_indent - indent;
            indent_str = marker.to_string().repeat(diff);
        }
        out.push(format!("{}{}", indent_str, trimmed));
        prev_indent = indent;
    }
    out
}

fn from_relative_indent(rel: &[String], file_lines: &[&str], start: usize) -> Vec<String> {
    let marker = '←';
    let mut out = Vec::with_capacity(rel.len());
    let mut prev_indent: usize = 0;
    if start < file_lines.len() {
        let first = file_lines[start];
        prev_indent = first.len() - first.trim_start().len();
    }
    for line in rel {
        let trimmed = line.trim_start();
        let prefix = &line[..line.len() - trimmed.len()];
        if prefix.contains(marker) {
            let outdent = prefix.chars().filter(|&c| c == marker).count();
            prev_indent = prev_indent.saturating_sub(outdent);
            out.push(format!("{:indent$}{}", "", trimmed, indent = prev_indent));
        } else if prefix.is_empty() && trimmed.len() < line.len() {
            out.push(format!("{:indent$}{}", "", trimmed, indent = prev_indent));
        } else {
            let add = prefix.len();
            prev_indent += add;
            out.push(format!("{:indent$}{}", "", trimmed, indent = prev_indent));
        }
    }
    out
}

// ── Location helpers ────────────────────────────────────────────────

fn find_locations(content: &str, needle: &str) -> Vec<String> {
    let mut locations = Vec::new();
    let mut pos = 0usize;
    while let Some(found) = content[pos..].find(needle) {
        let abs = pos + found;
        let line_num = content[..abs].lines().count() + 1; // 1-based
        let preview: String = content.lines()
            .nth(line_num - 1)
            .unwrap_or("")
            .chars()
            .take(80)
            .collect();
        locations.push(format!("  Line {}: …{}…", line_num, preview));
        pos = abs + 1;
        if locations.len() >= 10 { break; }
    }
    locations
}
