# Repository Guidelines

## Project Structure & Module Organization
- `client/`: Rust CLI (`stitch`) and TUI. Sample config at `client/stitch.example.toml` (copy to `~/.config/stitch/config.toml`).
- `server/`: gRPC + webhook + Discord integration. Layout: `src/{adapters,service,utils}`; SQLx migrations in `server/migrations/`; Dockerfile for releases.
- `proto/`: Protobufs and codegen (`build.rs`) using tonic/prost. Primary service is `stitch.v1.StitchService`.
- `helm/`: Kubernetes chart for deploying the server.
- `justfile`: Common tasks for build/test/dev.

## Build, Test, and Development Commands
- `just build-server | build-client`: Release builds for each crate.
- `just run-server`: Run server in debug (reads `.env` or env vars).
- `just run-client`: Run CLI against `STITCH_SERVER` (default `http://127.0.0.1:50051`).
- `just test`: `cargo test --workspace`.
- `just check`: `cargo fmt --all` then `cargo clippy -- -D warnings`.
- `just protoc`: Regenerate gRPC stubs from `proto/proto/...`.
- `just migrate`: Apply SQLx migrations (requires `sqlx` CLI and `DATABASE_URL`).
- Docker: `just build-docker` (local) or `just build-docker-push` (GHCR tags).

## Coding Style & Naming Conventions
- Rustfmt defaults; keep tree `cargo fmt`-clean. Clippy must pass with no warnings.
- Naming: modules/files `snake_case`; types/traits `PascalCase`; constants `SCREAMING_SNAKE_CASE`.
- Errors/logging: prefer `anyhow::Result` (CLI) and `thiserror` (server) with `tracing` spans; avoid `unwrap()` in long‑running code.
- Protobuf: keep package `stitch.v1`; add RPCs to `StitchService` and regenerate via `just protoc`.

## Testing Guidelines
- Place unit tests next to code using `#[cfg(test)] mod tests { ... }` (e.g., `server/src/adapters/webhook.rs`).
- Write deterministic tests; avoid live network/DB. Use async tests with `#[tokio::test]` when needed.
- Run `just test` locally; add tests for bug fixes and new endpoints.

## Commit & Pull Request Guidelines
- Commit format: `type(scope): subject` (examples: `feat(webhook): add rate limiting`, `fix(cli): correct untrack flow`).
- PRs should include: clear description, linked issues (`Closes #123`), test notes, screenshots/logs for CLI or API behavior, and migration steps when schema changes.
- Pre-submit: `just check` and `just test` must pass; update Helm values/docs when config/envs change.

## Security & Configuration Tips
- Server config via env or `.env`: `PORT`, `DATABASE_URL`, `WEBHOOK_URL/SECRET`, `TWITCH_CLIENT_ID/SECRET`, `DISCORD_TOKEN`, `DISCORD_CHANNEL`, `TOKIO_CONSOLE_PORT`. Never commit secrets.
- Client: set `STITCH_SERVER` or edit `~/.config/stitch/config.toml`.

