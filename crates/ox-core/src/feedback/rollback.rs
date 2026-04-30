use crate::persona::PersonaVector;

/// Manages persona snapshots for rollback capability
pub struct RollbackManager {
    /// Maximum number of snapshots to keep per language
    max_snapshots_per_lang: usize,
}

impl RollbackManager {
    pub fn new(max_snapshots_per_lang: usize) -> Self {
        Self {
            max_snapshots_per_lang,
        }
    }

    /// Create a snapshot of current persona state
    pub fn create_snapshot(
        &self,
        persona: &PersonaVector,
        satisfaction_score: f64,
        store: &crate::memory::store::MemoryStore,
    ) -> anyhow::Result<String> {
        let snapshot_id = uuid::Uuid::new_v4().to_string();
        
        store.save_persona_snapshot(
            &snapshot_id,
            &persona.language,
            persona.favors_safety_over_speed,
            persona.prefers_conciseness,
            persona.code_style_strictness,
            persona.refuses_unsafe_code,
            persona.frozen,
            &persona.forbidden_phrases,
            &persona.moral_priorities,
            satisfaction_score,
        )?;

        // Clean up old snapshots if we exceed the limit
        self.cleanup_old_snapshots(&persona.language, store)?;

        Ok(snapshot_id)
    }

    /// Restore persona from a snapshot
    pub fn restore_from_snapshot(
        &self,
        snapshot_id: &str,
        store: &crate::memory::store::MemoryStore,
    ) -> anyhow::Result<Option<PersonaVector>> {
        if let Some((language, safety, conciseness, style, refuses_unsafe, frozen, forbidden, priorities, _score)) = 
            store.load_persona_snapshot(snapshot_id)? {
            
            let persona = PersonaVector {
                favors_safety_over_speed: safety,
                prefers_conciseness: conciseness,
                code_style_strictness: style,
                forbidden_phrases: forbidden,
                moral_priorities: priorities,
                language,
                frozen,
                refuses_unsafe_code: refuses_unsafe,
            };

            // Activate this snapshot
            store.activate_snapshot(snapshot_id)?;

            Ok(Some(persona))
        } else {
            Ok(None)
        }
    }

    /// Evaluate satisfaction and decide if rollback is needed
    pub fn evaluate_and_maybe_rollback(
        &mut self,
        persona: &mut PersonaVector,
        current_satisfaction: f64,
        baseline_satisfaction: f64,
        store: &crate::memory::store::MemoryStore,
    ) -> anyhow::Result<RollbackDecision> {
        // If satisfaction dropped significantly below baseline, consider rollback
        let degradation = baseline_satisfaction - current_satisfaction;
        
        if degradation > 0.2 {
            // Significant degradation - check for active snapshot to rollback to
            if let Some(active_snapshot_id) = store.get_active_snapshot_id(&persona.language)? {
                if let Some(restored_persona) = self.restore_from_snapshot(&active_snapshot_id, store)? {
                    // Apply restored persona
                    *persona = restored_persona;
                    
                    return Ok(RollbackDecision::RolledBack {
                        from_score: current_satisfaction,
                        to_score: baseline_satisfaction,
                        snapshot_id: active_snapshot_id,
                    });
                }
            }
            
            Ok(RollbackDecision::NeedsRollback {
                current_score: current_satisfaction,
                baseline_score: baseline_satisfaction,
                degradation,
            })
        } else {
            // No rollback needed - save current state as snapshot for future
            let _snapshot_id = self.create_snapshot(persona, current_satisfaction, store)?;
            
            Ok(RollbackDecision::NoRollback {
                current_score: current_satisfaction,
            })
        }
    }

    /// Calculate composite satisfaction score from multiple signals
    pub fn calculate_satisfaction_score(
        &self,
        explicit_feedback_rate: f64,  // good / total feedback
        tool_success_rate: f64,       // successful tool calls / total
        code_accept_rate: f64,        // accepted writes / total writes
        has_explicit_feedback: bool,
    ) -> f64 {
        if has_explicit_feedback {
            // With explicit feedback: weight it higher
            explicit_feedback_rate * 0.4 + tool_success_rate * 0.3 + code_accept_rate * 0.3
        } else {
            // Without explicit feedback: rely on implicit signals
            explicit_feedback_rate * 0.1 + tool_success_rate * 0.3 + code_accept_rate * 0.6
        }
    }

    /// Clean up old snapshots, keeping only the most recent ones
    fn cleanup_old_snapshots(
        &self,
        language: &str,
        _store: &crate::memory::store::MemoryStore,
    ) -> anyhow::Result<()> {
        // This would ideally query all snapshots for this language and delete old ones
        // For now, we'll just log that cleanup should happen
        tracing::debug!("Snapshot cleanup for language: {} (max: {})", language, self.max_snapshots_per_lang);
        Ok(())
    }
}

/// Decision made by the rollback evaluation
#[derive(Debug)]
pub enum RollbackDecision {
    /// No rollback needed, state saved
    NoRollback {
        current_score: f64,
    },
    /// Rollback was performed
    RolledBack {
        from_score: f64,
        to_score: f64,
        snapshot_id: String,
    },
    /// Rollback is needed but no snapshot available
    NeedsRollback {
        current_score: f64,
        baseline_score: f64,
        degradation: f64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn temp_store() -> crate::memory::store::MemoryStore {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.db");
        crate::memory::store::MemoryStore::open(&path).unwrap()
    }

    #[test]
    fn test_create_and_restore_snapshot() {
        let store = temp_store();
        let manager = RollbackManager::new(5);
        let mut persona = PersonaVector::for_language("rust");
        
        // Create snapshot
        let snapshot_id = manager.create_snapshot(&persona, 0.8, &store).unwrap();
        
        // Modify persona
        persona.prefers_conciseness = 0.5;
        
        // Restore
        let restored = manager.restore_from_snapshot(&snapshot_id, &store).unwrap().unwrap();
        
        assert!((restored.prefers_conciseness - 0.8).abs() < 0.01);
    }

    #[test]
    fn test_calculate_satisfaction_with_explicit() {
        let manager = RollbackManager::new(5);
        let score = manager.calculate_satisfaction_score(0.9, 0.8, 0.7, true);
        
        // Should weight explicit feedback heavily
        assert!(score > 0.7);
    }

    #[test]
    fn test_calculate_satisfaction_without_explicit() {
        let manager = RollbackManager::new(5);
        let score = manager.calculate_satisfaction_score(0.5, 0.8, 0.7, false);
        
        // Should weight code accept rate heavily
        assert!(score > 0.6);
    }

    #[test]
    fn test_rollback_decision_no_degradation() {
        let store = temp_store();
        let mut manager = RollbackManager::new(5);
        let persona = PersonaVector::for_language("rust");
        
        let decision = manager.evaluate_and_maybe_rollback(
            &mut persona.clone(),
            0.85,  // current
            0.80,  // baseline
            &store,
        ).unwrap();
        
        matches!(decision, RollbackDecision::NoRollback { .. });
    }
}
