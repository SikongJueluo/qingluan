# ── Qingluan ──

# Frontend
frontend-dev:
    cd apps/web && bun dev

frontend-build:
    cd apps/web && bun run build

frontend-test:
    cd apps/web && bun run test:unit:run

# Rust
daemon-dev:
    cargo run -p qingluan-daemon

# Backend + frontend in parallel (formerly `cargo-make dev`)
dev:
    (just daemon-watch) & (just frontend-dev) & wait

# Watch mode daemon rebuilds (requires cargo-watch, in devenv packages)
daemon-watch:
    cargo watch -w crates/qingluan-daemon -w crates/qingluan-protocol -w crates/qingluan-sandbox -x "run -p qingluan-daemon"

cli ARGS='':
    cargo run -p qingluan-cli -- {{ARGS}}

check:
    cargo check --workspace

# Tauri. Invoked from apps/desktop because the tauri CLI finds src-tauri
# by walking up from cwd (apps/web has no src-tauri ancestor); hooks
# cd into ../web via tauri.conf.json.
tauri-dev:
    cd apps/desktop && bunx --package @tauri-apps/cli tauri dev

# ── Quality Gate ──

# Full quality gate — Harness / CI entry point. Non-zero exit on failure.
quality: quality-rust quality-fe

# Build the frontend bundle so `tauri::generate_context!` (which requires
# frontendDist to exist) compiles during clippy/test of qingluan-desktop.
frontend-dist:
    cd apps/web && bun run build-only

# Rust quality checks (read-only, no file modification)
quality-rust: frontend-dist
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace --all-targets

# Frontend quality checks (read-only, no file modification)
quality-fe:
    cd apps/web && bun run quality

# Soft reports (informational, don't block)
quality-soft:
    cd apps/web && bun run quality:soft

# ── Dev-time fix commands (modify files — NOT for CI/Harness) ──

# Auto-fix all (Rust + frontend)
fix:
    cargo fmt --all
    cd apps/web && bun run lint
    cd apps/web && bun run format

# Auto-fix Rust only
fix-rust:
    cargo fmt --all
    cargo clippy --workspace --all-targets --fix --allow-dirty --allow-staged

# Auto-fix frontend only
fix-fe:
    cd apps/web && bun run lint
    cd apps/web && bun run format

# ── Audit (soft report) ──

# License and dependency audit (requires cargo-deny)
audit:
    cargo deny check

# Coverage report
coverage:
    cd apps/web && bun run test:coverage

# Full validation (backward compat)
test-all:
    cargo check --workspace
    cd apps/web && bun run type-check
    cd apps/web && bun run test:unit
