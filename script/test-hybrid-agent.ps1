# Manual smoke script for hybrid Network + Local Agent mode.
# Run from the LEAD repo root after building LEAD.

$ErrorActionPreference = "Stop"

Write-Host "=== LEAD Hybrid Agent Smoke Test ===" -ForegroundColor Cyan

$leadExe = Join-Path $PSScriptRoot "..\target\debug\lead.exe"
if (-not (Test-Path $leadExe)) {
    Write-Host "Build LEAD first: cargo build -p zed" -ForegroundColor Yellow
    exit 1
}

Write-Host ""
Write-Host "Prerequisites:" -ForegroundColor Cyan
Write-Host "  1. LM Studio running with your local worker model loaded"
Write-Host "  2. Network endpoint reachable (Agent panel -> Network -> Configure)"
Write-Host "  3. agent.default_model set to the LM Studio worker model"
Write-Host "  4. default_profile: hybrid (or write with spawn_agent)"
Write-Host "  5. Network Agent toggle ON"
Write-Host ""

Write-Host "Optional live integration tests:" -ForegroundColor Cyan
Write-Host '  $env:NETWORK_AGENT_TEST_URL="http://192.168.145.248:1234/v1"'
Write-Host '  $env:NETWORK_AGENT_TEST_MODEL="your-network-model"'
Write-Host '  $env:LOCAL_AGENT_TEST_URL="http://127.0.0.1:1234/v1"'
Write-Host '  $env:LOCAL_AGENT_TEST_MODEL="your-local-model"'
Write-Host '  cargo test -p agent test_network_active_subagent_uses_default_model -- --nocapture'
Write-Host '  cargo test -p language_models live_network_agent -- --nocapture'
Write-Host ""

Write-Host "Manual UI verification:" -ForegroundColor Cyan
Write-Host "  1. Launch LEAD"
Write-Host "  2. Open Agent panel -> Network -> confirm orchestrator + local worker status"
Write-Host "  3. Send: Use spawn_agent to run git status in the project root"
Write-Host "  4. Confirm a subagent card appears labeled Local worker"
Write-Host "  5. Confirm LM Studio shows inference activity on the worker model"
Write-Host ""

$response = Read-Host "Launch LEAD now? (y/N)"
if ($response -eq "y" -or $response -eq "Y") {
    Start-Process -FilePath $leadExe
    Write-Host "LEAD launched." -ForegroundColor Green
}
