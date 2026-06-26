/// Unified Entity Model — the single knowledge atom across all four memory layers,
/// AST code symbols, and session context.
///
/// # Four-Layer Memory Model
/// - **L0 Working Memory**: Session-temporary context + self-state. Lifetime: single session.
/// - **L1 Atomic Memory**: Indivisible single facts, user preferences, tool execution results. Lifetime: long-term.
/// - **L2 Episodic Memory**: Timeline-organized event sequences, task checkpoints, conversation summaries. Lifetime: long-term.
/// - **L3 Semantic Memory**: Abstracted patterns, architectural principles, self-reflections. Lifetime: permanent.
///
/// # Memory Coordinates (每条记忆的多维坐标)
/// Each entity carries a memory coordinate for retrieval precision:
/// - **t** (timestamp): created_at / last_accessed
/// - **d** (depth): 0=L0, 1=L1, 2=L2, 3=L3
/// - **cid** (session anchor): session_id for WorkingMemory, project_id for long-term layers
/// - **e** (semantic vector): 384-dim embedding stored in TriviumDB
/// - **tags**: free-form label set for category-based filtering
///
/// # Extraction Triggers (per design doc)
/// - After tool calls: extract execution result + intent immediately
/// - On key info: user expresses preference, corrects AI, or provides project background
/// - On session end: global extraction triggered by user exit or long idle
///
/// # Filtering Rules
/// - Auto-filter: greetings, repeated confirmations, exploratory chatter with no signal
use serde::{Deserialize, Serialize};

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// EntityKind — four memory layers + code entities
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// Top-level entity category. Maps to the four-layer memory model plus code-level entities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EntityKind {
    // ── L0: Working Memory (工作记忆) ──
    /// Session-temporary context, self-state block, current action log.
    /// Lifetime: single session. Not persisted long-term.
    WorkingMemory,

    // ── L1: Atomic Memory (原子记忆) ──
    /// Indivisible single facts, user preferences, tool execution results.
    /// Example: "User prefers tabs over spaces", "auth.rs:validate_token returns Result<Token>"
    AtomicMemory,

    // ── L2: Episodic Memory (情景记忆) ──
    /// Timeline-organized event sequences, task checkpoints, conversation summaries.
    /// Example: "Session 2025-06-08: Fixed token refresh bug in auth.rs (3 turns)"
    EpisodicMemory,

    // ── L3: Semantic Memory (语义记忆) ──
    /// Abstracted patterns, architectural principles, AI self-reflections.
    /// Example: "This project uses hexagonal architecture with ports/adapters"
    SemanticMemory,

    // ── Code-level entities ──
    /// A code symbol: function, struct, class, trait, enum, etc.
    CodeSymbol,
    /// A source file.
    CodeFile,
    /// A module / package / namespace.
    CodeModule,
}

impl EntityKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::WorkingMemory => "WorkingMemory",
            Self::AtomicMemory => "AtomicMemory",
            Self::EpisodicMemory => "EpisodicMemory",
            Self::SemanticMemory => "SemanticMemory",
            Self::CodeSymbol => "CodeSymbol",
            Self::CodeFile => "CodeFile",
            Self::CodeModule => "CodeModule",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "WorkingMemory" => Some(Self::WorkingMemory),
            "AtomicMemory" => Some(Self::AtomicMemory),
            "EpisodicMemory" => Some(Self::EpisodicMemory),
            "SemanticMemory" => Some(Self::SemanticMemory),
            "CodeSymbol" => Some(Self::CodeSymbol),
            "CodeFile" => Some(Self::CodeFile),
            "CodeModule" => Some(Self::CodeModule),
            _ => None,
        }
    }

    /// The memory depth layer (0-3) for memory layers.
    /// Returns None for code-level entities.
    pub fn depth(&self) -> Option<u8> {
        match self {
            Self::WorkingMemory => Some(0),
            Self::AtomicMemory => Some(1),
            Self::EpisodicMemory => Some(2),
            Self::SemanticMemory => Some(3),
            _ => None,
        }
    }

    pub fn is_memory_layer(&self) -> bool {
        matches!(
            self,
            Self::WorkingMemory | Self::AtomicMemory | Self::EpisodicMemory | Self::SemanticMemory
        )
    }

    pub fn is_code_entity(&self) -> bool {
        matches!(self, Self::CodeSymbol | Self::CodeFile | Self::CodeModule)
    }

    /// Whether this layer should be persisted long-term.
    pub fn is_long_term(&self) -> bool {
        matches!(self, Self::EpisodicMemory | Self::SemanticMemory)
    }

    /// Whether this layer can be safely cleaned up by the janitor.
    pub fn is_cleanup_candidate(&self) -> bool {
        matches!(self, Self::WorkingMemory | Self::AtomicMemory)
    }
}

impl std::fmt::Display for EntityKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// SymbolType — AST symbol categories (migrated from symbol/types.rs)
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SymbolType {
    Function,
    Method,
    Struct,
    Class,
    Enum,
    Trait,
    Interface,
    Impl,
    TypeAlias,
    Constant,
    Static,
    Module,
    Namespace,
    Package,
    Macro,
    Variant,
}

impl std::fmt::Display for SymbolType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            SymbolType::Function => "function",
            SymbolType::Method => "method",
            SymbolType::Struct => "struct",
            SymbolType::Class => "class",
            SymbolType::Enum => "enum",
            SymbolType::Trait => "trait",
            SymbolType::Interface => "interface",
            SymbolType::Impl => "impl",
            SymbolType::TypeAlias => "type",
            SymbolType::Constant => "const",
            SymbolType::Static => "static",
            SymbolType::Module => "module",
            SymbolType::Namespace => "namespace",
            SymbolType::Package => "package",
            SymbolType::Macro => "macro",
            SymbolType::Variant => "variant",
        };
        write!(f, "{}", s)
    }
}

impl SymbolType {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "function" => Some(SymbolType::Function),
            "method" => Some(SymbolType::Method),
            "struct" => Some(SymbolType::Struct),
            "class" => Some(SymbolType::Class),
            "enum" => Some(SymbolType::Enum),
            "trait" => Some(SymbolType::Trait),
            "interface" => Some(SymbolType::Interface),
            "impl" => Some(SymbolType::Impl),
            "type" => Some(SymbolType::TypeAlias),
            "const" => Some(SymbolType::Constant),
            "static" => Some(SymbolType::Static),
            "module" => Some(SymbolType::Module),
            "namespace" => Some(SymbolType::Namespace),
            "package" => Some(SymbolType::Package),
            "macro" => Some(SymbolType::Macro),
            "variant" => Some(SymbolType::Variant),
            _ => None,
        }
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// EntityMetadata — type-specific payload (tagged enum, not giant struct)
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum EntityMetadata {
    /// L0: Working Memory — per-turn self-state and action log
    WorkingMemory {
        session_id: String,
        /// What was done this turn (action)
        action: String,
        /// Why it was done (intent)
        intent: Option<String>,
        /// What resulted (result summary)
        result: Option<String>,
        /// Tools used in this turn
        tools_used: Vec<String>,
        /// Whether code was modified
        has_code_changes: bool,
        /// Entity IDs of modified code symbols/files
        modified_entities: Vec<String>,
        /// AI self-state block (current role, progress, pending)
        self_state: Option<String>,
    },

    /// L1: Atomic Memory — indivisible fact or preference
    AtomicMemory {
        /// Category: Fact, Style, Architectural, AntiPattern, Business, BestPractice, Pattern, MetaSkill
        memory_type: String,
        project_id: Option<String>,
        language: String,
        /// Source: ToolObservation, LlmExtraction, UserExplicit, ConversationSummary
        source: String,
        /// Related file paths for context-aware retrieval
        related_files: Vec<String>,
        /// LLM judge feedback
        quality_score: f32,
        judge_eval_count: u32,
    },

    /// L2: Episodic Memory — timeline event or task checkpoint
    EpisodicMemory {
        /// Episode name / checkpoint label
        episode_name: String,
        project_id: Option<String>,
        /// Session this episode belongs to
        session_id: String,
        /// When the episode started (epoch seconds)
        start_time: i64,
        /// When the episode ended (epoch seconds)
        end_time: Option<i64>,
        /// Core task description
        task_description: String,
        /// Agreed-on conclusions
        conclusions: Vec<String>,
        /// Unresolved issues to carry forward
        unresolved: Vec<String>,
        /// Recommended re-entry point for continuation
        continuation_hint: Option<String>,
        /// Usage / revisit count
        usage_count: u32,
        /// IDs of related AtomicMemory entities
        related_atoms: Vec<String>,
    },

    /// L3: Semantic Memory — abstracted pattern or principle
    SemanticMemory {
        project_id: String,
        /// Version of this abstraction (incremented on update)
        version: u32,
        /// Domain: architecture, coding_style, debugging, testing, deployment, etc.
        domain: String,
        /// IDs of EpisodicMemory entities this was abstracted from
        source_episodes: Vec<String>,
        /// Confidence level (0.0-1.0)
        confidence: f32,
    },

    /// Code Symbol: function, struct, class, etc.
    CodeSymbol {
        symbol_type: SymbolType,
        language: String,
        start_line: usize,
        end_line: usize,
        file_path: String,
        signature: String,
        parent: Option<String>,
        /// Fully qualified name
        fq_name: String,
        /// Function names called by this function (FQ names)
        calls: Vec<String>,
    },

    /// Code File: a source file
    CodeFile {
        path: String,
        language: String,
        symbol_count: usize,
    },

    /// Code Module: a module / package / namespace
    CodeModule { name: String, path: String },
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Relation — graph edges between entities
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Relation {
    pub target_id: String,
    pub relation_type: RelationType,
    /// Strength 0.0-1.0
    pub weight: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RelationType {
    // ── Code-code relations ──
    /// fn A calls fn B
    Calls,
    /// struct implements trait
    Implements,
    /// Symbol is defined in File
    DefinesIn,
    /// File imports from Module
    ImportsFrom,
    /// File depends on another file
    DependsOn,

    // ── Code-memory relations (key cross-type links) ──
    /// WorkingMemory turn mentions a CodeSymbol
    MentionsSymbol,
    /// WorkingMemory turn modifies a CodeSymbol
    ModifiesSymbol,
    /// AtomicMemory fact relates to a CodeSymbol
    RelatesToSymbol,

    // ── Memory-memory relations ──
    /// Semantic similarity (from TriviumDB cosine score)
    SimilarTo,
    /// L1 AtomicMemory belongs to L2 EpisodicMemory
    BelongsTo,
    /// L3 SemanticMemory abstracts L2 EpisodicMemory
    Abstracts,
    /// Temporal precedence
    Precedes,

    // ── File relations ──
    /// File belongs to Module
    BelongsToModule,
}

impl RelationType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Calls => "calls",
            Self::Implements => "implements",
            Self::DefinesIn => "defines_in",
            Self::ImportsFrom => "imports_from",
            Self::DependsOn => "depends_on",
            Self::MentionsSymbol => "mentions_symbol",
            Self::ModifiesSymbol => "modifies_symbol",
            Self::RelatesToSymbol => "relates_to_symbol",
            Self::SimilarTo => "similar_to",
            Self::BelongsTo => "belongs_to",
            Self::Abstracts => "abstracts",
            Self::Precedes => "precedes",
            Self::BelongsToModule => "belongs_to_module",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "calls" => Some(Self::Calls),
            "implements" => Some(Self::Implements),
            "defines_in" => Some(Self::DefinesIn),
            "imports_from" => Some(Self::ImportsFrom),
            "depends_on" => Some(Self::DependsOn),
            "mentions_symbol" => Some(Self::MentionsSymbol),
            "modifies_symbol" => Some(Self::ModifiesSymbol),
            "relates_to_symbol" => Some(Self::RelatesToSymbol),
            "similar_to" => Some(Self::SimilarTo),
            "belongs_to" => Some(Self::BelongsTo),
            "abstracts" => Some(Self::Abstracts),
            "precedes" => Some(Self::Precedes),
            "belongs_to_module" => Some(Self::BelongsToModule),
            _ => None,
        }
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// MemoryCoordinate — 记忆坐标 (per design doc §5.1)
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// Multi-dimensional coordinate for precise memory retrieval.
///
/// Fields:
/// - **t** (temporal): created_at, last_accessed
/// - **d** (depth): 0=L0, 1=L1, 2=L2, 3=L3
/// - **cid** (anchor): session_id for L0, project_id for L1-L3
/// - **e** (embedding): 384-dim semantic vector in TriviumDB
/// - **tags**: free-form category labels
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryCoordinate {
    /// Unix timestamp of creation
    pub created_at: i64,
    /// Unix timestamp of last access
    pub last_accessed: i64,
    /// Memory depth: 0=L0, 1=L1, 2=L2, 3=L3
    pub depth: u8,
    /// Session anchor (for L0) or project anchor (for L1-L3)
    pub anchor: String,
    /// Semantic vector dimension (always 384 with current model)
    pub embedding_dim: u16,
    /// Free-form tags for category filtering
    #[serde(default)]
    pub tags: Vec<String>,
}

impl MemoryCoordinate {
    pub fn new(depth: u8, anchor: &str, embedding_dim: u16) -> Self {
        let now = chrono::Utc::now().timestamp();
        Self {
            created_at: now,
            last_accessed: now,
            depth,
            anchor: anchor.to_string(),
            embedding_dim,
            tags: Vec::new(),
        }
    }

    pub fn with_tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self
    }

    pub fn touch(&mut self) {
        self.last_accessed = chrono::Utc::now().timestamp();
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Entity — the universal knowledge atom
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// The unified knowledge entity. Every piece of knowledge — whether a code symbol,
/// a memory fact, an episode, or a semantic abstraction — is stored as an Entity.
///
/// All entities live in a single TriviumDB instance (`knowledge.tdb`) and are
/// connected via a graph of Relations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    /// Unique identifier (UUID v4)
    pub id: String,
    /// Top-level category
    pub kind: EntityKind,
    /// Human-readable content (what gets embedded and searched)
    pub content: String,
    /// Memory coordinate for retrieval precision
    pub coordinate: MemoryCoordinate,
    /// Type-specific payload
    pub metadata: EntityMetadata,
    /// Outgoing graph edges
    #[serde(default)]
    pub relations: Vec<Relation>,
    /// Whether this entity is critical (exempt from cleanup)
    #[serde(default)]
    pub is_critical: bool,
}

impl Entity {
    // ── Constructors by layer ──

    /// Create an L0 WorkingMemory entity from a conversation turn.
    pub fn working_memory(
        session_id: &str,
        action: &str,
        intent: Option<&str>,
        result: Option<&str>,
        tools_used: Vec<String>,
        has_code_changes: bool,
    ) -> Self {
        let id = uuid::Uuid::new_v4().to_string();
        let content = format!(
            "[L0 WorkingMemory] action={action} intent={intent} result={result}",
            action = action,
            intent = intent.unwrap_or(""),
            result = result.unwrap_or("")
        );
        Self {
            id: id.clone(),
            kind: EntityKind::WorkingMemory,
            content,
            coordinate: MemoryCoordinate::new(0, session_id, 384),
            metadata: EntityMetadata::WorkingMemory {
                session_id: session_id.to_string(),
                action: action.to_string(),
                intent: intent.map(|s| s.to_string()),
                result: result.map(|s| s.to_string()),
                tools_used,
                has_code_changes,
                modified_entities: Vec::new(),
                self_state: None,
            },
            relations: Vec::new(),
            is_critical: false,
        }
    }

    /// Create an L1 AtomicMemory entity.
    pub fn atomic_memory(
        content: &str,
        memory_type: &str,
        project_id: Option<&str>,
        language: &str,
        source: &str,
    ) -> Self {
        let id = uuid::Uuid::new_v4().to_string();
        let anchor = project_id.map(|s| s.to_string()).unwrap_or_default();
        Self {
            id,
            kind: EntityKind::AtomicMemory,
            content: content.to_string(),
            coordinate: MemoryCoordinate::new(1, &anchor, 384),
            metadata: EntityMetadata::AtomicMemory {
                memory_type: memory_type.to_string(),
                project_id: project_id.map(|s| s.to_string()),
                language: language.to_string(),
                source: source.to_string(),
                related_files: Vec::new(),
                quality_score: 0.0,
                judge_eval_count: 0,
            },
            relations: Vec::new(),
            is_critical: false,
        }
    }

    /// Create an L0 → L1 auto-extraction: from a WorkingMemory turn, produce an AtomicMemory.
    /// The content is what the LLM judge confirmed as a worthwhile fact.
    pub fn atomic_from_working(
        working: &Entity,
        fact_content: &str,
        memory_type: &str,
    ) -> Option<Self> {
        if working.kind != EntityKind::WorkingMemory {
            return None;
        }
        let wm = match &working.metadata {
            EntityMetadata::WorkingMemory { session_id, .. } => session_id.clone(),
            _ => return None,
        };
        let mut entity = Self::atomic_memory(fact_content, memory_type, None, "", "LlmExtraction");
        // Link to the source WorkingMemory turn
        entity.relations.push(Relation {
            target_id: working.id.clone(),
            relation_type: RelationType::Precedes,
            weight: 0.9,
        });
        entity.coordinate.anchor = wm;
        Some(entity)
    }

    /// Create an L2 EpisodicMemory entity from a session checkpoint or wrap-up.
    pub fn episodic_memory(
        episode_name: &str,
        session_id: &str,
        project_id: Option<&str>,
        task_description: &str,
    ) -> Self {
        let id = uuid::Uuid::new_v4().to_string();
        let anchor = project_id
            .map(|s| s.to_string())
            .unwrap_or_else(|| session_id.to_string());
        let now = chrono::Utc::now().timestamp();
        Self {
            id,
            kind: EntityKind::EpisodicMemory,
            content: task_description.to_string(),
            coordinate: MemoryCoordinate::new(2, &anchor, 384),
            metadata: EntityMetadata::EpisodicMemory {
                episode_name: episode_name.to_string(),
                project_id: project_id.map(|s| s.to_string()),
                session_id: session_id.to_string(),
                start_time: now,
                end_time: None,
                task_description: task_description.to_string(),
                conclusions: Vec::new(),
                unresolved: Vec::new(),
                continuation_hint: None,
                usage_count: 0,
                related_atoms: Vec::new(),
            },
            relations: Vec::new(),
            is_critical: false,
        }
    }

    /// Create an L3 SemanticMemory entity from abstraction.
    pub fn semantic_memory(
        project_id: &str,
        content: &str,
        domain: &str,
        source_episodes: Vec<String>,
    ) -> Self {
        let id = uuid::Uuid::new_v4().to_string();
        Self {
            id,
            kind: EntityKind::SemanticMemory,
            content: content.to_string(),
            coordinate: MemoryCoordinate::new(3, project_id, 384)
                .with_tags(vec![domain.to_string()]),
            metadata: EntityMetadata::SemanticMemory {
                project_id: project_id.to_string(),
                version: 1,
                domain: domain.to_string(),
                source_episodes,
                confidence: 0.5,
            },
            relations: Vec::new(),
            is_critical: true, // L3 is permanent, exempt from cleanup
        }
    }

    /// Create a CodeSymbol entity from an extracted AST symbol.
    pub fn code_symbol(
        _name: &str,
        fq_name: &str,
        symbol_type: SymbolType,
        language: &str,
        file_path: &str,
        start_line: usize,
        end_line: usize,
        signature: &str,
        parent: Option<&str>,
    ) -> Self {
        Self::code_symbol_with_doc(
            _name,
            fq_name,
            symbol_type,
            language,
            file_path,
            start_line,
            end_line,
            signature,
            parent,
            None,
        )
    }

    /// Code symbol with optional doc comment for richer embedding text.
    pub fn code_symbol_with_doc(
        _name: &str,
        fq_name: &str,
        symbol_type: SymbolType,
        language: &str,
        file_path: &str,
        start_line: usize,
        end_line: usize,
        signature: &str,
        parent: Option<&str>,
        doc_comment: Option<&str>,
    ) -> Self {
        let id = uuid::Uuid::new_v4().to_string();
        let sig_one_line: String = signature
            .lines()
            .next()
            .unwrap_or(signature)
            .chars()
            .take(512)
            .collect();
        let mut content = format!(
            "[{symbol_type}] {fq_name} @ {file_path}:{start_line}-{end_line} :: {sig_one_line}"
        );
        if let Some(doc) = doc_comment {
            let doc_trim: String = doc
                .lines()
                .map(|l| l.trim())
                .filter(|l| !l.is_empty())
                .take(3)
                .collect::<Vec<_>>()
                .join(" ");
            if !doc_trim.is_empty() {
                let doc_short: String = doc_trim.chars().take(400).collect();
                content.push_str(&format!(" | {doc_short}"));
            }
        }
        Self {
            id,
            kind: EntityKind::CodeSymbol,
            content,
            coordinate: MemoryCoordinate {
                created_at: chrono::Utc::now().timestamp(),
                last_accessed: chrono::Utc::now().timestamp(),
                depth: 0, // Code entities don't have memory layers; depth unused
                anchor: file_path.to_string(),
                embedding_dim: 384,
                tags: vec![language.to_string(), symbol_type.to_string()],
            },
            metadata: EntityMetadata::CodeSymbol {
                symbol_type,
                language: language.to_string(),
                start_line,
                end_line,
                file_path: file_path.to_string(),
                signature: signature.to_string(),
                parent: parent.map(|s| s.to_string()),
                fq_name: fq_name.to_string(),
                calls: Vec::new(),
            },
            relations: Vec::new(),
            is_critical: false,
        }
    }

    /// Create a CodeFile entity.
    pub fn code_file(path: &str, language: &str) -> Self {
        let id = uuid::Uuid::new_v4().to_string();
        Self {
            id: id.clone(),
            kind: EntityKind::CodeFile,
            content: format!("Source file: {path} ({language})"),
            coordinate: MemoryCoordinate {
                created_at: chrono::Utc::now().timestamp(),
                last_accessed: chrono::Utc::now().timestamp(),
                depth: 0,
                anchor: path.to_string(),
                embedding_dim: 384,
                tags: vec![language.to_string()],
            },
            metadata: EntityMetadata::CodeFile {
                path: path.to_string(),
                language: language.to_string(),
                symbol_count: 0,
            },
            relations: Vec::new(),
            is_critical: false,
        }
    }

    // ── Helpers ──

    /// Get the file_path if this entity is code-related.
    pub fn file_path(&self) -> Option<&str> {
        match &self.metadata {
            EntityMetadata::CodeSymbol { file_path, .. } => Some(file_path),
            EntityMetadata::CodeFile { path, .. } => Some(path),
            EntityMetadata::CodeModule { path, .. } => Some(path),
            _ => None,
        }
    }

    /// Get the project_id if this entity is memory-related.
    pub fn project_id(&self) -> Option<&str> {
        match &self.metadata {
            EntityMetadata::AtomicMemory { project_id, .. } => project_id.as_deref(),
            EntityMetadata::EpisodicMemory { project_id, .. } => project_id.as_deref(),
            EntityMetadata::SemanticMemory { project_id, .. } => Some(project_id),
            _ => None,
        }
    }

    /// Get the session_id if this entity has one.
    pub fn session_id(&self) -> Option<&str> {
        match &self.metadata {
            EntityMetadata::WorkingMemory { session_id, .. } => Some(session_id),
            EntityMetadata::EpisodicMemory { session_id, .. } => Some(session_id),
            _ => None,
        }
    }

    /// Touch: update last_accessed timestamp.
    pub fn touch(&mut self) {
        self.coordinate.touch();
    }

    /// Check whether this entity passes the noise filter (not a greeting, not empty).
    pub fn has_signal(&self) -> bool {
        if self.content.trim().is_empty() {
            return false;
        }
        let lower = self.content.to_lowercase();
        // Filter greetings and pure pleasantries
        let noise_patterns = [
            "hello",
            "hi there",
            "thanks",
            "thank you",
            "you're welcome",
            "ok",
            "okay",
            "sure",
            "got it",
            "understood",
        ];
        let trimmed = lower.trim();
        // Only filter if the WHOLE content is just a noise phrase
        if trimmed.len() < 20 {
            for pattern in &noise_patterns {
                if trimmed == *pattern {
                    return false;
                }
            }
        }
        true
    }

    /// Text sent to the embedding model — structured per kind, not a blind content chop.
    pub fn text_for_embedding(&self, max_chars: usize) -> String {
        let max_chars = max_chars.max(256);
        match &self.metadata {
            EntityMetadata::CodeSymbol {
                fq_name,
                symbol_type,
                file_path,
                start_line,
                end_line,
                signature,
                ..
            } => {
                let sig = compact_embed_signature(signature, 768);
                let text = format!(
                    "[{symbol_type}] {fq_name} @ {file_path}:{start_line}-{end_line} :: {sig}"
                );
                truncate_at_char_boundary(&text, max_chars)
            }
            _ => truncate_at_char_boundary(&self.content, max_chars),
        }
    }
}

/// Collapse whitespace and cap signature length for embedding (generics/annotations kept).
fn compact_embed_signature(signature: &str, max_chars: usize) -> String {
    let collapsed: String = signature.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_at_char_boundary(&collapsed, max_chars)
}

fn truncate_at_char_boundary(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        return text.to_string();
    }
    let mut end = max_chars;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Entity retrieval helpers
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// Priority order for context injection: L0 highest, L3 lowest if weakly matched.
/// Used in the "深度优先 + Token 预算" retrieval strategy.
pub fn injection_priority(kind: EntityKind) -> u8 {
    match kind {
        EntityKind::WorkingMemory => 0, // Highest — current session context
        EntityKind::CodeSymbol => 1,    // Very high — code the user is asking about
        EntityKind::CodeFile => 2,
        EntityKind::CodeModule => 3,
        EntityKind::SemanticMemory => 4, // High — permanent patterns (but only if high match)
        EntityKind::AtomicMemory => 5,
        EntityKind::EpisodicMemory => 6, // Lower — unless directly relevant
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_entity_kind_display() {
        assert_eq!(EntityKind::WorkingMemory.to_string(), "WorkingMemory");
        assert_eq!(EntityKind::AtomicMemory.to_string(), "AtomicMemory");
        assert_eq!(EntityKind::EpisodicMemory.to_string(), "EpisodicMemory");
        assert_eq!(EntityKind::SemanticMemory.to_string(), "SemanticMemory");
        assert_eq!(EntityKind::CodeSymbol.to_string(), "CodeSymbol");
    }

    #[test]
    fn test_entity_kind_depth() {
        assert_eq!(EntityKind::WorkingMemory.depth(), Some(0));
        assert_eq!(EntityKind::AtomicMemory.depth(), Some(1));
        assert_eq!(EntityKind::EpisodicMemory.depth(), Some(2));
        assert_eq!(EntityKind::SemanticMemory.depth(), Some(3));
        assert_eq!(EntityKind::CodeSymbol.depth(), None);
    }

    #[test]
    fn test_entity_kind_is_long_term() {
        assert!(!EntityKind::WorkingMemory.is_long_term());
        assert!(!EntityKind::AtomicMemory.is_long_term());
        assert!(EntityKind::EpisodicMemory.is_long_term());
        assert!(EntityKind::SemanticMemory.is_long_term());
    }

    #[test]
    fn test_symbol_type_display() {
        assert_eq!(SymbolType::Function.to_string(), "function");
        assert_eq!(SymbolType::Struct.to_string(), "struct");
        assert_eq!(SymbolType::Trait.to_string(), "trait");
    }

    #[test]
    fn test_symbol_type_from_str() {
        assert_eq!(SymbolType::from_str("function"), Some(SymbolType::Function));
        assert_eq!(SymbolType::from_str("unknown"), None);
    }

    #[test]
    fn test_working_memory_creation() {
        let wm = Entity::working_memory(
            "sess-1",
            "fixed auth bug",
            Some("user reported crash"),
            Some("patched validate_token"),
            vec!["edit_file".into(), "shell_exec".into()],
            true,
        );
        assert_eq!(wm.kind, EntityKind::WorkingMemory);
        assert_eq!(wm.coordinate.depth, 0);
        assert!(wm.content.contains("fixed auth bug"));
        assert!(wm.has_signal());
    }

    #[test]
    fn test_atomic_memory_creation() {
        let am = Entity::atomic_memory(
            "User prefers tabs over spaces",
            "Style",
            Some("my-project"),
            "rust",
            "UserExplicit",
        );
        assert_eq!(am.kind, EntityKind::AtomicMemory);
        assert_eq!(am.coordinate.depth, 1);
        assert!(am.content.contains("tabs"));
    }

    #[test]
    fn test_episodic_memory_creation() {
        let ep = Entity::episodic_memory(
            "Fixed token refresh bug",
            "sess-1",
            Some("my-project"),
            "Fixed a bug where tokens would not refresh after expiry",
        );
        assert_eq!(ep.kind, EntityKind::EpisodicMemory);
        assert_eq!(ep.coordinate.depth, 2);
    }

    #[test]
    fn test_semantic_memory_creation() {
        let sm = Entity::semantic_memory(
            "my-project",
            "This project uses hexagonal architecture with ports/adapters",
            "architecture",
            vec!["ep-1".into(), "ep-2".into()],
        );
        assert_eq!(sm.kind, EntityKind::SemanticMemory);
        assert_eq!(sm.coordinate.depth, 3);
        assert!(sm.is_critical);
    }

    #[test]
    fn test_code_symbol_creation() {
        let cs = Entity::code_symbol(
            "validate_token",
            "auth::validate_token",
            SymbolType::Function,
            "rust",
            "src/auth.rs",
            42,
            58,
            "fn validate_token(token: &Token) -> Result<bool>",
            None,
        );
        assert_eq!(cs.kind, EntityKind::CodeSymbol);
        assert!(cs.content.contains("validate_token"));
    }

    #[test]
    fn test_has_signal_filters_noise() {
        let hello = Entity::atomic_memory("hello", "Fact", None, "en", "ToolObservation");
        assert!(!hello.has_signal());

        let ok = Entity::atomic_memory("ok", "Fact", None, "en", "ToolObservation");
        assert!(!ok.has_signal());

        let real = Entity::atomic_memory(
            "The auth module uses JWT with RS256",
            "Fact",
            None,
            "en",
            "ToolObservation",
        );
        assert!(real.has_signal());
    }

    #[test]
    fn test_injection_priority_order() {
        assert!(
            injection_priority(EntityKind::WorkingMemory)
                < injection_priority(EntityKind::CodeSymbol)
        );
        assert!(
            injection_priority(EntityKind::CodeSymbol)
                < injection_priority(EntityKind::SemanticMemory)
        );
        assert!(
            injection_priority(EntityKind::SemanticMemory)
                < injection_priority(EntityKind::AtomicMemory)
        );
        assert!(
            injection_priority(EntityKind::AtomicMemory)
                < injection_priority(EntityKind::EpisodicMemory)
        );
    }

    #[test]
    fn test_relation_type_roundtrip() {
        for rt in &[
            RelationType::Calls,
            RelationType::ModifiesSymbol,
            RelationType::Abstracts,
        ] {
            let s = rt.as_str();
            let parsed = RelationType::from_str(s);
            assert_eq!(parsed, Some(*rt));
        }
    }
}
