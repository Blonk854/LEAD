# LEAD Local Agent Tuning Checklist

Quick reference for making LEAD's local agent more reliable on debugging tasks
(e.g. framework version mismatches, tool-calling workflows).

## 1. Confirm defaults

Bundled defaults in `assets/settings/default.json`:

- `agent.default_profile` → `local-agent`
- `agent.default_model.provider` → `lmstudio`
- `agent.default_model.model` → check LM Studio model id matches a loaded model
- `agent.network_agent.enabled` → `false` unless using hybrid mode

New threads use `default_profile` from agent settings.

## 2. Pick the right model

Local 14B models work for many tasks but miss subtle API/version breaks more often
than frontier models.

| Goal | Option |
| --- | --- |
| Stay fully local | Use a stronger coder model (32B+) in LM Studio |
| Best reliability | Enable **Network Agent** (strong model plans, local subagent executes) |
| Cloud | Configure OpenRouter / OpenAI / etc. as provider |

**Network Agent**: Agent panel → **Network** button → configure endpoint → toggle on.

## 3. Profile choice

- `local-agent` — default in LEAD; lean tool set for local workflows
- `write` — more IDE tools (`find_references`, `go_to_definition`, …)

Use `write` when debugging needs deeper code navigation.

## 4. Tool permissions (trusted projects)

Default `confirm` is safe but slows the agent. For trusted repos, consider in user
settings:

```json
"tool_permissions": {
  "default": "allow",
  "tools": {
    "terminal": { "default": "confirm" },
    "delete_path": { "default": "confirm" }
  }
}
```

Keep destructive git/shell patterns on confirm.

## 5. Project context

Add project-specific guidance so the agent doesn't rediscover architecture every session:

- `AGENTS.md` at repo root
- `.agents/skills/<name>/SKILL.md` for repeatable playbooks

LEAD also persists cross-session memory under `.local_agent/` in each worktree:

- `JOURNAL.md` — durable notes (`append_to_journal` / `read_journal` tools)
- `project_summary.md` — latest compaction or thread-rollover summary, injected into new threads
- `rag.db` — local document index (`rag_ingest` / `rag_search`, uses LM Studio embeddings)

Flap GPT example skill (sibling project): `flet-debugging` for Flet 0.80+ API drift.

## 6. Validate tool calling

Before trusting a local model on complex agent work:

- Use the agent panel **Test tool calling** button (hammer icon in the composer), or
- Run `script/run-local-tool-eval-matrix.ps1` from the LEAD repo

Weak tool-calling models benefit most from Network Agent.

## 7. Context compaction (local models)

When using LM Studio models, LEAD proactively compacts in-thread context as the window fills
(not only on thread rollover). Summaries are saved to `.local_agent/project_summary.md` for the
next thread.

## 8. Hybrid mode checklist (Network Agent + local worker)

Hybrid mode uses a **network model as orchestrator** and your **local LM Studio model as worker** via `spawn_agent`.

1. Configure the network endpoint (Agent panel → **Network** → Configure Network Agent)
2. Set `agent.default_model` to your LM Studio worker model (required — this is **not** the orchestrator)
3. Use profile **`hybrid`** or **`write`** (must include `spawn_agent`)
4. Optional: set `agent.thread_summary_model` to the same local model (auto-set on first network save)
5. Enable the **Network Agent** toggle
6. Open **Network** popover and confirm orchestrator + local worker status
7. Ask the agent to delegate a terminal command; confirm a **Local worker** subagent card appears

Run `script/test-hybrid-agent.ps1` after building for a guided smoke test.

### Hybrid troubleshooting

| Symptom | Fix |
| --- | --- |
| Local model never runs | Network is calling tools directly — use balanced delegation (default) and `spawn_agent` |
| `spawn_agent` errors | Start LM Studio; verify `default_model` matches the loaded model name |
| Wrong model in picker | Orchestrator vs worker split — picker updates **local worker** only when Network Agent is on |
| Compaction slow/wrong | Set `thread_summary_model` to your local LM Studio model |

Balanced delegation (`agent.network_agent.delegation_mode: "balanced"`, default) hides execution tools from the orchestrator so the network model must delegate heavy work.

## 9. Handoff prompt for a new chat

When switching workspaces, paste:

```text
Continue LEAD tuning:
- Default profile: local-agent
- Goal: improve debugging reliability on Flap GPT / Flet issues
- See docs/local-agent-tuning-checklist.md
- Flap GPT has .agents/skills/flet-debugging/ and AGENTS.md
```
