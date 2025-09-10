# Geth Support Architecture Plan

## Overview

This document outlines the architecture changes needed to add geth support to the reth-bench-compare tool. The goal is to create a client-agnostic comparison tool that can benchmark both reth and geth using the same standardized engine API measurements.

## Current Architecture Limitations

The current codebase is tightly coupled to reth in several areas:

1. **Compilation**: Hardcoded `make profiling` for Rust/Cargo builds
2. **Node Management**: Reth-specific CLI flags and startup patterns  
3. **Dependencies**: Uses reth crates for CLI, tracing, chainspec
4. **Binary Caching**: Assumes reth binary naming and locations
5. **Configuration**: Reth-specific datadir and JWT conventions

## Proposed Architecture

### 1. Client Abstraction Layer

Create a trait-based abstraction to handle client-specific operations:

```rust
// src/client.rs
pub trait EthereumClient {
    async fn compile(&self, git_ref: &str, commit: &str) -> Result<PathBuf>;
    async fn start_node(&self, binary_path: &Path, additional_args: &[String]) -> Result<tokio::process::Child>;
    async fn wait_for_ready(&self) -> Result<u64>;
    async fn stop_node(&self, child: &mut tokio::process::Child) -> Result<()>;
    async fn unwind_to_block(&self, block_number: u64) -> Result<()>;
    fn get_cached_binary_path(&self, commit: &str) -> PathBuf;
}
```

### 2. Client Implementations

#### RethClient
- Implements existing functionality from `compilation.rs` and `node.rs`
- Uses `make release` instead of `make profiling` (simplified build)
- Maintains reth-specific CLI arguments and configuration

#### GethClient  
- Implements Go/Make build system integration
- Uses geth-specific CLI flags and patterns
- Handles geth datadir and JWT conventions

### 3. Simplified Build Strategy

**Remove profiling complexity for initial implementation:**
- Reth: `make release` → `target/release/reth`
- Geth: `make geth` → `build/bin/geth`
- Skip samply integration (can be added later as enhancement)

### 4. File Structure Changes

```
src/
├── main.rs                 # Entry point (unchanged)
├── cli.rs                  # Updated with --client flag
├── client.rs               # New: Client trait definition
├── clients/
│   ├── mod.rs             # Module exports
│   ├── reth.rs            # RethClient implementation
│   └── geth.rs            # GethClient implementation  
├── git.rs                  # Unchanged
├── benchmark.rs            # Unchanged (reth-bench works with both)
└── comparison.rs           # Unchanged
```

**Remove/Refactor:**
- `compilation.rs` → absorbed into client implementations
- `node.rs` → absorbed into client implementations

## Implementation Plan

### Phase 1: Create Abstraction
1. Create `src/client.rs` with `EthereumClient` trait
2. Create `src/clients/` module structure
3. Move existing reth logic to `RethClient`

### Phase 2: Geth Implementation
1. Implement `GethClient` with Go build system
2. Add geth-specific node startup and configuration
3. Handle geth JWT and datadir conventions

### Phase 3: CLI Integration  
1. Add `--client` flag to choose between `reth` and `geth`
2. Update main orchestration to use client factory
3. Remove reth-specific dependencies where possible

### Phase 4: Testing & Refinement
1. Test both client implementations
2. Update documentation and examples
3. Add error handling for mixed client scenarios

## Key Changes by Module

### CLI (`cli.rs`)
```rust
#[derive(Debug, Parser)]
pub struct Args {
    // Existing args...
    
    /// Ethereum client to benchmark (reth or geth)
    #[arg(long, value_name = "CLIENT", default_value = "reth")]
    pub client: String,
}
```

### Main Orchestration
Replace direct usage of `CompilationManager` and `NodeManager` with:
```rust
let client: Box<dyn EthereumClient> = match args.client.as_str() {
    "reth" => Box::new(RethClient::new(&args)?),
    "geth" => Box::new(GethClient::new(&args)?),
    _ => return Err(eyre!("Unsupported client: {}", args.client)),
};
```

### Dependency Changes
- Keep reth dependencies for RethClient only
- Add minimal Go/system dependencies for GethClient
- Use generic types (Alloy) for Engine API communication

## Binary Caching Strategy

**Current**: `reth_{commit_hash}`
**Proposed**: `{client}_{commit_hash}`
- `reth_a1b2c3d4`
- `geth_a1b2c3d4`

## Configuration Mapping

### Chain Names
Both clients support standard chain names, but mapping may be needed:
- Most chains: direct mapping (`mainnet`, `sepolia`, `holesky`)
- Client-specific chains: handle in client implementations

### RPC Defaults
Keep existing RPC URL defaults, both clients can use the same external RPC endpoints.

### Datadir Structure
- **Reth**: `<datadir>/<chain>/` 
- **Geth**: `<datadir>/` (chain determined by other flags)

### JWT Paths
- **Reth**: `<datadir>/<chain>/jwt.hex`
- **Geth**: `<datadir>/geth/jwtsecret` or explicit path

## Engine API Compatibility

Both clients implement the standard Ethereum Engine API:
- `engine_newPayloadV1/V2/V3`  
- `engine_forkchoiceUpdatedV1/V2/V3`
- Same JWT authentication mechanism
- Same HTTP/JSON-RPC transport

This means benchmarking logic remains unchanged - only compilation and node management need client-specific implementations.

## Migration Strategy

1. **Maintain backward compatibility**: Default to reth client
2. **Incremental rollout**: Implement and test reth client first
3. **Shared validation**: Use same git operations and benchmark logic
4. **Error handling**: Clear error messages for unsupported combinations

## Benefits

1. **Extensibility**: Easy to add more clients (e.g., nethermind, besu)
2. **Maintainability**: Clear separation of concerns
3. **Testing**: Can compare different clients on same hardware
4. **Community**: Broader ecosystem engagement beyond reth

## Risks & Mitigations

**Risk**: Different client behaviors during startup/sync
**Mitigation**: Client-specific readiness detection logic

**Risk**: JWT/auth differences  
**Mitigation**: Client-specific configuration handling

**Risk**: Build system complexity
**Mitigation**: Start with simple release builds, add optimizations later

**Risk**: Cross-client result comparison validity
**Mitigation**: Document client-specific characteristics in reports