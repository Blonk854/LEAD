# Changelog

All notable changes to LEAD are documented here. LEAD's version is defined in
`crates/zed/Cargo.toml`. Detailed per-release notes live in
[`docs/src/releases/`](docs/src/releases/).

## [1.8.1] - 2026-07-17 (dev)

### Fixed

- **LM Studio tool calls with `<` in arguments were rejected.** Valid native
  `write_file` / `edit_file` calls whose arguments contained `<` (JSX, HTML,
  generics) were misrouted through XML tool-call repair and failed with
  "failed to parse embedded XML tool call". Well-formed calls are now accepted
  before any repair heuristics run.

### Changed

- The tool-call smoke test ("hammer" and eval matrix) now includes `write_file`
  and `edit_file` cases with JSX content, and offers each case only its
  expected tools so the result reflects call formatting rather than tool choice.
- GPUI test-scheduler parking timeout is configurable via `GPUI_TEST_TIMEOUT`
  (seconds) for evals that wait on slow local models.

## [1.8.0] - 2026-07-11 (dev)

Full notes: [`docs/src/releases/1.8.0.md`](docs/src/releases/1.8.0.md).

### Added

- **Guard-railed Unleashed access** (off by default via `agent.full_access.enabled`):
  native `run_code`, `process_control`, `http_request`, and `computer_use` tools,
  plus authorized absolute-path access for existing terminal and filesystem tools.
  Uses a confirm-on-escape permission model with `allowed_roots` / `denied_roots`
  and an unbypassable catastrophic-path denylist. See
  [`docs/full-access.md`](docs/full-access.md).
- **Hybrid Network Agent + local worker**: a network model orchestrates while the
  local LM Studio model executes delegated work via `spawn_agent`, with balanced
  delegation by default. See
  [`docs/local-agent-tuning-checklist.md`](docs/local-agent-tuning-checklist.md).
- **Auto thread rollover and hand-off** (`agent.auto_thread_rollover`): long
  conversations roll into a fresh thread seeded with a hand-off summary, and an
  active `/goal` auto-resumes.
- **Local-agent memory and retrieval**: per-worktree `.local_agent/` with
  `JOURNAL.md` (`append_to_journal` / `read_journal`), `project_summary.md`, and a
  `rag.db` document index (`rag_ingest` / `rag_search`).
- New agent profiles **Unleashed** and **Hybrid**.
- **Unleashed settings UI** under Settings → AI → Unleashed for enabling whole-PC
  access, editing allowed/denied roots, and configuring Unleashed tool
  permissions (`run_code`, `http_request`, `computer_use`, `process_control`).

### Changed

- Local-model sessions proactively compact in-thread context as the window fills,
  not only on thread rollover.
- Windows terminal permission checks now block drive formatting, partition
  management, and recursive deletion aimed at protected roots.

### Notes

- Windows installer output is `target/LEAD-1.8.0-x86_64.exe`; the standalone
  binary is `target/x86_64-pc-windows-msvc/release/lead.exe`.
