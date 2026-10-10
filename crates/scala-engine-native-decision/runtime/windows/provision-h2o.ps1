# Native Windows provisioning only; does not discover or change Scala app state.
[CmdletBinding()]
param(
    [Parameter(Mandatory=$true)][string]$RuntimeRoot,
    [Parameter(Mandatory=$true)][string]$ModelRoot,
    [Parameter(Mandatory=$true)][string]$EvidenceRoot,
    [string]$Name = 'h2o',
    [string]$RecipeDirectory = $PSScriptRoot
)
$ErrorActionPreference = 'Stop'
if ($env:OS -ne 'Windows_NT') { throw 'Native Windows is required' }
if (!$Name -or $Name -notmatch '^[a-zA-Z0-9_-]+$') { throw 'Invalid isolated runtime name' }
foreach ($directory in @($RuntimeRoot, $ModelRoot, $EvidenceRoot, $RecipeDirectory)) {
    if (![IO.Path]::IsPathRooted($directory)) { throw 'Explicit absolute roots are required' }
}
if (!(Test-Path (Join-Path $RecipeDirectory 'provision_h2o.py')) -or !(Test-Path $ModelRoot -PathType Container)) {
    throw 'Reviewed recipe and original pinned H2O model root are required'
}
[IO.Directory]::CreateDirectory($RuntimeRoot) | Out-Null
[IO.Directory]::CreateDirectory($EvidenceRoot) | Out-Null
function Native([string]$program, [string[]]$arguments) {
    $ErrorActionPreference = 'Continue'
    & $program @arguments
    if ($LASTEXITCODE -ne 0) { throw "$program exited with $LASTEXITCODE" }
}
# Native tar.exe and existing uv are prerequisites; no system Python, registry,
# driver, execution policy or PATH changes. The interpreter release is explicit.
Get-Command 'uv', 'tar.exe' -ErrorAction Stop | Out-Null
$baseRoot = Join-Path $RuntimeRoot "$Name-python-3.12.12-20260211"
$base = Join-Path $baseRoot 'python.exe'
$archive = Join-Path $RuntimeRoot "$Name-python-3.12.12-20260211.tar.gz"
$hash = '93bf8e8c05ede0077b197a29c99ebdaf253497f27190097494265150b4e70ba8'
$url = 'https://github.com/astral-sh/python-build-standalone/releases/download/20260211/cpython-3.12.12%2B20260211-x86_64-pc-windows-msvc-install_only_stripped.tar.gz'
$savedCache = $env:UV_CACHE_DIR
$env:UV_CACHE_DIR = Join-Path $RuntimeRoot "$Name-uv-cache"
try {
    if (!(Test-Path $archive)) {
        $partial = "$archive.partial"
        try {
            Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile $partial
            if ((Get-FileHash $partial -Algorithm SHA256).Hash.ToLowerInvariant() -ne $hash) { throw 'Interpreter acquisition hash mismatch' }
            Move-Item $partial $archive
        } finally { Remove-Item $partial -Force -ErrorAction SilentlyContinue }
    }
    if ((Get-FileHash $archive -Algorithm SHA256).Hash.ToLowerInvariant() -ne $hash) { throw 'Cached interpreter archive changed' }
    if (!(Test-Path $baseRoot)) {
        $staging = "$baseRoot.partial"
        if (Test-Path $staging) { throw 'Incomplete interpreter extraction exists; retry with a new runtime name' }
        [IO.Directory]::CreateDirectory($staging) | Out-Null
        Native 'tar.exe' @('-xf', $archive, '-C', $staging)
        Move-Item (Join-Path $staging 'python') $baseRoot
        Remove-Item $staging
    }
    if (!(Test-Path $base -PathType Leaf)) { throw 'Pinned interpreter is incomplete; retry with a new name' }
    Native $base @('-I', '-B', (Join-Path $RecipeDirectory 'provision_h2o.py'),
        '--runtime-root', $RuntimeRoot, '--model-root', $ModelRoot,
        '--evidence-root', $EvidenceRoot, '--name', $Name)
} finally { $env:UV_CACHE_DIR = $savedCache }
