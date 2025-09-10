//! Client abstraction layer for different Ethereum clients (reth, geth, etc.)

use eyre::Result;
use std::path::{Path, PathBuf};
use tokio::process::Child;

/// Trait defining common operations for Ethereum clients
#[async_trait::async_trait]
pub trait EthereumClient: Send + Sync {
    /// Compile the client for the given git reference and commit
    /// Returns the path to the compiled binary
    async fn compile(&self, git_ref: &str, commit: &str) -> Result<PathBuf>;

    /// Start a node instance with the given binary and additional arguments
    /// Returns the child process handle
    async fn start_node(
        &mut self,
        binary_path: &Path,
        git_ref: &str,
        ref_type: &str,
        additional_args: &[String],
    ) -> Result<Child>;

    /// Wait for the node to be ready and return the current tip block number
    async fn wait_for_ready(&self) -> Result<u64>;

    /// Stop the node gracefully
    async fn stop_node(&self, child: &mut Child) -> Result<()>;

    /// Unwind the node to a specific block number
    async fn unwind_to_block(&self, block_number: u64) -> Result<()>;

    /// Get the cached binary path for a given commit
    fn get_cached_binary_path(&self, commit: &str) -> PathBuf;

    /// Get the client name (e.g., "reth", "geth")
    fn client_name(&self) -> &'static str;
}