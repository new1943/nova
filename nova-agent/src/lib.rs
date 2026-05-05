pub mod agent_loop;
pub mod context;
pub mod forked;
pub mod preflight;
pub mod stages;
pub mod coordinator;
pub mod subagent;
pub mod hooks;
pub mod token;
pub mod workspace;
pub mod heartbeat;
pub mod delegate;

// Re-export key types
pub use agent_loop::{QueryLoop, QueryLoopConfig, LoopEvent};
pub use preflight::PreFlightChecker;
pub use coordinator::{Coordinator, CoordinatorPhase};
pub use subagent::{SubagentSpawner, SubagentConfig, SubagentType, SubagentHandle};
pub use hooks::HookManager;
pub use context::CallerContext;
pub use forked::ForkedAgent;
pub use token::budget::{TokenBudget, BudgetCheck};
pub use token::compact::Compactor;
pub use token::counter;
pub use workspace::BootstrapLoader;
pub use heartbeat::HeartbeatScheduler;
