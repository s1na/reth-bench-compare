# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

`reth-bench-compare` is an automated performance comparison tool for reth (Rust Ethereum client) that compares performance between two git references (branches, tags, or commits). The tool handles git operations, compilation, benchmarking, and report generation automatically.

## Architecture

The codebase is organized into distinct modules with clear separation of concerns:

- `main.rs` - Entry point with CLI runner integration and tracing setup
- `cli.rs` - Command-line argument parsing and main orchestration logic
- `git.rs` - Git operations management (checkout, state restoration, validation)
- `compilation.rs` - Reth compilation with caching by commit hash
- `node.rs` - Reth node lifecycle management (startup, readiness checks, shutdown)
- `benchmark.rs` - Integration with reth-bench for performance measurements
- `comparison.rs` - Results analysis and report generation

The tool follows a state management pattern where each module manages a specific aspect of the workflow and ensures proper cleanup on interruption.

## Key Dependencies

- **Reth Dependencies**: Uses specific git revision (`dddde9eff9`) of reth components for shared types
- **Alloy**: RPC communication with external endpoints
- **Tokio**: Async runtime for process management
- **Clap**: CLI argument parsing with derive macros
- **CSV**: Results data parsing and comparison
- **Tracing**: Structured logging throughout the application

## Development Commands

### Building
```bash
cargo build --release
```

### Running Tests
```bash
cargo test
```

### Basic Usage
```bash
cargo run -- --baseline-ref main --feature-ref feature-branch --blocks 100 --datadir /path/to/datadir
```

### Debug Mode
```bash
cargo run -- --baseline-ref main --feature-ref feature --blocks 50 -vvv
```

## Key Design Patterns

### Process Management
- Uses process groups for clean child process termination
- Implements graceful shutdown with signal handling
- Ensures git state restoration even on interruption

### Caching Strategy
- Compiled binaries are cached by git commit hash in `bin/` directory
- Avoids recompilation when switching between previously built references
- Binary naming convention: `reth_{sanitized_ref_name}`

### Safety Mechanisms
- Validates clean git state before operations (allows untracked files)
- Implements automatic git state restoration via `GitGuard`
- Uses JWT secrets for secure engine API communication
- Waits for node sync completion before benchmarking

### External Tool Integration
- **reth-bench**: Must be available in PATH for benchmarking
- **samply**: Auto-installed if needed for CPU profiling (--profile flag)
- **Python + uv**: Required for chart generation (--draw flag)

## Chain Support

The tool supports multiple Ethereum networks with chain-specific RPC defaults:
- mainnet, sepolia, holesky, etc.
- Configurable via `--chain` parameter
- Default RPC URLs are provided but can be overridden with `--rpc-url`

## Output Structure

Results are organized in timestamped directories under `results/`:
- `baseline/` and `feature/` subdirectories contain CSV data
- `comparison_report.json` contains detailed metrics comparison
- Optional `latency_comparison.png` chart if `--draw` is used
- CPU profiles stored in `profiles/` as compressed JSON files

## Performance Metrics

The tool measures and compares:
- NewPayload latency
- ForkchoiceUpdated latency  
- Total latency
- Gas processed per second
- Blocks processed per second

All metrics include statistical analysis (mean, median, percentiles) in the comparison report.