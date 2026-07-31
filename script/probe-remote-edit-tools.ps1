# Probe a remote LM Studio server for edit_file / write_file tool-call quality.
# Usage: .\script\probe-remote-edit-tools.ps1 [-BaseUrl http://192.168.145.248:1234] [-Model <id>]
param(
    [string]$BaseUrl = "http://192.168.145.248:1234",
    [string]$Model = ""
)

$ErrorActionPreference = "Stop"

if (-not $Model) {
    $models = (Invoke-RestMethod -Uri "$BaseUrl/api/v0/models" -TimeoutSec 10).data
    $loaded = $models | Where-Object { $_.state -eq 'loaded' -and $_.type -ne 'embeddings' }
    if (-not $loaded) { Write-Host "No non-embedding model loaded on $BaseUrl"; exit 1 }
    $Model = $loaded[0].id
}
Write-Host "Probing model: $Model on $BaseUrl`n"

$editTool = @{
    type = "function"
    function = @{
        name = "edit_file"
        description = "Edit a file by replacing old_text with new_text. Each edit finds old_text in the file and replaces it with new_text."
        parameters = @{
            type = "object"
            properties = @{
                path = @{ type = "string"; description = "The full path of the file to edit in the project. Must start with a project root directory." }
                edits = @{
                    type = "array"
                    description = "List of edit operations to apply sequentially."
                    items = @{
                        type = "object"
                        properties = @{
                            old_text = @{ type = "string"; description = "Exact text to find" }
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

$writeTool = @{
    type = "function"
    function = @{
        name = "write_file"
        description = "Create or overwrite a file with the given content."
        parameters = @{
            type = "object"
            properties = @{
                path = @{ type = "string"; description = "The full path of the file to create or overwrite in the project." }
                content = @{ type = "string"; description = "The entire content for the file." }
            }
            required = @("path", "content")
        }
    }
}

$cases = @(
    @{
        name = "write_file_simple"
        prompt = "You must call the write_file tool. Create a file `lead/hello.txt` containing exactly the text: Hello from LEAD"
        tools = @($writeTool)
    },
    @{
        name = "edit_file_single"
        prompt = "You must call the edit_file tool. In the file `lead/src/main.rs`, replace the text `let x = 1;` with `let x = 2;`. One edit only."
        tools = @($editTool)
    },
    @{
        name = "edit_file_multi"
        prompt = "You must call the edit_file tool. In `lead/config.toml`, make two edits: (1) replace `debug = true` with `debug = false`, and (2) replace `port = 8080` with `port = 9090`."
        tools = @($editTool)
    }
)

$passCount = 0
foreach ($case in $cases) {
    Write-Host "=== Case: $($case.name) ==="
    $body = @{
        model = $Model
        messages = @(
            @{ role = "system"; content = "You are a coding agent. Use the provided tools. Reply with a tool call, not prose." },
            @{ role = "user"; content = $case.prompt }
        )
        tools = $case.tools
        tool_choice = "auto"
        temperature = 0.1
        max_tokens = 1500
        stream = $false
    } | ConvertTo-Json -Depth 12

    try {
        $resp = Invoke-RestMethod -Uri "$BaseUrl/v1/chat/completions" -Method Post -ContentType "application/json" -Body $body -TimeoutSec 180
    } catch {
        Write-Host "  REQUEST FAILED: $_"
        continue
    }

    $choice = $resp.choices[0]
    $msg = $choice.message
    Write-Host "  finish_reason: $($choice.finish_reason)"

    if ($msg.tool_calls) {
        foreach ($tc in $msg.tool_calls) {
            Write-Host "  tool call: $($tc.function.name)"
            Write-Host "  raw arguments: $($tc.function.arguments)"
            try {
                $parsed = $tc.function.arguments | ConvertFrom-Json
                Write-Host "  JSON parse: OK"
                if ($tc.function.name -eq "edit_file") {
                    if ($parsed.edits -is [array] -or $parsed.edits) {
                        $editsJson = $parsed.edits | ConvertTo-Json -Depth 5 -Compress
                        Write-Host "  edits field: $editsJson"
                        $firstEdit = @($parsed.edits)[0]
                        if ($firstEdit.old_text -and $null -ne $firstEdit.new_text) {
                            Write-Host "  RESULT: PASS"
                            $passCount++
                        } else {
                            Write-Host "  RESULT: FAIL (edits items missing old_text/new_text)"
                        }
                    } else {
                        Write-Host "  RESULT: FAIL (no edits array; keys: $($parsed.PSObject.Properties.Name -join ', '))"
                    }
                } elseif ($tc.function.name -eq "write_file") {
                    if ($parsed.path -and $null -ne $parsed.content) {
                        Write-Host "  RESULT: PASS"
                        $passCount++
                    } else {
                        Write-Host "  RESULT: FAIL (missing path/content; keys: $($parsed.PSObject.Properties.Name -join ', '))"
                    }
                }
            } catch {
                Write-Host "  JSON parse: FAILED - $_"
                Write-Host "  RESULT: FAIL (unparseable arguments)"
            }
        }
    } else {
        $preview = if ($msg.content) { $msg.content.Substring(0, [Math]::Min(600, $msg.content.Length)) } else { "(empty)" }
        Write-Host "  NO TOOL CALL. Content preview:"
        Write-Host "  $preview"
        Write-Host "  RESULT: FAIL (no tool call emitted)"
    }
    Write-Host ""
}

Write-Host "Passed $passCount of $($cases.Count) cases."
