pub mod server;
pub mod client;
pub type Error = Box<dyn std::error::Error + Send + Sync>;