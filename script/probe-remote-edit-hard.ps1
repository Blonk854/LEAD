# Harder probe: many tools in context + multi-line edits with quotes/escapes,
# approximating a real LEAD session more closely than the simple probe.
param(
    [string]$BaseUrl = "http://192.168.145.248:1234",
    [string]$Model = ""
)

$ErrorActionPreference = "Stop"

if (-not $Model) {
    $models = (Invoke-RestMethod -Uri "$BaseUrl/api/v0/models" -TimeoutSec 10).data
    $loaded = $models | Where-Object { $_.state -eq 'loaded' -and $_.type -ne 'embeddings' }
    if (-not $loaded) { Write-Host "No non-embedding model loaded"; exit 1 }
    $Model = $loaded[0].id
}
Write-Host "Hard probe against: $Model`n"

function New-Tool($name, $desc, $props, $required) {
    @{ type = "function"; function = @{ name = $name; description = $desc; parameters = @{ type = "object"; properties = $props; required = $required } } }
}

$tools = @(
    (New-Tool "read_file" "Read a file from the project." @{ path = @{ type = "string" }; offset = @{ type = "integer" }; limit = @{ type = "integer" } } @("path")),
    (New-Tool "terminal" "Run a shell command." @{ command = @{ type = "string" }; cd = @{ type = "string" } } @("command", "cd")),
    (New-Tool "grep" "Search file contents with regex." @{ regex = @{ type = "string" }; include_pattern = @{ type = "string" } } @("regex")),
    (New-Tool "list_directory" "List a directory." @{ path = @{ type = "string" } } @("path")),
    (New-Tool "find_path" "Find files by glob." @{ glob = @{ type = "string" } } @("glob")),
    (New-Tool "write_file" "Create or overwrite a file with content." @{ path = @{ type = "string" }; content = @{ type = "string" } } @("path", "content")),
    (New-Tool "delete_path" "Delete a file or directory." @{ path = @{ type = "string" } } @("path")),
    (New-Tool "copy_path" "Copy a file." @{ source_path = @{ type = "string" }; destination_path = @{ type = "string" } } @("source_path", "destination_path")),
    (New-Tool "move_path" "Move/rename a file." @{ source_path = @{ type = "string" }; destination_path = @{ type = "string" } } @("source_path", "destination_path")),
    (New-Tool "create_directory" "Create a directory." @{ path = @{ type = "string" } } @("path")),
    (New-Tool "now" "Get current datetime." @{ timezone = @{ type = "string" } } @()),
    (New-Tool "fetch" "Fetch a URL." @{ url = @{ type = "string" } } @("url")),
    (New-Tool "thinking" "Record a thought." @{ content = @{ type = "string" } } @("content")),
    (New-Tool "web_search" "Search the web." @{ query = @{ type = "string" } } @("query"))
)

$editTool = @{
    type = "function"
    function = @{
        name = "edit_file"
        description = "This is a tool for applying edits to an existing file. Each edit finds old_text in the file and replaces it with new_text. old_text must match exactly including whitespace."
        parameters = @{
            type = "object"
            properties = @{
                path = @{ type = "string"; description = "Full path starting with a project root directory." }
                edits = @{
                    type = "array"
                    description = "List of edit operations applied sequentially."
                    items = @{
                        type = "object"
                        properties = @{
                            old_text = @{ type = "string"; description = "Exact existing text" }
                            new_text = @{ type = "string"; description = "Replacement text" }
                        }
                        required = @("old_text", "new_text")
                    }
                }
            }
            required = @("path", "edits")
        }
    }
}
$tools += $editTool

$fileSnippet = @'
fn spawn_worker(config: &Config) -> Result<Worker> {
    let name = format!("worker-{}", config.id);
    log::info!("spawning \"{}\" with retries={}", name, MAX_RETRIES);
    Worker::builder()
        .name(&name)
        .timeout(Duration::from_secs(30))
        .build()
}
'@

$userPrompt = @"
Here is the current content of LEAD/crates/agent/src/worker.rs:

``````rust
$fileSnippet
``````

You must call the edit_file tool to make BOTH of these changes in one call:
1. Change the timeout from 30 seconds to 60 seconds.
2. Change the log message from ``spawning "{}" with retries={}`` to ``starting worker "{}" (retries={})``.

Preserve exact indentation. Use the edit_file tool now.
"@

$systemPrompt = "You are LEAD, an expert coding agent. The project has one root directory named LEAD. Always use tools to act; never reply in prose when a tool applies. When editing files, old_text must match the file content exactly, including whitespace and escape sequences."

$body = @{
    model = $Model
    messages = @(
        @{ role = "system"; content = $systemPrompt },
        @{ role = "user"; content = $userPrompt }
    )
    tools = $tools
    tool_choice = "auto"
    temperature = 0.1
    max_tokens = 2000
    stream = $false
} | ConvertTo-Json -Depth 14

try {
    $resp = Invoke-RestMethod -Uri "$BaseUrl/v1/chat/completions" -Method Post -ContentType "application/json" -Body $body -TimeoutSec 240
} catch {
    Write-Host "REQUEST FAILED: $_"
    exit 1
}

$choice = $resp.choices[0]
$msg = $choice.message
Write-Host "finish_reason: $($choice.finish_reason)"
if ($msg.content) {
    Write-Host "text content: $($msg.content.Substring(0, [Math]::Min(400, $msg.content.Length)))"
}
if (-not $msg.tool_calls) {
    Write-Host "RESULT: FAIL - no tool call"
    exit 0
}
foreach ($tc in $msg.tool_calls) {
    Write-Host "tool: $($tc.function.name)"
    Write-Host "raw arguments:"
    Write-Host $tc.function.arguments
    try {
        $parsed = $tc.function.arguments | ConvertFrom-Json
        Write-Host "JSON parse: OK"
        if ($parsed.edits) {
            $i = 0
            foreach ($e in @($parsed.edits)) {
                $i++
                Write-Host "--- edit $i old_text ---"
                Write-Host $e.old_text
                Write-Host "--- edit $i new_text ---"
                Write-Host $e.new_text
                if ($fileSnippet.Contains($e.old_text)) {
                    Write-Host "old_text matches file content: YES"
                } else {
                    Write-Host "old_text matches file content: NO (would fail in LEAD with 'could not find old_text')"
                }
            }
        }
    } catch {
        Write-Host "JSON parse FAILED: $_"
    }
}
