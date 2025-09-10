//! Reth client implementation

use crate::{cli::Args, client::EthereumClient, git::GitManager};
use alloy_primitives::address;
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

/// Reth client implementation
pub struct RethClient {
    repo_root: String,
    output_dir: PathBuf,
    git_manager: GitManager,
    datadir: Option<String>,
    metrics_port: u16,
    chain: Chain,
    use_sudo: bool,
    binary_path: Option<PathBuf>,
    additional_reth_args: Vec<String>,
}

impl RethClient {
    /// Create a new RethClient from CLI args
    pub fn new(args: &Args, git_manager: GitManager) -> Result<Self> {
        let repo_root = std::env::current_dir()?
            .to_string_lossy()
            .to_string();
        
        Ok(Self {
            repo_root,
            output_dir: args.output_dir_path(),
            git_manager,
            datadir: Some(args.datadir_path().to_string_lossy().to_string()),
            metrics_port: args.metrics_port,
            chain: args.chain,
            use_sudo: args.sudo,
            binary_path: None,
            additional_reth_args: args.reth_args.clone(),
        })
    }

    /// Detect if the RPC endpoint is an Optimism chain
    async fn detect_optimism_chain(&self, rpc_url: &str) -> Result<bool> {
        info!("Detecting chain type from RPC endpoint...");
        
        // Create Alloy provider
        let url = rpc_url
            .parse()
            .map_err(|e| eyre!("Invalid RPC URL '{}': {}", rpc_url, e))?;
        let provider = ProviderBuilder::new().connect_http(url);

        // Check for Optimism predeploy at address 0x420000000000000000000000000000000000000F
        let is_optimism = !provider
            .get_code_at(address!("0x420000000000000000000000000000000000000F"))
            .await?
            .is_empty();

        if is_optimism {
            info!("Detected Optimism chain");
        } else {
            info!("Detected Ethereum chain");
        }

        Ok(is_optimism)
    }

    /// Build reth arguments as a vector of strings
    fn build_reth_args(
        &self,
        binary_path_str: &str,
        additional_args: &[String],
    ) -> Vec<String> {
        let mut reth_args = vec![binary_path_str.to_string(), "node".to_string()];

        // Add chain argument (skip for mainnet as it's the default)
        let chain_str = self.chain.to_string();
        if chain_str != "mainnet" {
            reth_args.extend_from_slice(&["--chain".to_string(), chain_str]);
        }

        // Add datadir if specified
        if let Some(ref datadir) = self.datadir {
            reth_args.extend_from_slice(&["--datadir".to_string(), datadir.clone()]);
        }

        // Add reth-specific arguments
        let metrics_arg = format!("0.0.0.0:{}", self.metrics_port);
        reth_args.extend_from_slice(&[
            "--engine.accept-execution-requests-hash".to_string(),
            "--metrics".to_string(),
            metrics_arg,
            "--http".to_string(),
            "--http.api".to_string(),
            "eth".to_string(),
            "--disable-discovery".to_string(),
            "--trusted-only".to_string(),
        ]);

        // Add any additional arguments passed via command line
        reth_args.extend_from_slice(&self.additional_reth_args);

        // Add reference-specific additional arguments
        reth_args.extend_from_slice(additional_args);

        reth_args
    }

    /// Create a command for direct reth execution
    fn create_direct_command(&self, reth_args: &[String]) -> Command {
        let binary_path = &reth_args[0];

        if self.use_sudo {
            info!("Starting reth node with sudo...");
            let mut cmd = Command::new("sudo");
            cmd.args(reth_args);
            cmd
        } else {
            info!("Starting reth node...");
            let mut cmd = Command::new(binary_path);
            cmd.args(&reth_args[1..]); // Skip the binary path since it's the command
            cmd
        }
    }
}

#[async_trait::async_trait]
impl EthereumClient for RethClient {
    async fn compile(&self, git_ref: &str, commit: &str) -> Result<PathBuf> {
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
            info!("Using cached reth binary (commit: {})", &commit[..8]);
            return Ok(cached_path);
        }

        info!("No cached binary found, compiling reth (commit: {})...", &commit[..8]);

        // Use make release instead of make profiling for simplicity
        let mut cmd = Command::new("make");
        cmd.arg("release").current_dir(&self.repo_root);

        debug!("Executing make command: {:?}", cmd);

        let output = cmd
            .output()
            .await
            .wrap_err("Failed to execute make release command")?;

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
                "Reth compilation failed with exit code: {:?}",
                output.status.code()
            ));
        }

        info!("Reth compilation completed");

        // Copy the compiled binary to cache
        let source_path = PathBuf::from(&self.repo_root).join("target/release/reth");
        if !source_path.exists() {
            return Err(eyre!("Compiled binary not found at {:?}", source_path));
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

        info!("Cached compiled binary at: {:?}", cached_path);
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

        let binary_path_str = binary_path.to_string_lossy();
        let reth_args = self.build_reth_args(&binary_path_str, additional_args);

        // Log additional arguments if any
        if !self.additional_reth_args.is_empty() {
            info!(
                "Using common additional reth arguments: {:?}",
                self.additional_reth_args
            );
        }
        if !additional_args.is_empty() {
            info!(
                "Using reference-specific additional reth arguments: {:?}",
                additional_args
            );
        }

        let mut cmd = self.create_direct_command(&reth_args);

        // Set process group for better signal handling
        #[cfg(unix)]
        {
            cmd.process_group(0);
        }

        debug!("Executing reth command: {cmd:?}");

        let mut child = cmd
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .wrap_err("Failed to start reth node")?;

        info!(
            "Reth node started with PID: {:?} (binary: {})",
            child.id().ok_or_eyre("Reth node is not running")?,
            binary_path_str
        );

        // Stream stdout and stderr with prefixes at debug level
        if let Some(stdout) = child.stdout.take() {
            tokio::spawn(async move {
                let reader = AsyncBufReader::new(stdout);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    debug!("[RETH] {}", line);
                }
            });
        }

        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let reader = AsyncBufReader::new(stderr);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    debug!("[RETH] {}", line);
                }
            });
        }

        // Give the node a moment to start up
        sleep(Duration::from_secs(5)).await;

        Ok(child)
    }

    async fn wait_for_ready(&self) -> Result<u64> {
        info!("Waiting for reth node to be ready and synced...");

        let max_wait = Duration::from_secs(120);
        let check_interval = Duration::from_secs(2);
        let rpc_url = "http://localhost:8545";

        // Create Alloy provider
        let url = rpc_url
            .parse()
            .map_err(|e| eyre!("Invalid RPC URL '{}': {}", rpc_url, e))?;
        let provider = ProviderBuilder::new().connect_http(url);

        let result = timeout(max_wait, async {
            loop {
                // First check if RPC is up and node is not syncing
                match provider.syncing().await {
                    Ok(sync_result) => {
                        match sync_result {
                            SyncStatus::Info(sync_info)
                                if sync_info.current_block != sync_info.highest_block =>
                            {
                                debug!("Node is still syncing {sync_info:?}, waiting...");
                            }
                            _ => {
                                // Node is not syncing, now get the tip
                                match provider.get_block_number().await {
                                    Ok(tip) => {
                                        info!("Reth node is ready and not syncing at block: {}", tip);
                                        return Ok(tip);
                                    }
                                    Err(e) => {
                                        debug!("Failed to get block number: {}", e);
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        debug!("Node RPC not ready yet or failed to check sync status: {}", e);
                    }
                }

                sleep(check_interval).await;
            }
        })
        .await
        .wrap_err("Timed out waiting for reth node to be ready and synced")?;

        result
    }

    async fn stop_node(&self, child: &mut Child) -> Result<()> {
        let pid = child.id().expect("Child process ID should be available");

        // Check if the process has already exited
        match child.try_wait() {
            Ok(Some(status)) => {
                info!(
                    "Reth node (PID: {}) has already exited with status: {:?}",
                    pid, status
                );
                return Ok(());
            }
            Ok(None) => {
                info!("Stopping reth process gracefully with SIGINT (PID: {})...", pid);
            }
            Err(e) => {
                return Err(eyre!("Failed to check reth process status: {}", e));
            }
        }

        #[cfg(unix)]
        {
            // Send SIGINT to process group
            let nix_pgid = Pid::from_raw(pid as i32);

            match killpg(nix_pgid, Signal::SIGINT) {
                Ok(()) => {}
                Err(nix::errno::Errno::ESRCH) => {
                    info!("Reth process group {} has already exited", pid);
                }
                Err(e) => {
                    return Err(eyre!(
                        "Failed to send SIGINT to reth process group {}: {}",
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
                    info!("Reth process {} has already exited", pid);
                } else {
                    return Err(eyre!("Failed to kill reth process {}: {}", pid, stderr));
                }
            }
        }

        // Wait for the process to exit
        match child.wait().await {
            Ok(status) => {
                info!("Reth node (PID: {}) exited with status: {:?}", pid, status);
            }
            Err(e) => {
                debug!("Error waiting for reth process exit (may have already exited): {}", e);
            }
        }

        Ok(())
    }

    async fn unwind_to_block(&self, block_number: u64) -> Result<()> {
        if self.use_sudo {
            info!("Unwinding reth node to block: {} (with sudo)", block_number);
        } else {
            info!("Unwinding reth node to block: {}", block_number);
        }

        // Use the binary path from the last start_node call
        let binary_path = self
            .binary_path
            .as_ref()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "./target/release/reth".to_string());

        let mut cmd = if self.use_sudo {
            let mut sudo_cmd = Command::new("sudo");
            sudo_cmd.args([&binary_path, "stage", "unwind"]);
            sudo_cmd
        } else {
            let mut reth_cmd = Command::new(&binary_path);
            reth_cmd.args(["stage", "unwind"]);
            reth_cmd
        };

        // Add chain argument (skip for mainnet as it's the default)
        let chain_str = self.chain.to_string();
        if chain_str != "mainnet" {
            cmd.args(["--chain", &chain_str]);
        }

        // Add datadir if specified
        if let Some(ref datadir) = self.datadir {
            cmd.args(["--datadir", datadir]);
        }

        cmd.args(["to-block", &block_number.to_string()]);

        debug!("Executing reth unwind command: {:?}", cmd);

        let mut child = cmd
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .wrap_err("Failed to start reth unwind command")?;

        // Stream stdout and stderr with prefixes
        if let Some(stdout) = child.stdout.take() {
            tokio::spawn(async move {
                let reader = AsyncBufReader::new(stdout);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    debug!("[RETH-UNWIND] {}", line);
                }
            });
        }

        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let reader = AsyncBufReader::new(stderr);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    debug!("[RETH-UNWIND] {}", line);
                }
            });
        }

        let status = child
            .wait()
            .await
            .wrap_err("Failed to wait for reth unwind command")?;

        if !status.success() {
            return Err(eyre!(
                "Reth unwind command failed with exit code: {:?}",
                status.code()
            ));
        }

        info!("Unwound reth to block: {}", block_number);
        Ok(())
    }

    fn get_cached_binary_path(&self, commit: &str) -> PathBuf {
        let identifier = &commit[..8]; // Use first 8 chars of commit
        let binary_name = format!("reth_{}", identifier);
        self.output_dir.join("bin").join(binary_name)
    }

    fn client_name(&self) -> &'static str {
        "reth"
    }
}