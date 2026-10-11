param([string]$Codex = 'codex')
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$output = Join-Path $root 'target/protocol-export'
& $Codex app-server generate-json-schema --out $output
if ($LASTEXITCODE -ne 0) { throw 'Schema export failed' }
$request = Get-Content (Join-Path $output 'ServerRequest.json') -Raw | ConvertFrom-Json
$contract = [ordered]@{
    version = (& $Codex --version | Out-String).Trim()
    source = 'https://learn.chatgpt.com/docs/app-server'
    serverMethods = @($request.oneOf | ForEach-Object { $_.properties.method.enum[0] })
    schemas = [ordered]@{}
}
foreach ($name in @('CommandExecutionRequestApprovalParams','CommandExecutionRequestApprovalResponse','FileChangeRequestApprovalParams','FileChangeRequestApprovalResponse','PermissionsRequestApprovalParams','PermissionsRequestApprovalResponse')) {
    $contract.schemas[$name] = Get-Content (Join-Path $output "$name.json") -Raw | ConvertFrom-Json
}
$contract | ConvertTo-Json -Depth 100 | Set-Content (Join-Path $root 'protocol/contract.json') -Encoding utf8NoBOM
