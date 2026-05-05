use std::sync::Arc;
use nova_core::llm_backend::LlmBackend;

/// Static assertion: Arc<dyn LlmBackend> must be Send + Sync
/// This ensures the trait object can be shared across threads safely.
#[test]
fn llm_backend_trait_object_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Arc<dyn LlmBackend>>();
}
