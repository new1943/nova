pub mod events;
pub use events::{ShadowEvent, TaskAction};

/// Event emitter trait for shadow events.
/// Implemented by DispatcherSender in nova-daemon to emit events from tools.
pub trait ShadowEventEmitter: Send + Sync {
    fn emit(&self, event: ShadowEvent);
}
