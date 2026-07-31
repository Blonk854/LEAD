# Unleashed (guard-railed full PC access)

LEAD can expose native tools that operate beyond the open project:

- `run_code`: run Python, Node.js, PowerShell, or Bash source through an installed interpreter.
- `process_control`: inspect processes/system information, terminate a process, or list/start/stop Windows services.
- `http_request`: send generic HTTP/HTTPS requests.
- `computer_use`: capture the primary monitor and control the mouse or keyboard.
- Existing terminal and filesystem tools accept authorized absolute paths outside project worktrees.

## Enable it

Unleashed is off by default. Prefer **Settings → AI → Unleashed**:

1. Turn on **Enable Unleashed**.
2. Open **Unleashed Setup** to edit allowed/denied roots and Unleashed tool permissions.
3. Select the **Unleashed** agent profile (or **Hybrid**).

Or add this to user settings:

```json
{
  "agent": {
    "full_access": {
      "enabled": true,
      "allowed_roots": [],
      "denied_roots": []
    }
  }
}
```

The elevated tools are removed from model tool lists while `enabled` is false. Terminal and filesystem tools keep their normal project-only behavior.

## Unleashed settings UI

Under **Settings → AI → Unleashed**:

| Control | Setting |
| --- | --- |
| Enable Unleashed | `agent.full_access.enabled` |
| Allowed roots | `agent.full_access.allowed_roots` |
| Denied roots | `agent.full_access.denied_roots` |
| Run Code / HTTP Request / Computer Use / Process Control permissions | `agent.tool_permissions.tools.<name>` |

General tool permissions (terminal, edit file, and so on) stay under **Settings → AI → Tool Permissions**.

## Authorization policy

Paths in open project worktrees are in trusted scope. Absolute paths outside those worktrees trigger LEAD's existing permission dialog before the operation runs.

`allowed_roots` adds trusted absolute directory subtrees. Use it only for directories where unattended agent actions are acceptable:

```json
"allowed_roots": [
  "C:\\Users\\me\\AgentScratch"
]
```

`denied_roots` adds absolute directory subtrees that Unleashed tools must never touch:

```json
"denied_roots": [
  "D:\\Backups",
  "C:\\Users\\me\\.ssh"
]
```

User-denied roots and LEAD's built-in protected paths take precedence over allowed roots and permission rules.

Process termination and every screenshot/mouse/keyboard action always prompt, even when the tool default is Allow. Setting those tools to Deny blocks them. State-changing HTTP methods are authorized by URL; GET is treated as read-only. `run_code` and terminal calls still pass through normal tool permissions because source code and commands can access resources not evident from their working directory.

## Unbypassable protections

Unleashed filesystem tools reject drive roots and protected operating-system trees such as Windows, Program Files, and ProgramData. Terminal permission checks also reject drive formatting, partition-management commands, and recursive deletion commands aimed at protected roots.

These checks are built into LEAD and cannot be disabled through settings. Symlinked parents are canonicalized before path policy is applied.

## Operational notes

- Prefer a dedicated scratch directory in `allowed_roots` instead of a broad user-profile root.
- Keep normal project work in the Write or Local Agent profile.
- GUI automation depends on operating-system accessibility/input permissions and may not work on secure desktops.
- `run_code` requires the selected interpreter to be available on `PATH`.
- External `edit_file` operations require each `old_text` match to be exact and unique.
- External searches do not follow directory symlinks, skip unreadable/binary files, and cap individual text files at 2 MiB.
