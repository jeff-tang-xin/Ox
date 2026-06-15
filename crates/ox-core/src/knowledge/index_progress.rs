//! Tagged indexing progress — avoids mixing AST file counts with embedding entity counts.

/// Progress event from background indexing (AST walk or embedding).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexProgress {
    /// Phase 1: walking files and extracting symbols.
    Parsing {
        files_done: usize,
        files_total: usize,
        symbols_so_far: usize,
    },
    /// Phase 2: BERT embedding entities.
    Embedding {
        entities_done: usize,
        entities_total: usize,
    },
}

impl IndexProgress {
    pub fn parsing(files_done: usize, files_total: usize, symbols_so_far: usize) -> Self {
        Self::Parsing {
            files_done,
            files_total,
            symbols_so_far,
        }
    }

    pub fn embedding(entities_done: usize, entities_total: usize) -> Self {
        Self::Embedding {
            entities_done,
            entities_total,
        }
    }
}
