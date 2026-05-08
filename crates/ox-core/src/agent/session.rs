use crate::agent::workflow::Workflow;

/// Session state - tracks the current state of a conversation session
#[derive(Debug, Clone)]
pub struct SessionState {
    /// Session ID
    pub session_id: String,
    /// Current active mode ID
    pub current_mode: String,
    /// Current workflow ID
    pub current_workflow: String,
    /// Current step index in the workflow
    pub current_step_index: usize,
    /// Whether waiting for user confirmation
    pub awaiting_user_confirmation: bool,
    /// Session-specific variables (for validators, context, etc.)
    pub variables: std::collections::HashMap<String, String>,
    /// Message count (for compression tracking)
    pub message_count: usize,
    /// Whether this session is active
    pub is_active: bool,
}

impl SessionState {
    pub fn new(session_id: &str) -> Self {
        Self {
            session_id: session_id.to_string(),
            current_mode: "free".to_string(),
            current_workflow: "free_workflow".to_string(),
            current_step_index: 0,
            awaiting_user_confirmation: false,
            variables: std::collections::HashMap::new(),
            message_count: 0,
            is_active: true,
        }
    }

    /// Set a session variable
    pub fn set_variable(&mut self, key: &str, value: &str) {
        self.variables.insert(key.to_string(), value.to_string());
    }

    /// Get a session variable
    pub fn get_variable(&self, key: &str) -> Option<&String> {
        self.variables.get(key)
    }

    /// Check if a variable exists and has a truthy value
    pub fn has_variable(&self, key: &str) -> bool {
        self.variables.contains_key(key)
    }

    /// Advance to next workflow step
    pub fn advance_step(&mut self, total_steps: usize) -> bool {
        if self.current_step_index < total_steps - 1 {
            self.current_step_index += 1;
            self.awaiting_user_confirmation = false;
            true
        } else {
            false
        }
    }

    /// Mark that we're waiting for user confirmation
    pub fn wait_for_confirmation(&mut self) {
        self.awaiting_user_confirmation = true;
    }

    /// Clear confirmation flag (after user confirms)
    pub fn clear_confirmation(&mut self) {
        self.awaiting_user_confirmation = false;
    }

    /// Increment message count
    pub fn increment_message_count(&mut self) {
        self.message_count += 1;
    }
}

/// State Registry - manages validation functions and session states
pub struct StateRegistry {
    /// Registered validation functions
    validators: std::collections::HashMap<String, Box<dyn Fn(&SessionState) -> bool>>,
    /// Active sessions
    sessions: std::collections::HashMap<String, SessionState>,
    /// Current active session ID
    current_session_id: Option<String>,
}

impl StateRegistry {
    pub fn new() -> Self {
        let mut registry = Self {
            validators: std::collections::HashMap::new(),
            sessions: std::collections::HashMap::new(),
            current_session_id: None,
        };

        // Register default validators
        registry.register_default_validators();

        registry
    }

    /// Register built-in validators
    fn register_default_validators(&mut self) {
        // Check if task type has been classified
        self.validators.insert(
            "check_task_classified".to_string(),
            Box::new(|state| state.has_variable("task_type")),
        );

        // Check if spec file has been created
        self.validators.insert(
            "check_spec_file_exists".to_string(),
            Box::new(|state| state.has_variable("spec_file_created")),
        );

        // Check if spec has been confirmed by user
        self.validators.insert(
            "check_spec_confirmed".to_string(),
            Box::new(|state| state.has_variable("spec_confirmed")),
        );

        // Check if task file has been created
        self.validators.insert(
            "check_task_file_exists".to_string(),
            Box::new(|state| state.has_variable("task_file_created")),
        );

        // Check if task has been confirmed by user
        self.validators.insert(
            "check_task_confirmed".to_string(),
            Box::new(|state| state.has_variable("task_confirmed")),
        );
    }

    /// Register a custom validator
    pub fn register_validator<F>(&mut self, name: &str, validator: F)
    where
        F: Fn(&SessionState) -> bool + 'static,
    {
        self.validators
            .insert(name.to_string(), Box::new(validator));
    }

    /// Run a validator
    pub fn validate(&self, validator_name: &str, state: &SessionState) -> bool {
        if let Some(validator) = self.validators.get(validator_name) {
            validator(state)
        } else {
            tracing::warn!("Validator '{}' not found", validator_name);
            false
        }
    }

    /// Create a new session
    pub fn create_session(&mut self, session_id: &str) -> &mut SessionState {
        self.sessions
            .entry(session_id.to_string())
            .or_insert_with(|| SessionState::new(session_id))
    }

    /// Get current session
    pub fn current_session(&self) -> Option<&SessionState> {
        self.current_session_id
            .as_ref()
            .and_then(|id| self.sessions.get(id))
    }

    /// Get mutable reference to current session
    pub fn current_session_mut(&mut self) -> Option<&mut SessionState> {
        self.current_session_id
            .as_ref()
            .and_then(|id| self.sessions.get_mut(id))
    }

    /// Set current session
    pub fn set_current_session(&mut self, session_id: &str) {
        self.current_session_id = Some(session_id.to_string());
    }

    /// Switch to a different session
    pub fn switch_session(&mut self, session_id: &str) -> Option<&mut SessionState> {
        if self.sessions.contains_key(session_id) {
            // Deactivate old session
            if let Some(old_session) = self.current_session_mut() {
                old_session.is_active = false;
            }

            // Activate new session
            self.current_session_id = Some(session_id.to_string());
            if let Some(new_session) = self.sessions.get_mut(session_id) {
                new_session.is_active = true;
                Some(new_session)
            } else {
                None
            }
        } else {
            None
        }
    }
}
