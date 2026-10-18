#requires -Version 5.1
# Explicit Install only. Existing qualified Microsoft tools are never upgraded.
param([string]$Root,[string]$ResultPath,[string]$BundleSha256,[switch]$SelfTest)
. (Join-Path $PSScriptRoot 'windows-tools.ps1')
function Compatible-Msvc {
    $where=Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
    if(!(Test-Path -LiteralPath $where)){return $null}
    $instances=& $where -products '*' -version '[16.0,)' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -format json|ConvertFrom-Json
    foreach($instance in $instances){
        $batch=Join-Path $instance.installationPath 'VC/Auxiliary/Build/vcvarsall.bat'
        if(!(Test-Path -LiteralPath $batch)){continue}
        $runner=Join-Path $PSScriptRoot ('compiler-'+[guid]::NewGuid().ToString('N')+'.cmd')
        [IO.File]::WriteAllText($runner,('@echo off'+"`r`n"+'call "'+$batch+'" x64 >nul'+"`r`n"+'if errorlevel 1 exit /b 1'+"`r`n"+'set'+"`r`n"),[Text.Encoding]::ASCII)
        try{
            $lines=& $env:ComSpec /d /s /c ('"'+$runner+'"') 2>$null
            if($LASTEXITCODE -ne 0){continue}
            $values=@{};foreach($line in $lines){if($line -match '^([^=]+)=(.*)$'){$values[$matches[1]]=$matches[2]}}
            $compiler=Join-Path $values.VCToolsInstallDir 'bin/Hostx64/x64/cl.exe'
            $sdk=Join-Path $values.WindowsSdkDir ('Include/'+$values.WindowsSDKVersion.TrimEnd('\')+'/um/Windows.h')
            if(!(Test-Path -LiteralPath $compiler) -or !(Test-Path -LiteralPath $sdk)){continue}
            if([version]$values.VCToolsVersion -lt [version]'14.20'){continue}
            return $values
        }catch{continue}
        finally{Remove-Item -LiteralPath $runner -Force -ErrorAction SilentlyContinue}
    }
    return $null
}
if($SelfTest){
    $script:Lock=[pscustomobject]@{vs_product='BuildTools';vs_components=@('C++','SDK')}
    $args=Msvc-Arguments 'C:\Build Tools' 'C:\manifest' 'C:\channel'
    if($args -contains '--includeRecommended' -or $args -notcontains '--norestart' -or $args -notcontains '--nocache'){throw 'Unexpected Microsoft installation policy'}
    Write-Output 'Managed compiler self-test passed (no installation)';exit 0
}
try{
    if($BundleSha256 -notmatch '^[a-f0-9]{64}$' -or !$Root -or !$ResultPath){throw 'Missing managed compiler inputs'}
    Assert-NotLink $Root;Assert-NotLink $ResultPath
    if((Get-Content -LiteralPath (Join-Path $Root '.scala-cache-owner') -Raw) -ne $BundleSha256){throw 'Compiler cache ownership mismatch'}
    $script:DownloadRoot=Join-Path ([IO.Path]::GetDirectoryName($Root)) 'downloads'
    Assert-NotLink $DownloadRoot;New-Item -ItemType Directory -Force $DownloadRoot|Out-Null
    $script:Lock=Get-Content -LiteralPath (Join-Path $PSScriptRoot 'msvc.lock.json') -Raw|ConvertFrom-Json
    foreach($asset in $Lock.assets){Assert-Asset $asset}
    $env:PATH=$env:SystemRoot+'\System32;'+$env:SystemRoot
    $selected=Compatible-Msvc;$operation='reused'
    if(!$selected){$operation='provisioned-or-reused-pinned';$selected=Ensure-Msvc}
    if(!$selected.VCToolsVersion -or !$selected.WindowsSDKVersion){throw 'Microsoft compiler/SDK verification failed'}
    Write-Json $ResultPath ([ordered]@{schema_version=1;bundle_sha256=$BundleSha256;operation=$operation;installation=[string]$selected.VSINSTALLDIR;compiler_version=[string]$selected.VCToolsVersion;sdk_version=[string]$selected.WindowsSDKVersion;compiler=(Join-Path $selected.VCToolsInstallDir 'bin/Hostx64/x64/cl.exe')})
    Progress ('Microsoft C++ tools ready ('+$operation+'); upstream vLLM retains native compiler discovery')
}catch{Write-Error $_ -ErrorAction Continue;exit 1}
finally{if($DownloadRoot){try{Trim-Downloads $DownloadRoot 2GB}catch{Progress ('Download cache cleanup: '+$_.Exception.Message)}}}
