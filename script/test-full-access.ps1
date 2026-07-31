param(
    [string]$ScratchRoot = (Join-Path $env:TEMP "lead-full-access-smoke")
)

$ErrorActionPreference = "Stop"

New-Item -ItemType Directory -Force -Path $ScratchRoot | Out-Null
$sampleFile = Join-Path $ScratchRoot "sample.txt"
Set-Content -Path $sampleFile -Value "LEAD full-access smoke test"

Write-Host "Prepared safe smoke-test directory:"
Write-Host "  $ScratchRoot"
Write-Host ""
Write-Host "Before testing, enable agent.full_access.enabled and select the Full Access profile."
Write-Host "Use a new LEAD thread and run these checks:"
Write-Host ""
Write-Host "1. read_file path '$sampleFile'"
Write-Host "   Expected: an outside-project authorization prompt, then file contents."
Write-Host "2. run_code language 'python' with: print('lead-run-code-ok')"
Write-Host "   Expected: authorization prompt and lead-run-code-ok output."
Write-Host "3. process_control action 'list', name_filter 'lead'"
Write-Host "   Expected: process rows without terminating anything."
Write-Host "4. http_request method 'GET', url 'https://example.com'"
Write-Host "   Expected: HTTP status, headers, and response body."
Write-Host "5. computer_use action 'screenshot'"
Write-Host "   Expected: an always-confirm prompt and an image result."
Write-Host "6. create_directory under '$ScratchRoot', then write/edit/copy/move/delete it."
Write-Host "   Expected: outside-project prompts and successful filesystem operations."
Write-Host ""
Write-Host "Negative checks (must be rejected without an approval option):"
Write-Host "  delete_path C:\Windows"
Write-Host "  terminal command: format C:"
Write-Host ""
Write-Host "Disable agent.full_access.enabled and start another thread."
Write-Host "Expected: run_code, process_control, http_request, and computer_use are absent;"
Write-Host "outside-project terminal/filesystem calls return to project-only behavior."
