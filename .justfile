set shell := ["pwsh", "-c"]

# ── Qingluan ──

# Frontend
frontend-dev:
    cd apps/desktop/frontend && bun dev

frontend-build:
    cd apps/desktop/frontend && bun run build

frontend-test:
    cd apps/desktop/frontend && bun run test:unit:run

# Rust
daemon-dev:
    cargo run -p qingluan-daemon

cli ARGS='':
    cargo run -p qingluan-cli -- {{ARGS}}

check:
    cargo check --workspace

# Tauri
tauri-dev:
    cd apps/desktop/frontend && bun tauri dev

# ── Quality Gate ──

# Full quality gate — Harness / CI entry point. Non-zero exit on failure.
quality: quality-rust quality-fe

# Rust quality checks (read-only, no file modification)
quality-rust:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace --all-targets

# Frontend quality checks (read-only, no file modification)
quality-fe:
    cd apps/desktop/frontend && bun run quality

# Soft reports (informational, don't block)
quality-soft:
    cd apps/desktop/frontend && bun run quality:soft

# ── Dev-time fix commands (modify files — NOT for CI/Harness) ──

# Auto-fix all (Rust + frontend)
fix:
    cargo fmt --all
    cd apps/desktop/frontend && bun run lint
    cd apps/desktop/frontend && bun run format

# Auto-fix Rust only
fix-rust:
    cargo fmt --all
    cargo clippy --workspace --all-targets --fix --allow-dirty --allow-staged

# Auto-fix frontend only
fix-fe:
    cd apps/desktop/frontend && bun run lint
    cd apps/desktop/frontend && bun run format

# ── Audit (soft report) ──

# License and dependency audit (requires cargo-deny)
audit:
    cargo deny check

# Coverage report
coverage:
    cd apps/desktop/frontend && bun run test:coverage

# Full validation (backward compat)
test-all:
    cargo check --workspace
    cd apps/desktop/frontend && bun run type-check
    cd apps/desktop/frontend && bun run test:unit
