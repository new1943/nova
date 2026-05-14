pub mod memory;
pub mod sidequery;
pub mod session;

// Re-export key types
pub use memory::{
    DualWriteMemory, MemoryType, MemoryRecall,
    MemoryConsolidator, TopicTracker, Topic, TopicStatus, TopicTransition,
    TensionTracker, TensionCalculator, UserState, SessionState, Emotion, Context, UserIntent,
    ModeRouter, Mode, MemoryBoard,
};
pub use sidequery::SideQuery;
pub use session::manager::{Session, SessionManager, SessionMeta, TokenStats};
pub use session::AgenticSessionSearch;
