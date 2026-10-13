# Native Windows provisioning only. Never discovers or changes Scala app state.
# Requires the existing uv and git tools; no system Python/CUDA/PATH replacement.
[CmdletBinding()]
param(
    [Parameter(Mandatory=$true)][string]$RuntimeRoot,
    [Parameter(Mandatory=$true)][string]$EvidenceRoot,
    [string]$Name = 'imajev',
    # Explicit when invoking reviewed script text without changing execution policy.
    [string]$RecipeDirectory = $PSScriptRoot
)
$ErrorActionPreference = 'Stop'
if ($env:OS -ne 'Windows_NT') { throw 'Native Windows is required' }
if (!$Name -or $Name -notmatch '^[a-zA-Z0-9_-]+$') { throw 'Invalid isolated runtime name' }
if (![IO.Path]::IsPathRooted($RecipeDirectory) -or !(Test-Path (Join-Path $RecipeDirectory 'imajev-requirements.lock'))) {
    throw 'The absolute recipe directory must contain the reviewed Windows lock'
}
foreach ($directory in @($RuntimeRoot, $EvidenceRoot)) {
    if (![IO.Path]::IsPathRooted($directory)) { throw 'Explicit absolute roots are required' }
    [IO.Directory]::CreateDirectory($directory) | Out-Null
}
$savedInstall = $env:UV_PYTHON_INSTALL_DIR
$savedCache = $env:UV_CACHE_DIR
$env:UV_PYTHON_INSTALL_DIR = Join-Path $RuntimeRoot 'python'
$env:UV_CACHE_DIR = Join-Path $RuntimeRoot 'cache'
function Native([string]$program, [string[]]$arguments) {
    # PowerShell 5 treats native stderr as error records, including normal uv
    # progress. Check the native exit code instead of changing system policies.
    $ErrorActionPreference = 'Continue'
    & $program @arguments
    if ($LASTEXITCODE -ne 0) { throw "$program exited with $LASTEXITCODE" }
}
try {
    $environment = Join-Path $RuntimeRoot "$Name-env"
    $python = Join-Path $environment 'Scripts\python.exe'
    $implementation = Join-Path $RuntimeRoot "$Name-source"
    $runtime = Join-Path $RuntimeRoot "$Name-runtime"
    $revision = '91729bff7a806187324e287fd0d730839dc32026'
    $receipt = Join-Path $EvidenceRoot "$Name-runtime-probe.json"
    if (Test-Path $runtime) {
        if (!(Test-Path $receipt)) { throw 'Existing runtime needs its original probe receipt' }
        $current = Native $python @('-I', (Join-Path $runtime 'server.py'), '--scala-probe')
        if (($current | ConvertFrom-Json).revision -ne (Get-Content $receipt -Raw | ConvertFrom-Json).revision) {
            throw 'Existing immutable runtime changed; provision a separate named runtime'
        }
        $current
        return
    }
    Native 'uv' @('python', 'install', '3.12.12')
    $base = Join-Path $env:UV_PYTHON_INSTALL_DIR 'cpython-3.12.12-windows-x86_64-none\python.exe'
    if (!(Test-Path $environment)) { Native 'uv' @('venv', '--python', $base, $environment) }
    Native 'uv' @('pip', 'sync', '--python', $python, '--require-hashes', '--only-binary', ':all:',
        '--index', 'https://download.pytorch.org/whl/cu130', '--index-strategy', 'unsafe-best-match',
        (Join-Path $RecipeDirectory 'imajev-requirements.lock'))
    if (!(Test-Path $implementation)) {
        Native 'git' @('-c', 'core.autocrlf=false', 'clone', '--no-checkout',
            'https://github.com/mohit67890/imajev.git', $implementation)
        Native 'git' @('-C', $implementation, '-c', 'core.autocrlf=false', 'checkout', '--detach', $revision)
    }
    # prepare.py verifies the clean exact source revision, every installed wheel
    # against RECORD, the base interpreter closure and dedicated venv binding.
    $prepare = Join-Path $RecipeDirectory '..\prepare.py'
    $probe = Native $python @('-I', $prepare, 'runtime', '--backend', 'torch-readout',
        '--python', $python, '--source', $implementation, '--source-revision', $revision,
        '--option', 'device', '\"cuda\"', '--option', 'rotations', '4',
        '--option', 'max_input_tokens', '4096', '--output', $runtime)
    [IO.File]::WriteAllText($receipt, ($probe -join "`n") + "`n", [Text.UTF8Encoding]::new($false))
} finally {
    $env:UV_PYTHON_INSTALL_DIR = $savedInstall
    $env:UV_CACHE_DIR = $savedCache
}
