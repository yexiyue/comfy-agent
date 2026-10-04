# Repository Guidelines

## Project Structure & Module Organization

This Rust 2024 project is a virtual Cargo workspace. Shared dependencies and lints live in the root `Cargo.toml`.

- `crates/agent/`: agent loop, streaming events, model configuration, and tool registry.
- `crates/tools/`: `AgentTool` interface and exported `#[agent_tool]` attribute.
- `crates/tool-macros/`: procedural macro implementation.
- `crates/server/`: `routes/` uses utoipa-axum to register endpoints and collect OpenAPI; `api.rs` defines public DTOs; `views.rs` projects domain records; `stream.rs` owns SSE replay.
- `crates/runtime/`: domain state and storage ports; `execution.rs` owns attempt lifecycle, `execution/driver/` handles model/tool phases; no HTTP/ORM dependency.
- `crates/persistence/`: Toasty semantic transactions in `repository.rs`, private SQL/codec helpers in `repository/records.rs`, explicit migrations, Apalis queue/outbox and recovery.
- `crates/telemetry/`: OpenInference spans, content policy, bounded OTLP export, and terminal outcomes. Hosts initialize exporters; the agent core does not.
- `crates/*/tests/`: integration tests for agent behavior and generated tools.
- `workflows/`: ComfyUI frontend and API JSON examples.
- `scripts/`: PowerShell ComfyUI smoke test; `docs/`: explanatory articles.
- `apps/web/`: chat frontend — pnpm + Vite + React 19 + TypeScript, Tailwind v4, shadcn/ui, AI Elements (registry components live in `src/components/`), AI SDK v7. Consumes `POST /api/chat` from `crates/server`.

## Build, Test, and Development Commands

Run from the repository root:

```sh
cargo build --workspace
cargo check --workspace
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

These build all crates, check compilation, run tests, verify formatting, and reject lint warnings. Use `cargo fmt --all` to apply formatting. Start `compose.postgres.yaml` and run `cargo run -p persistence --bin migrate` explicitly before running the HTTP backend with `cargo run -p server` (default `127.0.0.1:3001`).

For a running local ComfyUI instance, use:

```powershell
./scripts/comfy-smoke.ps1 -WorkflowPath workflows/sdxlturbo.api.json
```

This submits a generation job and verifies its output; it requires compatible installed models.

Frontend commands (require Node 22.18+ and pnpm 10+), run from the repository root:

```sh
pnpm -C apps/web install
pnpm -C apps/web dev     # dev server on http://localhost:5173, expects backend on :3001
pnpm -C apps/web build   # type-check (tsc -b) + production build
pnpm -C apps/web api:generate # offline Rust OpenAPI export + Hey API client/query generation
pnpm -C apps/web api:check    # verify checked-in artifacts without modifying them
pnpm -C apps/web test         # session invariants and React/query integration tests
```

Backend address comes from `VITE_API_BASE` in `apps/web/.env` (default `http://localhost:3001`). Frontend tool rendering is declared in `apps/web/src/lib/tools.tsx`; update it when the backend registers new tools.

Ordinary HTTP requests use `src/api/generated` and TanStack Query; AI SDK owns chat SSE. Never edit generated clients or `docs/api/openapi.json` manually. Update Rust DTOs/path annotations, regenerate, and check the contract. Keep TypeScript at the generator-supported 5.9.3 until its compiler API supports newer versions.

API address and client initialization live in `src/api/client.ts`. Vite and Vitest share `vite.config.ts`. Install only registry components and dependencies used by the application; preserve transitive component imports when pruning.

## Coding Style & Naming Conventions

Follow rustfmt defaults: four-space indentation, `snake_case` functions/modules, `PascalCase` types, and `SCREAMING_SNAKE_CASE` constants. Keep reusable dependencies in `[workspace.dependencies]` and inherit workspace lints; unsafe code is forbidden. Preserve the single Agent phase machine. Acquire conversation locks before run locks, fence durable writes by live lease/generation, and commit decisions/results before external actions. Tools default to conservative recovery; declare safe/idempotent/reconcilable policies explicitly. A resumable external operation must be reconciled before steering; safe resumption does not imply safe abandonment. Fence frontend asynchronous results by conversation selection lifetime and task identity. Keep terminal output outside the agent core and expose progress through events. Use Mermaid for useful documentation diagrams.

## Testing Guidelines

Use Rust tests and `#[tokio::test]` for asynchronous behavior. Name tests descriptively, such as `tool_roundtrip_preserves_history_and_emits_events`. Tests use local mock models without production credentials. Cover history, step limits, tool errors, serialization, SSE boundaries, and cancellation. No numeric coverage threshold is configured. Run `cargo test -p server` for credential-free protocol tests. PostgreSQL tests are ignored by default: set a dedicated `_test` `TEST_DATABASE_URL`, then run `cargo test -p persistence -p server -- --ignored --test-threads=1`. Mock eval/server fixtures create their own child databases. Run `node --experimental-strip-types scripts/durable-check/check.mjs` for actual process restart and official replay validation, and `pnpm -C apps/web test` for frontend submission/replay invariants. Validate with the official AI SDK parser using `npm ci --prefix scripts/ai-sdk-check` and `npm run check:mock --prefix scripts/ai-sdk-check`, then run the workspace suite.

## Commit & Pull Request Guidelines

For observability changes, run `cargo build -p server`, `npm ci --prefix scripts/evals`, `npm run check --prefix scripts/evals`, and `npm test --prefix scripts/evals`. Run `npm run eval:mock --prefix scripts/evals -- --phoenix` against local Phoenix for persisted experiment/trace validation. Real evaluations require explicit `--real`; see `docs/observability.md`. Missing usage stays unknown, `finished` is not task quality, and subscription fees are not token costs.

Recent commits use `feat: <summary>`, with Chinese summaries. Follow that prefix style and keep commits focused. PRs should explain the behavior change, link relevant issues, list validation commands/results, and update affected documentation. Include workflow output evidence when changing ComfyUI integration.

## Security & Configuration

Use `.env.example` as the configuration reference. Server loads optional `.env`; the core reads process variables only. Keep model prefixes, endpoints, and keys compatible. Never commit credentials or generated outputs. The backend is for local development and has no authentication.

Server settings are parsed with envy and injected into infrastructure. Configure Toasty and Apalis pool sizes together; each process consumes both connection budgets. Pool waits are bounded. API DTOs must not expose model history, checkpoint, lease, or configuration internals.
