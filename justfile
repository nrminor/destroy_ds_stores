# justfile for dds (destroy_ds_stores)
# Load environment variables from .env if it exists

set dotenv-load := true

# Set shell for Windows compatibility

set windows-shell := ["powershell.exe", "-c"]

# Default recipe - show available commands
default:
    @just --list

alias d := dev

# Generate debug build
[group('dev')]
dev: build

# Build debug version
[group('build')]
build:
    cargo build

# Build release version
[group('build')]
build-release:
    cargo build --release

alias b := build-release

# Install locally
[group('install')]
install:
    cargo install --path=.

alias i := install

# Run tests
[group('test')]
test:
    SQLX_OFFLINE=true cargo test

alias t := test

# Run tests with output
[group('test')]
test-verbose:
    SQLX_OFFLINE=true cargo test -- --nocapture

# Format code
[group('dev')]
fmt:
    cargo fmt

alias f := fmt

# Run clippy linter
[group('dev')]
clippy:
    cargo clippy

alias c := clippy

# Run all checks (fmt, clippy, test)
[group('dev')]
check: fmt clippy test

# Clean build artifacts
[group('build')]
clean:
    cargo clean

# Run with arguments
[group('run')]
run *args:
    cargo run -- {{ args }}

# Run in release mode with arguments
[group('run')]
run-release *args:
    cargo run --release -- {{ args }}

# Quick test - dry run in current directory
[group('run')]
quick:
    cargo run -- -d .

# Recursive dry run in current directory
[group('run')]
recursive-dry:
    cargo run -- -r -d .

# Show cache status
[group('cache')]
cache-status:
    cargo run -- --cache-status

# Show cache statistics
[group('cache')]
cache-stats:
    cargo run -- --cache-stats

# Clear incomplete cache entries
[group('cache')]
cache-clear:
    cargo run -- --cache-clear-incomplete

# Run benchmarks (if benchmark script exists)
[group('test')]
bench:
    #!/usr/bin/env bash
    if [ -f tests/benchmark.sh ]; then
        bash tests/benchmark.sh
    else
        echo "No benchmark script found at tests/benchmark.sh"
    fi

# Run integration tests
[group('test')]
integration:
    #!/usr/bin/env bash
    if [ -f tests/integration_test.sh ]; then
        bash tests/integration_test.sh
    else
        echo "No integration test script found at tests/integration_test.sh"
    fi

# Update dependencies
[group('dev')]
update:
    cargo update

# Generate documentation
[group('dev')]
doc:
    cargo doc --open

# Watch for changes and rebuild
[group('dev')]
watch:
    cargo watch -x build

# Create a new release build and show size
[group('build')]
release: build-release
    @echo "Release binary size:"
    @ls -lh target/release/dds | awk '{print $5}'
