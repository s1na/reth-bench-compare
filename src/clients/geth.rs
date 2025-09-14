//! Geth client implementation

use crate::{cli::Args, client::EthereumClient, git::GitManager};
use alloy_provider::{Provider, ProviderBuilder};
use alloy_rpc_types_eth::SyncStatus;
use eyre::{eyre, OptionExt, Result, WrapErr};
#[cfg(unix)]
use nix::sys::signal::{killpg, Signal};
#[cfg(unix)]
use nix::unistd::Pid;
use reth_chainspec::Chain;
use std::{fs, path::PathBuf, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, BufReader as AsyncBufReader},
    process::{Child, Command},
    time::{sleep, timeout},
};
use tracing::{debug, info, warn};

/// Geth client implementation
pub struct GethClient {
    repo_root: String,
    output_dir: PathBuf,
    git_manager: GitManager,
    datadir: Option<String>,
    chain: Chain,
    use_sudo: bool,
    binary_path: Option<PathBuf>,
    additional_reth_args: Vec<String>, // Keep same name for compatibility, but these are geth args
}

impl GethClient {
    /// Create a new GethClient from CLI args
    pub fn new(args: &Args, git_manager: GitManager) -> Result<Self> {
        let repo_root = std::env::current_dir()?
            .to_string_lossy()
            .to_string();
        
        Ok(Self {
            repo_root,
            output_dir: args.output_dir_path(),
            git_manager,
            datadir: Some(args.datadir_path().to_string_lossy().to_string()),
            chain: args.chain,
            use_sudo: args.sudo,
            binary_path: None,
            additional_reth_args: args.reth_args.clone(), // Reuse for geth args
        })
    }

    /// Map reth chain names to geth network IDs/names
    fn get_geth_network_args(&self) -> Vec<String> {
        match self.chain.to_string().as_str() {
            "mainnet" => vec![], // Default for geth
            "sepolia" => vec!["--sepolia".to_string()],
            "holesky" => vec!["--holesky".to_string()],
            "goerli" => vec!["--goerli".to_string()],
            chain => {
                warn!("Unknown chain '{}' for geth, using mainnet", chain);
                vec![]
            }
        }
    }

    /// Get JWT secret path for geth (different convention than reth)
    fn get_jwt_secret_path(&self) -> PathBuf {
        if let Some(ref datadir) = self.datadir {
            // Geth uses <datadir>/geth/jwtsecret by default
            PathBuf::from(datadir).join("geth").join("jwtsecret")
        } else {
            // Fallback to current directory
            PathBuf::from("./jwtsecret")
        }
    }

    /// Build geth arguments as a vector of strings
    fn build_geth_args(
        &self,
        binary_path_str: &str,
        additional_args: &[String],
    ) -> Vec<String> {
        let mut geth_args = vec![binary_path_str.to_string()];

        // Add network arguments
        geth_args.extend(self.get_geth_network_args());

        // Add datadir if specified
        if let Some(ref datadir) = self.datadir {
            geth_args.extend_from_slice(&["--datadir".to_string(), datadir.clone()]);
        }

        // Geth-specific arguments for engine API and RPC
        geth_args.extend_from_slice(&[
            "--authrpc.jwtsecret".to_string(),
            self.get_jwt_secret_path().to_string_lossy().to_string(),
            
            // Regular JSON-RPC (for sync status checks)
            "--http".to_string(),
            "--http.api".to_string(),
            "eth,debug".to_string(), // Enable debug API for debug_setHead

            // Sync and networking
            "--nodiscover".to_string(), // Disable peer discovery like reth's --disable-discovery
            "--maxpeers".to_string(),
            "0".to_string(),

            // Disable history storage for performance
            "--history.transactions".to_string(),
            "0".to_string(),
            "--history.logs.disable".to_string(),
        ]);

        // Add any additional arguments passed via command line
        geth_args.extend_from_slice(&self.additional_reth_args);

        // Add reference-specific additional arguments
        geth_args.extend_from_slice(additional_args);

        geth_args
    }

    /// Create a command for direct geth execution
    fn create_direct_command(&self, geth_args: &[String]) -> Command {
        let binary_path = &geth_args[0];

        if self.use_sudo {
            info!("Starting geth node with sudo...");
            let mut cmd = Command::new("sudo");
            cmd.args(geth_args);
            cmd
        } else {
            info!("Starting geth node...");
            let mut cmd = Command::new(binary_path);
            cmd.args(&geth_args[1..]); // Skip the binary path since it's the command
            cmd
        }
    }

    /// Create JWT secret file if it doesn't exist
    async fn ensure_jwt_secret(&self) -> Result<()> {
        let jwt_path = self.get_jwt_secret_path();
        
        if jwt_path.exists() {
            info!("Using existing JWT secret at: {:?}", jwt_path);
            return Ok(());
        }

        // Create parent directories if they don't exist
        if let Some(parent) = jwt_path.parent() {
            fs::create_dir_all(parent).wrap_err("Failed to create JWT secret directory")?;
        }

        // Generate a random 32-byte hex string for JWT secret
        use rand::Rng;
        let mut rng = rand::thread_rng();
        let secret: [u8; 32] = rng.gen();
        let secret_hex = hex::encode(secret);

        fs::write(&jwt_path, secret_hex).wrap_err("Failed to write JWT secret file")?;
        info!("Generated JWT secret at: {:?}", jwt_path);
        
        Ok(())
    }
}

#[async_trait::async_trait]
impl EthereumClient for GethClient {
    async fn compile(&self, _git_ref: &str, commit: &str) -> Result<PathBuf> {
        // Validate that current git commit matches the expected commit
        let current_commit = self.git_manager.get_current_commit()?;
        if current_commit != commit {
            return Err(eyre!(
                "Git commit mismatch! Expected: {}, but currently at: {}",
                &commit[..8],
                &current_commit[..8]
            ));
        }

        let cached_path = self.get_cached_binary_path(commit);

        // Check if cached binary already exists
        if cached_path.exists() {
            info!("Using cached geth binary (commit: {})", &commit[..8]);
            return Ok(cached_path);
        }

        info!("No cached binary found, compiling geth (commit: {})...", &commit[..8]);

        // Use make geth for Go build
        let mut cmd = Command::new("make");
        cmd.arg("geth").current_dir(&self.repo_root);

        debug!("Executing make command: {:?}", cmd);

        let output = cmd
            .output()
            .await
            .wrap_err("Failed to execute make geth command")?;

        // Print stdout and stderr with prefixes at debug level
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        for line in stdout.lines() {
            if !line.trim().is_empty() {
                debug!("[MAKE] {}", line);
            }
        }

        for line in stderr.lines() {
            if !line.trim().is_empty() {
                debug!("[MAKE] {}", line);
            }
        }

        if !output.status.success() {
            return Err(eyre!(
                "Geth compilation failed with exit code: {:?}",
                output.status.code()
            ));
        }

        info!("Geth compilation completed");

        // Copy the compiled binary to cache
        let source_path = PathBuf::from(&self.repo_root).join("build/bin/geth");
        if !source_path.exists() {
            return Err(eyre!("Compiled geth binary not found at {:?}", source_path));
        }

        // Create bin directory if it doesn't exist
        let bin_dir = self.output_dir.join("bin");
        fs::create_dir_all(&bin_dir).wrap_err("Failed to create bin directory")?;

        // Copy binary to cache
        fs::copy(&source_path, &cached_path).wrap_err("Failed to copy binary to cache")?;

        // Make the cached binary executable
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&cached_path)?.permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&cached_path, perms)?;
        }

        info!("Cached compiled geth binary at: {:?}", cached_path);
        Ok(cached_path)
    }

    async fn start_node(
        &mut self,
        binary_path: &std::path::Path,
        _git_ref: &str,
        _ref_type: &str,
        additional_args: &[String],
    ) -> Result<Child> {
        // Store the binary path for later use
        self.binary_path = Some(binary_path.to_path_buf());

        // Ensure JWT secret exists before starting node
        self.ensure_jwt_secret().await?;

        let binary_path_str = binary_path.to_string_lossy();
        let geth_args = self.build_geth_args(&binary_path_str, additional_args);

        // Log additional arguments if any
        if !self.additional_reth_args.is_empty() {
            info!(
                "Using common additional geth arguments: {:?}",
                self.additional_reth_args
            );
        }
        if !additional_args.is_empty() {
            info!(
                "Using reference-specific additional geth arguments: {:?}",
                additional_args
            );
        }

        info!("Built geth args: {:?}", geth_args);
        
        let mut cmd = self.create_direct_command(&geth_args);

        // Don't set process group to avoid SIGTTOU issues when geth writes to terminal

        info!("Final command being executed: {:?}", cmd);
        debug!("Executing geth command: {cmd:?}");

        let mut child = cmd
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .wrap_err("Failed to start geth node")?;

        info!(
            "Geth node started with PID: {:?} (binary: {})",
            child.id().ok_or_eyre("Geth node is not running")?,
            binary_path_str
        );

        // Temporarily disable logging capture to avoid hanging
        info!("Skipping geth logging capture to avoid deadlock");
        
        // Drop the streams to prevent geth from blocking
        drop(child.stdout.take());
        drop(child.stderr.take());

        // Skip the sleep entirely to test if the function can return
        info!("Skipping sleep - returning immediately");

        Ok(child)
    }

    async fn wait_for_ready(&self) -> Result<u64> {
        info!("Waiting for geth node to be ready and synced...");

        let max_wait = Duration::from_secs(120);
        let check_interval = Duration::from_secs(2);
        let rpc_url = "http://localhost:8545";

        info!("Starting timeout block with max_wait: {:?}", max_wait);
        let result = timeout(max_wait, async move {
            info!("Inside timeout async block");
            
            // Create Alloy provider inside async block
            info!("Parsing RPC URL: {}", rpc_url);
            let url = rpc_url
                .parse()
                .map_err(|e| eyre!("Invalid RPC URL '{}': {}", rpc_url, e))?;
            info!("Creating Alloy provider inside async block...");
            let provider = ProviderBuilder::new().connect_http(url);
            info!("Provider created inside async block");
            loop {
                info!("Checking geth RPC status...");
                // First check if RPC is up and node is not syncing
                match provider.syncing().await {
                    Ok(sync_result) => {
                        match sync_result {
                            SyncStatus::Info(sync_info)
                                if sync_info.current_block != sync_info.highest_block =>
                            {
                                info!("Geth node is still syncing: current_block={}, highest_block={}, waiting...", 
                                      sync_info.current_block, sync_info.highest_block);
                            }
                            SyncStatus::Info(sync_info) => {
                                info!("Geth node sync status: current_block={}, highest_block={} (synced)", 
                                      sync_info.current_block, sync_info.highest_block);
                                // Node is synced, now get the tip
                                match provider.get_block_number().await {
                                    Ok(tip) => {
                                        info!("Geth node is ready and not syncing at block: {}", tip);
                                        return Ok(tip);
                                    }
                                    Err(e) => {
                                        info!("Failed to get block number: {}", e);
                                    }
                                }
                            }
                            SyncStatus::None => {
                                info!("Geth node is not syncing (SyncStatus::None)");
                                // Node is not syncing, now get the tip
                                match provider.get_block_number().await {
                                    Ok(tip) => {
                                        info!("Geth node is ready and not syncing at block: {}", tip);
                                        return Ok(tip);
                                    }
                                    Err(e) => {
                                        info!("Failed to get block number: {}", e);
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        info!("Geth node RPC not ready yet or failed to check sync status: {}", e);
                    }
                }

                sleep(check_interval).await;
            }
        })
        .await
        .wrap_err("Timed out waiting for geth node to be ready and synced")?;

        result
    }

    async fn stop_node(&self, child: &mut Child) -> Result<()> {
        let pid = child.id().expect("Child process ID should be available");

        // Check if the process has already exited
        match child.try_wait() {
            Ok(Some(status)) => {
                info!(
                    "Geth node (PID: {}) has already exited with status: {:?}",
                    pid, status
                );
                return Ok(());
            }
            Ok(None) => {
                info!("Stopping geth process gracefully with SIGINT (PID: {})...", pid);
            }
            Err(e) => {
                return Err(eyre!("Failed to check geth process status: {}", e));
            }
        }

        #[cfg(unix)]
        {
            // Send SIGINT to process group
            let nix_pgid = Pid::from_raw(pid as i32);

            match killpg(nix_pgid, Signal::SIGINT) {
                Ok(()) => {}
                Err(nix::errno::Errno::ESRCH) => {
                    info!("Geth process group {} has already exited", pid);
                }
                Err(e) => {
                    return Err(eyre!(
                        "Failed to send SIGINT to geth process group {}: {}",
                        pid, e
                    ));
                }
            }
        }

        #[cfg(not(unix))]
        {
            let output = Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/F"])
                .output()
                .await
                .wrap_err("Failed to execute taskkill command")?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                if stderr.contains("not found") || stderr.contains("not exist") {
                    info!("Geth process {} has already exited", pid);
                } else {
                    return Err(eyre!("Failed to kill geth process {}: {}", pid, stderr));
                }
            }
        }

        // Wait for the process to exit
        match child.wait().await {
            Ok(status) => {
                info!("Geth node (PID: {}) exited with status: {:?}", pid, status);
            }
            Err(e) => {
                debug!("Error waiting for geth process exit (may have already exited): {}", e);
            }
        }

        Ok(())
    }

    async fn unwind_to_block(&self, block_number: u64) -> Result<()> {
        info!("Unwinding geth node to block: {} using debug_setHead", block_number);

        // Geth uses debug_setHead API to rewind to a specific block
        // This needs to be called when the node is running
        let rpc_url = "http://localhost:8545";

        // Create Alloy provider
        let url = rpc_url
            .parse()
            .map_err(|e| eyre!("Invalid RPC URL '{}': {}", rpc_url, e))?;
        let provider = ProviderBuilder::new().connect_http(url);

        // Convert block number to hex format (required by debug_setHead)
        let hex_block = format!("0x{:x}", block_number);

        // Send the raw RPC request using debug_setHead
        let response: serde_json::Value = provider
            .client()
            .request("debug_setHead", [hex_block.as_str()])
            .await
            .wrap_err("Failed to call debug_setHead on geth node")?;

        debug!("debug_setHead response: {:?}", response);
        
        info!("Successfully unwound geth node to block: {}", block_number);
        Ok(())
    }

    fn get_cached_binary_path(&self, commit: &str) -> PathBuf {
        let identifier = &commit[..8]; // Use first 8 chars of commit
        let binary_name = format!("geth_{}", identifier);
        self.output_dir.join("bin").join(binary_name)
    }

    fn client_name(&self) -> &'static str {
        "geth"
    }

    fn requires_node_for_unwind(&self) -> bool {
        true // Geth uses debug_setHead RPC which requires running node
    }
}
