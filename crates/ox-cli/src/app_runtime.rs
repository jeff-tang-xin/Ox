//! AppRuntime — consolidates all shared subsystem state.
//!
//! Previously, `handle_key_event` took 19 parameters and `run_app` was 1200+ lines.
//! This struct holds the shared state once, eliminating parameter-sprawl.

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

use ox_core::agent::interjection::InterjectionBuffer;
use ox_core::agent::interrupt::InterruptController;
use ox_core::agent::AgentToUiEvent;
use ox_core::config::{AgentConfig, OxConfig};
use ox_core::context::compressed_store::CompressedContextStore;
use ox_core::context::ContextBuilder;
use ox_core::cost::CostTracker;
use ox_core::knowledge::KnowledgeEngine;
use ox_core::llm::{LlmProvider, ProviderResolveInfo};
use ox_core::memory::MemoryManager;
use ox_core::message::{Message, Session};
use ox_core::runtime::RuntimeEnvironment;
use ox_core::safety::TrustManager;
use ox_core::tools::{ToolContext, ToolRegistry};

use crate::slash_commands::CommandRegistry;
use crate::terminal::event::EventHandler;

/// Consolidated runtime state for the Ox application.
///
/// Holds all subsystem references and mutable loop state, replacing the 19-parameter
/// `handle_key_event` signature with a single `&AppRuntime` reference.
pub struct AppRuntime {
    // ── Config ──
    pub config: OxConfig,
    pub agent_config: Arc<AgentConfig>,

    // ── Environment ──
    pub rt_env: RuntimeEnvironment,

    // ── LLM ──
    pub provider: Option<Arc<dyn LlmProvider>>,
    pub resolve_info: Option<ProviderResolveInfo>,
    pub model_name: String,

    // ── Tools & Commands ──
    pub tool_registry: Arc<ToolRegistry>,
    pub command_registry: CommandRegistry,
    pub tool_ctx: Arc<ToolContext>,

    // ── Context ──
    pub context_builder: ContextBuilder,
    pub context_window: u32,
    pub compressed_ctx_store: Arc<CompressedContextStore>,

    // ── Knowledge & Memory ──
    pub knowledge_engine: Arc<tokio::sync::RwLock<KnowledgeEngine>>,
    pub memory: Arc<MemoryManager>,

    // ── Safety & Trust ──
    pub trust_manager: Arc<std::sync::Mutex<TrustManager>>,

    // ── Cost ──
    pub cost_tracker: CostTracker,

    // ── Channels ──
    pub agent_tx: mpsc::UnboundedSender<AgentToUiEvent>,

    // ── Mutable loop state ──
    pub interrupt_ctrl: InterruptController,
    pub interjection_buf: InterjectionBuffer,
    pub events: EventHandler,
    pub tick_count: u64,
    /// Cached compressed context: (compressed_messages, source_msg_count)
    pub compressed_cache: Option<(Vec<Message>, usize)>,
    /// Session held in background during active-agent session switch
    pub background_session: Option<Session>,
}

impl AppRuntime {
    /// Create a new AppRuntime with all subsystems initialized.
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
        knowledge_engine: Arc<tokio::sync::RwLock<KnowledgeEngine>>,
        memory: Arc<MemoryManager>,
        trust_manager: Arc<std::sync::Mutex<TrustManager>>,
        cost_tracker: CostTracker,
        agent_tx: mpsc::UnboundedSender<AgentToUiEvent>,
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
            knowledge_engine,
            memory,
            trust_manager,
            cost_tracker,
            agent_tx,
            interrupt_ctrl: InterruptController::new(),
            interjection_buf: InterjectionBuffer::new(),
            events,
            tick_count: 0,
            compressed_cache: None,
            background_session: None,
        }
    }

    /// Check if LLM provider is available.
    pub fn has_provider(&self) -> bool {
        self.provider.is_some()
    }

    /// Get a reference to the provider, panicking if unavailable.
    pub fn provider_ref(&self) -> &Arc<dyn LlmProvider> {
        self.provider.as_ref().expect("LLM provider not available")
    }
}
