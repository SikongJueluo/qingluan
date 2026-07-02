# ── Qingluan ──

# Frontend
frontend-dev:
    cd apps/desktop/frontend && bun dev

frontend-build:
    cd apps/desktop/frontend && bun run build

frontend-test:
    cd apps/desktop/frontend && bun run test:unit

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

# Full validation
test-all:
    cargo check --workspace
    cd apps/desktop/frontend && bun run type-check
    cd apps/desktop/frontend && bun run test:unit
