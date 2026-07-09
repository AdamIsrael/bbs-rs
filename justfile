# sshtui developer tasks. Run `just` (or `just --list`) to see all recipes.

# Runtime artifacts created on first run (see Config defaults / .gitignore).
db := "bbs.db"
host_key := "host_key"

# Show available recipes.
default:
    @just --list

# Run the BBS server (SSH on :2222; `ssh guest@localhost -p 2222`, password 'guest').
run *ARGS:
    cargo run -- {{ARGS}}

# Build the debug binary.
build:
    cargo build

# Build the optimized release binary.
release:
    cargo build --release

# Type-check without producing a binary.
check:
    cargo check --all-targets

# Run the test suite (input-parser unit tests + service integration tests).
test:
    cargo test

# Format the source.
fmt:
    cargo fmt

# Lint: clippy (warnings as errors) + formatting check.
lint:
    cargo clippy --all-targets -- -D warnings
    cargo fmt --check

# Delete the SQLite database; it is recreated, migrated, and re-seeded on next run.
reset-db:
    rm -f {{db}} {{db}}-journal {{db}}-wal {{db}}-shm

# Delete the database and the SSH host key (a fresh key is generated on next run).
reset-all: reset-db
    rm -f {{host_key}} {{host_key}}.pub

# Format, lint, and test — a pre-commit sanity sweep.
ci: fmt lint test
