# Probe remote LM Studio streaming tool-call behavior for edit_file (mirrors LEAD's streaming path).
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
Write-Host "Streaming probe against: $Model`n"

# Mirror LEAD's real edit_file schema description (long doc comments + path warning).
$pathDescription = @"
The full path of the file to edit in the project.

WARNING: When specifying which file path need changing, you MUST start each path with one of the project's root directories, unless it's a global agent skill under ``~/.agents/skills``.

The following examples assume we have two root directories in the project:
- /a/b/backend
- /c/d/frontend

<example>
``backend/src/main.rs``

Notice how the file path starts with ``backend``. Without that, the path would be ambiguous and the call would fail!
</example>

<example>
``frontend/db.js``
</example>
"@

$editTool = @{
    type = "function"
    function = @{
        name = "edit_file"
        description = "This is a tool for editing files by finding and replacing text."
        parameters = @{
            type = "object"
            properties = @{
                path = @{ type = "string"; description = $pathDescription }
                edits = @{
                    type = "array"
                    description = "List of edit operations to apply sequentially. Each edit finds old_text in the file and replaces it with new_text."
                    items = @{
                        type = "object"
                        properties = @{
                            old_text = @{ type = "string" }
                            new_text = @{ type = "string" }
                        }
                        required = @("old_text", "new_text")
                    }
                }
            }
            required = @("path", "edits")
        }
    }
}

$body = @{
    model = $Model
    messages = @(
        @{ role = "system"; content = "You are LEAD, a coding agent. The project has one root directory: LEAD. Use tools; do not reply in prose." },
        @{ role = "user"; content = "You must call the edit_file tool. In the file LEAD/crates/agent/src/thread.rs, replace ``const MAX_RETRIES: usize = 3;`` with ``const MAX_RETRIES: usize = 5;``." }
    )
    tools = @($editTool)
    tool_choice = "auto"
    temperature = 0.1
    max_tokens = 1500
    stream = $true
} | ConvertTo-Json -Depth 12

$req = [System.Net.HttpWebRequest]::Create("$BaseUrl/v1/chat/completions")
$req.Method = "POST"
$req.ContentType = "application/json"
$req.Timeout = 180000
$req.ReadWriteTimeout = 180000
$bytes = [System.Text.Encoding]::UTF8.GetBytes($body)
$reqStream = $req.GetRequestStream()
$reqStream.Write($bytes, 0, $bytes.Length)
$reqStream.Close()

$resp = $req.GetResponse()
$reader = New-Object System.IO.StreamReader($resp.GetResponseStream())

$toolName = ""
$argFragments = New-Object System.Collections.Generic.List[string]
$textContent = ""
$chunkCount = 0
$finishReason = ""

while (-not $reader.EndOfStream) {
    $line = $reader.ReadLine()
    if (-not $line.StartsWith("data: ")) { continue }
    $payload = $line.Substring(6)
    if ($payload -eq "[DONE]") { break }
    $chunk = $payload | ConvertFrom-Json
    $delta = $chunk.choices[0].delta
    if ($chunk.choices[0].finish_reason) { $finishReason = $chunk.choices[0].finish_reason }
    if ($delta.content) { $textContent += $delta.content }
    if ($delta.tool_calls) {
        foreach ($tc in $delta.tool_calls) {
            $chunkCount++
            if ($tc.function.name) { $toolName += $tc.function.name }
            if ($null -ne $tc.function.arguments) { $argFragments.Add($tc.function.arguments) }
        }
    }
}
$reader.Close()
$resp.Close()

$fullArgs = $argFragments -join ""
Write-Host "finish_reason: $finishReason"
Write-Host "tool-call delta chunks: $chunkCount"
Write-Host "accumulated tool name: '$toolName'"
Write-Host "accumulated arguments:"
Write-Host $fullArgs
if ($textContent) {
    Write-Host "text content streamed alongside:"
    Write-Host $textContent.Substring(0, [Math]::Min(600, $textContent.Length))
}
try {
    $parsed = $fullArgs | ConvertFrom-Json
    Write-Host "JSON parse of accumulated arguments: OK"
    Write-Host "path: $($parsed.path)"
    Write-Host "edits type: $($parsed.edits.GetType().Name)"
    Write-Host ("edits: " + ($parsed.edits | ConvertTo-Json -Depth 5 -Compress))
} catch {
    Write-Host "JSON parse of accumulated arguments FAILED: $_"
}
