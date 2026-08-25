include!("memory_impl_core.rs");
include!("memory_impl_controller.rs");

impl CheckpointStore for InMemoryCheckpointStore {
memory_impl_core!();
memory_impl_controller!();
}
