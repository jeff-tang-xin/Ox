/// User intervention types - points where user can interact with workflow
#[derive(Debug, Clone)]
pub enum InterventionType {
    /// User confirmation required to proceed
    Confirmation {
        message: String,
        allow_reject: bool,
    },
    /// User input required (text)
    InputRequired {
        prompt: String,
        default_value: Option<String>,
    },
    /// User can modify content before proceeding
    ContentEdit {
        content_type: String,
        current_content: String,
        editable: bool,
    },
    /// User can skip current step
    SkipOption {
        step_name: String,
        warning: Option<String>,
    },
}

/// Intervention request sent to UI
#[derive(Debug, Clone)]
pub struct InterventionRequest {
    /// Unique request ID
    pub id: String,
    /// Type of intervention
    pub intervention_type: InterventionType,
    /// Current workflow step
    pub current_step: String,
    /// Current mode
    pub mode: String,
}

impl InterventionRequest {
    pub fn confirmation(message: &str, current_step: &str, mode: &str) -> Self {
        Self {
            id: format!("confirm_{}", std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()),
            intervention_type: InterventionType::Confirmation {
                message: message.to_string(),
                allow_reject: true,
            },
            current_step: current_step.to_string(),
            mode: mode.to_string(),
        }
    }
    
    pub fn input_required(prompt: &str, current_step: &str, mode: &str) -> Self {
        Self {
            id: format!("input_{}", std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()),
            intervention_type: InterventionType::InputRequired {
                prompt: prompt.to_string(),
                default_value: None,
            },
            current_step: current_step.to_string(),
            mode: mode.to_string(),
        }
    }
}

/// User intervention response
#[derive(Debug, Clone)]
pub enum InterventionResponse {
    /// User confirmed
    Confirmed,
    /// User rejected
    Rejected,
    /// User provided input
    Input(String),
    /// User requested to skip step
    Skip,
    /// User requested to cancel workflow
    Cancel,
}

/// Intervention Manager - handles user interaction points in workflows
pub struct InterventionManager {
    /// Pending intervention requests
    pending_requests: Vec<InterventionRequest>,
    /// Current active intervention
    active_intervention: Option<InterventionRequest>,
}

impl InterventionManager {
    pub fn new() -> Self {
        Self {
            pending_requests: Vec::new(),
            active_intervention: None,
        }
    }
    
    /// Check if there's an active intervention waiting for user response
    pub fn has_active_intervention(&self) -> bool {
        self.active_intervention.is_some()
    }
    
    /// Get the active intervention request
    pub fn get_active_intervention(&self) -> Option<&InterventionRequest> {
        self.active_intervention.as_ref()
    }
    
    /// Queue a new intervention request
    pub fn queue_intervention(&mut self, request: InterventionRequest) {
        self.pending_requests.push(request);
    }
    
    /// Activate the next pending intervention (if any)
    pub fn activate_next(&mut self) -> Option<&InterventionRequest> {
        if !self.pending_requests.is_empty() {
            let request = self.pending_requests.remove(0);
            self.active_intervention = Some(request);
            self.active_intervention.as_ref()
        } else {
            None
        }
    }
    
    /// Process user response to an intervention
    pub fn process_response(&mut self, response: InterventionResponse) -> InterventionAction {
        match response {
            InterventionResponse::Confirmed => {
                self.active_intervention = None;
                InterventionAction::Proceed
            }
            InterventionResponse::Rejected => {
                self.active_intervention = None;
                InterventionAction::Abort
            }
            InterventionResponse::Input(text) => {
                self.active_intervention = None;
                InterventionAction::ContinueWithInput(text)
            }
            InterventionResponse::Skip => {
                self.active_intervention = None;
                InterventionAction::SkipStep
            }
            InterventionResponse::Cancel => {
                self.active_intervention = None;
                InterventionAction::CancelWorkflow
            }
        }
    }
    
    /// Clear all pending interventions (e.g., when switching modes)
    pub fn clear_all(&mut self) {
        self.pending_requests.clear();
        self.active_intervention = None;
    }
}

/// Action to take after processing intervention response
#[derive(Debug, Clone)]
pub enum InterventionAction {
    /// Proceed to next step
    Proceed,
    /// Abort current operation
    Abort,
    /// Continue with user-provided input
    ContinueWithInput(String),
    /// Skip current step
    SkipStep,
    /// Cancel entire workflow
    CancelWorkflow,
}
