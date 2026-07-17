//! AppRuntime — consolidates all shared subsystem state.

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

use ox_core::agent::AgentToUiEvent;
use ox_core::agent::interjection::InterjectionBuffer;
use ox_core::agent::interrupt::InterruptController;
use ox_core::config::{AgentConfig, OxConfig};
use ox_core::context::ContextBuilder;
use ox_core::context::compressed_store::CompressedContextStore;
use ox_core::cost::CostTracker;
use ox_core::llm::{LlmProvider, ProviderResolveInfo};
use ox_core::message::{Message, Session};
use ox_core::runtime::RuntimeEnvironment;
use ox_core::safety::TrustManager;
use ox_core::mcp::GitNexusService;
use ox_core::tools::{ToolContext, ToolRegistry};

use crate::slash_commands::CommandRegistry;
use crate::terminal::event::EventHandler;

/// Consolidated runtime state for the Ox application.
pub struct AppRuntime {
    pub config: OxConfig,
    pub agent_config: Arc<AgentConfig>,
    pub rt_env: RuntimeEnvironment,
    pub provider: Option<Arc<dyn LlmProvider>>,
    pub resolve_info: Option<ProviderResolveInfo>,
    pub model_name: String,
    pub tool_registry: Arc<ToolRegistry>,
    pub command_registry: CommandRegistry,
    pub tool_ctx: Arc<ToolContext>,
    pub context_builder: ContextBuilder,
    pub context_window: u32,
    pub compressed_ctx_store: Arc<CompressedContextStore>,
    pub trust_manager: Arc<std::sync::Mutex<TrustManager>>,
    pub cost_tracker: CostTracker,
    pub agent_tx: mpsc::UnboundedSender<AgentToUiEvent>,
    pub interrupt_ctrl: InterruptController,
    pub interjection_buf: InterjectionBuffer,
    pub events: EventHandler,
    pub tick_count: u64,
    pub compressed_cache: Option<(Vec<Message>, usize)>,
    pub background_session: Option<Session>,
    pub gitnexus: Arc<GitNexusService>,
}

impl AppRuntime {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: OxConfig,
        agent_config: Arc<AgentConfig>,
        rt_env: RuntimeEnvironment,
        provider: Option<Arc<dyn LlmProvider>>,
        resolve_info: Option<ProviderResolveInfo>,
        model_name: String,
        tool_registry: Arc<ToolRegistry>,
        command_registry: CommandRegistry,
        tool_ctx: Arc<ToolContext>,
        context_builder: ContextBuilder,
        context_window: u32,
        compressed_ctx_store: Arc<CompressedContextStore>,
        trust_manager: Arc<std::sync::Mutex<TrustManager>>,
        cost_tracker: CostTracker,
        agent_tx: mpsc::UnboundedSender<AgentToUiEvent>,
        gitnexus: Arc<GitNexusService>,
    ) -> Self {
        let events = EventHandler::new(Duration::from_millis(33));
        Self {
            config,
            agent_config,
            rt_env,
            provider,
            resolve_info,
            model_name,
            tool_registry,
            command_registry,
            tool_ctx,
            context_builder,
            context_window,
            compressed_ctx_store,
            trust_manager,
            cost_tracker,
            agent_tx,
            interrupt_ctrl: InterruptController::new(),
            interjection_buf: InterjectionBuffer::new(),
            events,
            tick_count: 0,
            compressed_cache: None,
            background_session: None,
            gitnexus,
        }
    }

    pub fn has_provider(&self) -> bool {
        self.provider.is_some()
    }

    pub fn provider_ref(&self) -> &Arc<dyn LlmProvider> {
        self.provider.as_ref().expect("LLM provider not available")
    }
}
