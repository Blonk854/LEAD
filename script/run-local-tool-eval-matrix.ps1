# Run tool-call smoke evals against every model listed in local-tool-eval-models.txt
#
# Prerequisites:
#   - LM Studio (or Ollama) server running with models loaded
#   - Models configured in settings under language_models
#   - cargo + cargo-nextest installed (https://rustup.rs, then: cargo install cargo-nextest --locked)
#
# Usage:
#   .\script\run-local-tool-eval-matrix.ps1
#   .\script\run-local-tool-eval-matrix.ps1 script\local-tool-eval-models.txt

param(
    [string]$ModelFile = ""
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)

if (-not $ModelFile) {
    $ModelFile = Join-Path $Root "script\local-tool-eval-models.txt"
}

if (-not (Test-Path $ModelFile)) {
    Write-Error "Model list not found: $ModelFile"
    exit 1
}

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Error @"
cargo not found on PATH.

Install Rust: https://rustup.rs
Then install nextest: cargo install cargo-nextest --locked
Restart your terminal so cargo is on PATH.
"@
    exit 1
}

if (-not (Get-Command cargo-nextest -ErrorAction SilentlyContinue)) {
    Write-Error @"
cargo-nextest not found on PATH.

Install with: cargo install cargo-nextest --locked
Restart your terminal after installing.
"@
    exit 1
}

try {
    $lmModels = Invoke-RestMethod -Uri "http://localhost:1234/api/v0/models" -TimeoutSec 5
    # LM Studio labels some chat models as "vlm" (vision-capable). Zed uses both.
    $lmChatModels = @($lmModels.data | Where-Object { $_.type -in @("llm", "vlm") })
    $lmChatIds = @($lmChatModels | ForEach-Object { $_.id })
    $loaded = @($lmChatModels | Where-Object { $_.state -eq "loaded" })
    if ($loaded.Count -eq 0) {
        Write-Warning "LM Studio is reachable but no chat model is loaded. Load a model before running evals."
    }
} catch {
    Write-Error @"
Cannot reach LM Studio at http://localhost:1234

Start LM Studio, load your model, and ensure the local server is running.
Error: $_
"@
    exit 1
}

$Models = Get-Content $ModelFile |
    Where-Object { $_ -notmatch '^\s*#' -and $_ -match '\S' } |
    ForEach-Object { $_.Trim() }

if ($Models.Count -eq 0) {
    Write-Error "No models in $ModelFile (use provider/model per line, e.g. lmstudio/qwopus-glm-18b-merged)"
    exit 1
}

Write-Host "Running tool-call smoke evals for $($Models.Count) model(s)"
Write-Host "Model list: $ModelFile"
Write-Host ""

$Passed = 0
$Failed = 0
$Results = @()

# Not chat/agent models — tool-call smoke eval does not apply.
$SkipModelIds = @(
    "neutts-air" # TTS model (outputs speech tokens, not tool calls)
)

Push-Location $Root
try {
    foreach ($model in $Models) {
        if ($model -notmatch '/') {
            Write-Warning "Model '$model' missing provider prefix; using lmstudio/$model"
            $model = "lmstudio/$model"
        }

        if ($model -match '^lmstudio/(.+)$') {
            $modelId = $Matches[1]
            if ($modelId -in $SkipModelIds) {
                Write-Warning "Skipping '$modelId' - not a chat/agent model. Remove from list or delete from SkipModelIds to force-run."
                $Results += "SKIP  $model"
                Write-Host ""
                continue
            }
            $canonicalId = $lmChatIds | Where-Object { $_ -ceq $modelId } | Select-Object -First 1
            if (-not $canonicalId) {
                $canonicalId = $lmChatIds | Where-Object {
                    $_.ToLower() -eq $modelId.ToLower()
                } | Select-Object -First 1
            }
            if (-not $canonicalId) {
                $hint = $lmChatIds | Where-Object {
                    $_ -like "*$($modelId.ToLower() -replace '[^a-z0-9]', '*')*"
                } | Select-Object -First 3
                $hintText = if ($hint) { "`n  Did you mean: $($hint -join ', ')?" } else { "" }
                Write-Error @"
LM Studio has no model with id '$modelId'.$hintText

List ids: http://localhost:1234/api/v0/models
Use the API 'id' field in local-tool-eval-models.txt (not the GGUF filename).
"@
            }
            if ($canonicalId -cne $modelId) {
                Write-Warning "Model id '$modelId' does not match LM Studio API casing; using '$canonicalId'."
                $modelId = $canonicalId
                $model = "lmstudio/$canonicalId"
            }
            $isLoaded = $loaded.id -contains $modelId
            if (-not $isLoaded) {
                Write-Host "Model '$modelId' is not loaded - warming up via LM Studio API (may take a minute)..."
                try {
                    $warmup = @{
                        model    = $modelId
                        messages = @(@{ role = "user"; content = "ok" })
                        stream   = $false
                        max_tokens = 1
                    } | ConvertTo-Json -Depth 5
                    Invoke-RestMethod -Uri "http://localhost:1234/api/v0/chat/completions" `
                        -Method Post -Body $warmup -ContentType "application/json" -TimeoutSec 180 | Out-Null
                    Write-Host "Warm-up complete."
                } catch {
                    Write-Error @"
Failed to load/warm up model '$modelId' in LM Studio.

Load the model in LM Studio first, then re-run the matrix.
Error: $_
"@
                }
            }
        }

        Write-Host "========================================"
        Write-Host "Model: $model"
        Write-Host "========================================"

        $env:ZED_AGENT_MODEL = $model
        $env:GPUI_TEST_TIMEOUT = "1500"

        & cargo nextest run -p agent --features unit-eval --no-capture -E "test(eval_tool_call_smoke)"
        if ($LASTEXITCODE -ne 0) {
            Write-Host ""
            Write-Host "Hint: re-run with full output to see per-case results:"
            Write-Host "  `$env:ZED_AGENT_MODEL='$model'; cargo test -p agent --features unit-eval eval_tool_call_smoke -- --nocapture"
            Write-Host ""
        }
        if ($LASTEXITCODE -eq 0) {
            $Passed++
            $Results += "PASS  $model"
        } else {
            $Failed++
            $Results += "FAIL  $model"
        }
        Write-Host ""
    }
} finally {
    Pop-Location
}

Write-Host "========================================"
Write-Host "Summary"
Write-Host "========================================"
$Results | ForEach-Object { Write-Host $_ }
Write-Host ""
Write-Host "Passed: $Passed  Failed: $Failed  Total: $($Models.Count)"

if ($Failed -gt 0) {
    exit 1
}
