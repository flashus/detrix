//! LLDB process lifecycle management.

mod discovery;
mod manager;

pub use discovery::{find_codelldb, find_lldb_dap};
pub use manager::{LldbManager, LldbProcess};
