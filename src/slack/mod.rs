pub mod client;
mod thread_activity;
// Fields in types are used for serde deserialization even if not read directly.
#[expect(dead_code, reason = "serde deserialization fields")]
pub mod types;
