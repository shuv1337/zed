# HANDOFF

## Objective
- Continue implementing `PLAN-openai-codex-oauth.md`: add a separate OpenAI Codex / ChatGPT OAuth language model provider for Zed Agent while preserving the existing OpenAI API-key provider.

## Current status
- A first compileable vertical slice was implemented: new provider file, settings schema, provider registration, OAuth browser flow scaffold, credential persistence, Codex model list, request sending, and shared Responses SSE parser extraction.
- Existing OpenAI provider remains separate; only shared `open_ai::responses::Request` fields/parser were touched.
- The implementation is not yet a complete/production-ready fulfillment of the plan: OAuth helpers were not lifted from `context_server`, refresh dedupe was simplified/disabled, request conversion only sets a default `instructions` string rather than moving system messages, UI is minimal, and tests were not added.

## Key context
- Provider ID/name: `openai_codex` / `OpenAI Codex`.
- Credential key: `https://chatgpt.com/backend-api/codex/oauth`.
- OAuth constants in new provider: client ID `app_EMoamEEZ73f0CkXaXp7hrann`, redirect `http://localhost:1455/auth/callback`, originator `zed`.
- `./script/clippy --all-targets` hit unrelated/pre-existing `open_router` GPUI test-support errors, so validation used library-target clippy.
- Follow repo rules in `AGENTS.md`; use `./script/clippy` normally, avoid `unwrap()`, and keep errors visible.

## Important files
- `PLAN-openai-codex-oauth.md` — source plan; currently untracked in git status.
- `crates/language_models/src/provider/open_ai_codex.rs` — new provider implementation; currently untracked.
- `crates/language_models/src/language_models.rs` — registers the provider.
- `crates/language_models/src/settings.rs` and `crates/settings_content/src/language_model.rs` — add `openai_codex` settings.
- `crates/open_ai/src/responses.rs` — added `parse_responses_sse_stream`, optional `instructions`, optional `store`.
- `crates/open_ai/src/completion.rs` — updated existing OpenAI Responses request construction for new optional fields.
- `crates/language_models/Cargo.toml` / `Cargo.lock` — added OAuth-related dependencies.

## Changed artifacts
- Modified: `Cargo.lock`, `crates/language_models/Cargo.toml`, `crates/language_models/src/language_models.rs`, `crates/language_models/src/provider.rs`, `crates/language_models/src/settings.rs`, `crates/open_ai/src/completion.rs`, `crates/open_ai/src/responses.rs`, `crates/settings_content/src/language_model.rs`.
- Added/untracked: `crates/language_models/src/provider/open_ai_codex.rs`, `PLAN-openai-codex-oauth.md`.

## Validation
- Passed: `cargo check -p language_models -p open_ai`.
- Passed: `cargo clippy -p language_models -p open_ai -p settings_content --lib -- --deny warnings`.
- Not completed: full plan test suite, real OAuth login, real Codex request, Cloudflare check, all-target clippy.

## Next steps
1. Inspect `git diff` and harden the new provider before adding more functionality.
2. Properly implement plan gaps: shared OAuth helper lift/reuse, fixed-port cancel/retry, refresh stampede dedupe, system-message-to-`instructions` conversion, 401 refresh retry, richer config UI, and unit tests.
3. Run targeted validations again; use `./script/clippy` where possible and document any pre-existing all-target blockers.

## Risks / open questions
- Current OAuth helper code duplicates logic instead of reusing/lifting `context_server::oauth`, contrary to plan.
- Refresh is not wired into streaming dispatch after simplifying to get compilation passing.
- `instructions` currently defaults to a generic string and does not preserve system messages.
- No real backend validation yet; Cloudflare/cookie requirements are unknown.
- Need decide whether `PLAN-openai-codex-oauth.md` should be tracked or left local.

## Resume prompt
- Pick up by reviewing the current diff, then close the plan gaps in `open_ai_codex.rs` while keeping `cargo check -p language_models -p open_ai` green after each major step.
