#requires -Version 5.1
# Scala-owned native dependency provisioning. Runs only on explicit runtime installation.
# No model, runtime selection, GPU driver or system PATH/CUDA changes. MSVC/SDK uses
# Microsoft's signed installer, with Windows approval when elevation is required.
param([string]$Root, [string]$ResultPath, [string]$BundleSha256, [switch]$SelfTest)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$Utf8 = New-Object System.Text.UTF8Encoding($false)
Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem
Add-Type -AssemblyName System.Net.Http
function Progress([string]$Message) { [Console]::Out.WriteLine('Native dependencies: ' + $Message) }
function Hash([string]$Path) { (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant() }
function Write-Json([string]$Path, $Value) {
    $partial = $Path + '.partial'
    [IO.File]::WriteAllText($partial, ($Value | ConvertTo-Json -Depth 12 -Compress), $Utf8)
    Move-Item -LiteralPath $partial -Destination $Path -Force
}
function Assert-Under([string]$Parent, [string]$Path) {
    $p = [IO.Path]::GetFullPath($Parent).TrimEnd('\','/') + [IO.Path]::DirectorySeparatorChar
    $v = [IO.Path]::GetFullPath($Path)
    if (!$v.StartsWith($p, [StringComparison]::OrdinalIgnoreCase)) { throw "Path escapes managed dependency root: $Path" }
    $v
}
function Assert-NotLink([string]$Path) {
    $p = [IO.Path]::GetFullPath($Path)
    while ($p) {
        if ((Test-Path -LiteralPath $p) -and ((Get-Item -LiteralPath $p -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            throw "Managed dependency path contains a junction or symbolic link: $p"
        }
        $next = [IO.Path]::GetDirectoryName($p)
        if ($next -eq $p) { break }; $p = $next
    }
}
function Assert-Asset($Asset) {
    $uri = [Uri]$Asset.url
    if ($uri.Scheme -ne 'https' -or $uri.UserInfo -or
        $uri.Host -notin @('github.com','developer.download.nvidia.com','download.visualstudio.microsoft.com') -or
        $Asset.sha256 -notmatch '^[a-f0-9]{64}$' -or [long]$Asset.size -le 0 -or
        $Asset.id -notmatch '^[a-z0-9_-]+$') { throw 'Invalid pinned native dependency asset' }
}
function Download($Asset) {
    Assert-Asset $Asset
    $path = Join-Path $Root ('downloads/' + $Asset.sha256 + [IO.Path]::GetExtension(([Uri]$Asset.url).AbsolutePath))
    Assert-NotLink $path
    if ((Test-Path -LiteralPath $path) -and (Get-Item -LiteralPath $path).Length -eq [long]$Asset.size -and (Hash $path) -eq $Asset.sha256) {
        return $path
    }
    Progress ('Downloading ' + $Asset.id)
    $part = $path + '.partial'
    Assert-NotLink $part
    $client = New-Object System.Net.Http.HttpClient
    $client.Timeout = [TimeSpan]::FromMinutes(30)
    $response = $null; $stream = $null; $output = $null
    try {
        $response = $client.GetAsync([string]$Asset.url, [Net.Http.HttpCompletionOption]::ResponseHeadersRead).GetAwaiter().GetResult()
        $response.EnsureSuccessStatusCode() | Out-Null
        if ($response.RequestMessage.RequestUri.Scheme -ne 'https') { throw 'Dependency download redirected away from HTTPS' }
        $stream = $response.Content.ReadAsStreamAsync().GetAwaiter().GetResult()
        $output = [IO.File]::Open($part, [IO.FileMode]::Create, [IO.FileAccess]::Write, [IO.FileShare]::None)
        $buffer = New-Object byte[] 1048576; [long]$total = 0
        while (($count = $stream.Read($buffer, 0, $buffer.Length)) -gt 0) {
            $total += $count
            if ($total -gt [long]$Asset.size) { throw ('Oversized download: ' + $Asset.id) }
            $output.Write($buffer, 0, $count)
        }
        $output.Dispose(); $output = $null
        if ($total -ne [long]$Asset.size -or (Hash $part) -ne $Asset.sha256) { throw ('Dependency checksum/size mismatch: ' + $Asset.id) }
        Move-Item -LiteralPath $part -Destination $path -Force
    } finally {
        if ($output) { $output.Dispose() }; if ($stream) { $stream.Dispose() }
        if ($response) { $response.Dispose() }; $client.Dispose()
        if (Test-Path -LiteralPath $part) { Remove-Item -LiteralPath $part -Force }
    }
    $path
}
function Expand-Safe([string]$Archive, [string]$Destination) {
    New-Item -ItemType Directory -Force $Destination | Out-Null
    $zip = [IO.Compression.ZipFile]::OpenRead($Archive)
    try {
        if ($zip.Entries.Count -gt 100000) { throw 'Dependency archive has too many entries' }
        [long]$total = 0
        foreach ($entry in $zip.Entries) {
            $name = $entry.FullName.Replace('\','/')
            if ($name.StartsWith('/') -or $name.Contains(':') -or $name.Split('/') -contains '..' -or
                (($entry.ExternalAttributes -shr 16) -band 61440) -eq 40960) { throw 'Unsafe dependency archive entry' }
            $target = Assert-Under $Destination (Join-Path $Destination $name)
            $total += $entry.Length
            if ($total -gt 32GB) { throw 'Dependency archive exceeds extraction limit' }
            if ($name.EndsWith('/')) { New-Item -ItemType Directory -Force $target | Out-Null; continue }
            New-Item -ItemType Directory -Force ([IO.Path]::GetDirectoryName($target)) | Out-Null
            [IO.Compression.ZipFileExtensions]::ExtractToFile($entry, $target, $false)
        }
    } finally { $zip.Dispose() }
}
function Package($Asset) {
    $archive = Download $Asset
    $folder = Join-Path $Root ('packages/' + $Asset.id)
    $receipt = Join-Path $folder 'scala-extraction.json'
    Assert-NotLink $folder
    $valid = $false
    if (Test-Path -LiteralPath $receipt) {
        try {
            $saved = Get-Content -LiteralPath $receipt -Raw | ConvertFrom-Json
            $valid = $saved.sha256 -eq $Asset.sha256 -and @($saved.files).Count -gt 0
            foreach ($file in $saved.files) {
                if (!$valid) { break }
                $p = Assert-Under $folder (Join-Path $folder $file.path); Assert-NotLink $p
                $valid = (Test-Path -LiteralPath $p -PathType Leaf) -and (Hash $p) -eq $file.sha256
            }
        } catch { $valid = $false }
    }
    if (!$valid) {
        if (Test-Path -LiteralPath $folder) { Remove-Item -LiteralPath $folder -Recurse -Force }
        Progress ('Extracting ' + $Asset.id)
        Expand-Safe $archive $folder
        $files = @(Get-ChildItem -LiteralPath $folder -Recurse -File | ForEach-Object {
            [ordered]@{path=$_.FullName.Substring($folder.Length+1).Replace('\','/');sha256=(Hash $_.FullName)}
        })
        Write-Json $receipt ([ordered]@{sha256=$Asset.sha256;files=$files})
    }
    if ($Asset.root) { Join-Path $folder $Asset.root } else { $folder }
}
function Run([string]$Program, [string[]]$Arguments) {
    $old=$ErrorActionPreference
    try {
        $ErrorActionPreference='Continue'
        & $Program @Arguments 2>&1 | ForEach-Object { [Console]::Out.WriteLine([string]$_) }
        if ($LASTEXITCODE -ne 0) { throw ('Dependency command failed (' + $LASTEXITCODE + '): ' + [IO.Path]::GetFileName($Program)) }
    } finally { $ErrorActionPreference=$old }
}
function Capture([string]$Program, [string[]]$Arguments) {
    $old=$ErrorActionPreference
    try {
        $ErrorActionPreference='Continue'
        $text = & $Program @Arguments 2>&1
        if ($LASTEXITCODE -ne 0) { throw ('Dependency verification failed: ' + [IO.Path]::GetFileName($Program) + ': ' + ($text -join ' ')) }
        ($text -join "`n").Trim()
    } finally { $ErrorActionPreference=$old }
}
function Vs-Environment([string]$Installation) {
    $batch=Join-Path $Installation 'VC/Auxiliary/Build/vcvarsall.bat'
    if (!(Test-Path -LiteralPath $batch)) { return $null }
    # Run vcvarsall through a private batch script instead of nested CMD quoting.
    $runner=Join-Path $PSScriptRoot ('scala-msvc-'+[guid]::NewGuid().ToString('N')+'.cmd')
    $nl=[string][char]13+[char]10
    $body='@echo off'+$nl+'call "'+$batch+'" x64 '+$Lock.sdk_version+' -vcvars_ver='+$Lock.msvc_version+' >nul'+$nl+'if errorlevel 1 exit /b 1'+$nl+'set'+$nl
    [IO.File]::WriteAllText($runner,$body,[Text.Encoding]::ASCII)
    $process=New-Object Diagnostics.Process
    $process.StartInfo.FileName=$env:ComSpec
    $process.StartInfo.Arguments='/d /s /c ""'+$runner+'""'
    $process.StartInfo.UseShellExecute=$false
    $process.StartInfo.CreateNoWindow=$true
    $process.StartInfo.RedirectStandardOutput=$true
    $process.StartInfo.RedirectStandardError=$true
    try {
        $process.Start()|Out-Null
        $stdout=$process.StandardOutput.ReadToEnd()
        $stderr=$process.StandardError.ReadToEnd()
        $process.WaitForExit()
        if($process.ExitCode -ne 0){return $null}
        $values=$stdout -split ([string][char]13+[char]10)
    } finally {
        $process.Dispose()
        Remove-Item -LiteralPath $runner -Force -ErrorAction SilentlyContinue
    }
    $map=@{}
    foreach($value in $values){if($value -match '^([^=]+)=(.*)$'){$map[$matches[1]]=$matches[2]}}
    if(!$map.VCToolsVersion -or !$map.VCToolsVersion.StartsWith($Lock.msvc_version+'.') -or
       !$map.WindowsSDKVersion -or $map.WindowsSDKVersion.TrimEnd('\') -ne $Lock.sdk_version){return $null}
    $cl=Join-Path $map.VCToolsInstallDir 'bin/Hostx64/x64/cl.exe'
    if(!(Test-Path -LiteralPath $cl)){return $null}
    $map
}
function Msvc-Arguments([string]$Installation, [string]$Manifest, [string]$Channel) {
    @('--productId', $Lock.vs_product, '--channelId', 'VisualStudio.17.Release',
      '--installPath', $Installation, '--installCatalogUri', $Manifest,
      '--installChannelUri', $Channel, '--channelUri', $Channel, '--add', $Lock.vs_components[0], '--add', $Lock.vs_components[1],
      '--passive', '--wait', '--norestart', '--nocache')
}
function Ensure-Msvc {
    $where = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
    if (Test-Path -LiteralPath $where) {
        $instances = (& $where -products '*' -version '[17.14,17.15)' -format json | ConvertFrom-Json)
        foreach ($instance in $instances) {
            $captured = Vs-Environment $instance.installationPath
            if ($captured) { Progress 'Reusing compatible MSVC and Windows SDK'; return $captured }
        }
    }
    $installation = Join-Path ${env:ProgramFiles(x86)} 'Scala/BuildTools-17.14'
    $existing = Vs-Environment $installation
    if ($existing) { return $existing }
    $setup = Download ($Lock.assets | Where-Object id -eq 'vs-setup')
    $manifest = Download ($Lock.assets | Where-Object id -eq 'vs-manifest')
    $channel = Join-Path $PSScriptRoot 'vs-channel.json'
    if ((Hash $channel) -ne $Lock.vs_channel_sha256) { throw 'Pinned Visual Studio channel changed' }
    $signature = Get-AuthenticodeSignature -LiteralPath $setup
    if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Subject -notmatch 'O=Microsoft Corporation') {
        throw 'Microsoft build-tools installer signature is invalid'
    }
    Progress 'Installing Microsoft C++ build tools and Windows SDK; approve the Windows permission prompt if shown'
    $arguments = Msvc-Arguments $installation $manifest $channel
    $quoted = ($arguments | ForEach-Object { if ($_ -match '["\r\n]') { throw 'Invalid installer argument' }; '"' + $_ + '"' }) -join ' '
    $admin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
    try {
        if ($admin) { $process = Start-Process -FilePath $setup -ArgumentList $quoted -Wait -PassThru -NoNewWindow }
        else { $process = Start-Process -FilePath $setup -ArgumentList $quoted -Wait -PassThru -Verb RunAs }
    } catch { throw ('Windows build-tools approval was cancelled or unavailable: ' + $_.Exception.Message) }
    if ($process.ExitCode -eq 3010) { throw '[restart-required] Windows installed build prerequisites and requires a restart. Retry the runtime install after restarting; verified downloads will be reused.' }
    if ($process.ExitCode -ne 0) { throw ('Microsoft build-tools installation failed with exit code ' + $process.ExitCode) }
    # Microsoft Setup can exit successfully just before vcvars/SDK registration
    # becomes visible to the requesting Windows process. Retry a bounded check.
    for ($attempt = 0; $attempt -lt 30; $attempt++) {
        $captured = Vs-Environment $installation
        if ($captured) { return $captured }
        Start-Sleep -Seconds 2
    }
    throw 'Microsoft setup completed but selected MSVC/SDK failed private developer environment validation'
}
function Assert-Probes($Tools, $Environment, [string]$CudaRoot, [string]$Prefix) {
    $cmake = Capture $Tools.cmake @('--version')
    if ($cmake -notmatch 'cmake version 3\.31\.8') { throw 'Unexpected managed CMake version' }
    $ninja = Capture $Tools.ninja @('--version')
    if ($ninja -ne '1.13.1') { throw 'Unexpected managed Ninja version' }
    $nvcc = Capture $Tools.nvcc @('--version')
    if ($nvcc -notmatch 'release 13\.4,') { throw 'The selected CUDA 13.4 toolkit is unavailable' }
    $targets = Capture $Tools.nvcc @('--help')
    if ($targets -notmatch '\bsm_120a\b' -or $targets -notmatch '\bcompute_120a\b') { throw 'Selected CUDA compiler does not support sm_120a' }
    $probe = Join-Path $Root 'msvc-probe.cpp'
    [IO.File]::WriteAllText($probe, '#include <span>' + "`n" + '#if _MSC_VER != 1944' + "`n" + '#error Wrong MSVC generation' + "`n" + '#endif' + "`n" + 'int main(){int a[1]{};std::span<int> s{a};return int(s.size())-1;}', $Utf8)
    try { Run $Tools.cl @('/nologo','/std:c++20','/Zc:__cplusplus','/Zs',$probe) } finally { Remove-Item -LiteralPath $probe -Force }
    foreach ($relative in @('include/cuda.h','include/cuda_runtime.h','include/cublas_v2.h','include/nvrtc.h','include/nvml.h','include/cccl/cuda/std/span')) {
        if (!(Test-Path -LiteralPath (Join-Path $CudaRoot $relative))) { throw ('Incomplete CUDA toolchain: ' + $relative) }
    }
    foreach ($relative in @('include/libavformat/avformat.h','include/curl/curl.h','lib/avformat.lib','lib/avcodec.lib','lib/avutil.lib','lib/swscale.lib','share/curl/CURLConfig.cmake')) {
        if (!(Test-Path -LiteralPath (Join-Path $Prefix $relative))) { throw ('Incomplete managed FFmpeg/curl dependencies: ' + $relative) }
    }
    @{cmake_version=$cmake;ninja_version=$ninja;make_version='not required';cpp_compiler=('MSVC ' + $Environment.VCToolsVersion);nvcc_version=$nvcc;pkg_config_version='not required';system_dependencies=@{}}
}
function Test-Provisioner {
    $tmp = Join-Path ([IO.Path]::GetTempPath()) ('scala-source-test-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory $tmp | Out-Null
    try {
        $failed=$false; try { Assert-Under $tmp (Join-Path $tmp '../escape') | Out-Null } catch {$failed=$true}; if(!$failed){throw 'Traversal was accepted'}
        $failed=$false; try { Assert-Asset ([pscustomobject]@{id='x';url='http://github.com/x';sha256=('a'*64);size=1}) } catch {$failed=$true}; if(!$failed){throw 'HTTP was accepted'}
        $zipPath=Join-Path $tmp 'unsafe.zip'; $zip=[IO.Compression.ZipFile]::Open($zipPath,[IO.Compression.ZipArchiveMode]::Create)
        $zip.CreateEntry('../outside') | Out-Null; $zip.Dispose()
        $failed=$false; try { Expand-Safe $zipPath (Join-Path $tmp 'expanded') } catch {$failed=$true}; if(!$failed){throw 'Unsafe zip was accepted'}
        $safe=Join-Path $tmp 'safe.zip'; $zip=[IO.Compression.ZipFile]::Open($safe,[IO.Compression.ZipArchiveMode]::Create)
        $e=$zip.CreateEntry('dir/tool.txt'); $w=New-Object IO.StreamWriter($e.Open()); $w.Write('fixture'); $w.Dispose(); $zip.Dispose()
        Expand-Safe $safe (Join-Path $tmp 'ok')
        if ((Get-Content (Join-Path $tmp 'ok/dir/tool.txt') -Raw) -ne 'fixture') { throw 'Safe zip failed' }
        $script:Lock=[pscustomobject]@{vs_product='BuildTools';vs_components=@('C++','SDK')}
        $a=Msvc-Arguments 'C:\Build Tools' 'C:\manifest' 'C:\channel'
        if ($a -notcontains '--channelUri' -or $a -notcontains '--norestart' -or $a -contains '--includeRecommended' -or $a -notcontains 'SDK') { throw 'Invalid minimum installer plan' }
        $script:Root=$tmp
        New-Item -ItemType Directory -Force (Join-Path $tmp 'downloads') | Out-Null
        $sha=Hash $safe
        $asset=[pscustomobject]@{id='fixture';url='https://github.com/example/fixture.zip';sha256=$sha;size=(Get-Item $safe).Length;root=''}
        Copy-Item $safe (Join-Path $tmp ('downloads/'+$sha+'.zip'))
        $one=Package $asset; $two=Package $asset
        if ($one -ne $two -or @($one).Count -ne 1) { throw 'Cached package did not return one stable path' }
        [IO.File]::WriteAllText((Join-Path $one 'dir/tool.txt'),'damaged',$Utf8)
        $three=Package $asset
        if ((Get-Content (Join-Path $three 'dir/tool.txt') -Raw) -ne 'fixture') { throw 'Damaged extraction was not repaired from verified cache' }
        $vsRoot=Join-Path $tmp 'Build Tools'
        $vcTools=Join-Path $vsRoot 'VC/Tools/MSVC/14.44.0'
        $batchDir=Join-Path $vsRoot 'VC/Auxiliary/Build'
        New-Item -ItemType Directory -Force $batchDir,(Join-Path $vcTools 'bin/Hostx64/x64') | Out-Null
        [IO.File]::WriteAllText((Join-Path $vcTools 'bin/Hostx64/x64/cl.exe'),'synthetic compiler, never executed',$Utf8)
        $batch="@echo off`r`nset VCToolsVersion=14.44.0`r`nset WindowsSDKVersion=10.0.26100.0\`r`nset VCToolsInstallDir=$vcTools\`r`nexit /b 0`r`n"
        [IO.File]::WriteAllText((Join-Path $batchDir 'vcvarsall.bat'),$batch,$Utf8)
        $script:Lock=[pscustomobject]@{msvc_version='14.44';sdk_version='10.0.26100.0'}
        $captured=Vs-Environment $vsRoot
        if (!$captured -or $captured.VCToolsVersion -ne '14.44.0') { throw 'Automatic developer environment capture failed for a path with spaces' }
        [IO.File]::WriteAllText((Join-Path $batchDir 'vcvarsall.bat'),$batch.Replace('10.0.26100.0','10.0.22621.0'),$Utf8)
        if (Vs-Environment $vsRoot) { throw 'An incompatible SDK was reused' }
        Write-Output 'Native provisioning self-test: 9 passed (no downloads, installations or compiled-code execution)'
    } finally { Remove-Item -LiteralPath $tmp -Recurse -Force }
}
if ($SelfTest) { Test-Provisioner; exit 0 }
try {
    if (!$Root -or !$ResultPath -or $BundleSha256 -notmatch '^[a-f0-9]{64}$') { throw 'Missing managed installation inputs' }
    $Root=[IO.Path]::GetFullPath($Root).TrimEnd('\','/'); Assert-NotLink $Root
    if ((Get-Content -LiteralPath (Join-Path $Root '.scala-cache-owner') -Raw) -ne $BundleSha256) { throw 'Dependency cache has no matching Scala ownership' }
    $script:Lock=Get-Content -LiteralPath (Join-Path $PSScriptRoot 'windows.lock.json') -Raw | ConvertFrom-Json
    if ($Lock.schema_version -ne 1) { throw 'Unknown native dependency lock schema' }
    foreach($asset in $Lock.assets) { Assert-Asset $asset }
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    foreach($dir in @('downloads','packages','cuda','vcpkg-downloads','vcpkg-cache')) { New-Item -ItemType Directory -Force (Join-Path $Root $dir) | Out-Null }
    $env:PATH=($env:SystemRoot+'\System32;'+$env:SystemRoot+';'+$env:SystemRoot+'\System32\WindowsPowerShell\v1.0')
    $gitRoot=Package ($Lock.assets | Where-Object id -eq 'git')
    $cmakeRoot=Package ($Lock.assets | Where-Object id -eq 'cmake')
    $ninjaRoot=Package ($Lock.assets | Where-Object id -eq 'ninja')
    $git=Join-Path $gitRoot 'cmd/git.exe'; $cmake=Join-Path $cmakeRoot 'bin/cmake.exe'; $ninja=Join-Path $ninjaRoot 'ninja.exe'
    $env:PATH=([IO.Path]::GetDirectoryName($git)+';'+[IO.Path]::GetDirectoryName($cmake)+';'+$ninjaRoot+';'+$env:PATH)
    $vs=Ensure-Msvc
    foreach($key in @('PATH','INCLUDE','LIB','LIBPATH','VSINSTALLDIR','VCINSTALLDIR','VCToolsInstallDir','VCToolsRedistDir','VCToolsVersion','WindowsSdkDir','WindowsSDKVersion','WindowsSDKLibVersion','UniversalCRTSdkDir','UCRTVersion','VSCMD_ARG_TGT_ARCH','VSCMD_ARG_HOST_ARCH')) {
        if ($vs[$key]) { Set-Item -Path ('Env:'+ $key) -Value $vs[$key] }
    }
    $cudaRoot=Join-Path $Root 'cuda'
    foreach($asset in $Lock.assets | Where-Object kind -eq 'cuda') {
        $package=Package $asset
        foreach($item in Get-ChildItem -LiteralPath $package -Force) {
            if ($item.Name -match '^(LICENSE|EULA|version|README)' -or $item.Name -eq 'scala-extraction.json') { continue }
            Copy-Item -LiteralPath $item.FullName -Destination $cudaRoot -Recurse -Force
        }
    }
    $nvcc=Join-Path $cudaRoot 'bin/nvcc.exe'
    $env:CUDA_PATH=$cudaRoot
    $env:PATH=([IO.Path]::GetDirectoryName($nvcc)+';'+(Join-Path $cudaRoot 'bin/x64')+';'+[IO.Path]::GetDirectoryName($git)+';'+[IO.Path]::GetDirectoryName($cmake)+';'+$ninjaRoot+';'+$env:PATH)
    $env:VCPKG_ROOT=Join-Path $Root 'vcpkg'; $env:VCPKG_TARGET_TRIPLET='x64-windows'; $env:VCPKG_DEFAULT_TRIPLET='x64-windows'
    $env:VCPKG_DOWNLOADS=Join-Path $Root 'vcpkg-downloads'; $env:VCPKG_DEFAULT_BINARY_CACHE=Join-Path $Root 'vcpkg-cache'
    $env:VCPKG_DISABLE_METRICS='1'; $env:VCPKG_MAX_CONCURRENCY=[string][Math]::Min(8,[Environment]::ProcessorCount)
    $env:VCPKG_VISUAL_STUDIO_PATH=$vs.VSINSTALLDIR.TrimEnd('\')
    $env:GIT_CONFIG_NOSYSTEM='1'; $env:GIT_CONFIG_GLOBAL='NUL'; $env:GIT_TERMINAL_PROMPT='0'
    if (!(Test-Path -LiteralPath (Join-Path $env:VCPKG_ROOT '.git'))) {
        New-Item -ItemType Directory -Force $env:VCPKG_ROOT | Out-Null
        Progress 'Acquiring the pinned vcpkg dependency recipes'
        Run $git @('init','--quiet',$env:VCPKG_ROOT)
        Run $git @('-C',$env:VCPKG_ROOT,'remote','add','origin','https://github.com/microsoft/vcpkg.git')
    }
    # Windows Git must admit long pinned vcpkg source paths before checkout.
    Run $git @('-C',$env:VCPKG_ROOT,'config','--local','core.longpaths','true')
    Run $git @('-C',$env:VCPKG_ROOT,'config','--local','core.autocrlf','false')
    try { $head=Capture $git @('-C',$env:VCPKG_ROOT,'rev-parse','HEAD') } catch { $head='' }
    if ($head -ne $Lock.vcpkg_commit) {
        Run $git @('-C',$env:VCPKG_ROOT,'fetch','--quiet','--depth=1','origin',$Lock.vcpkg_commit)
        Run $git @('-C',$env:VCPKG_ROOT,'-c','core.autocrlf=false','-c','core.eol=lf','checkout','--quiet','--detach','FETCH_HEAD')
    }
    if ((Capture $git @('-C',$env:VCPKG_ROOT,'rev-parse','HEAD^{tree}')) -ne $Lock.vcpkg_tree) { throw 'Pinned vcpkg source tree changed' }
    # Retry recovery comes exclusively from the verified immutable Git tree.
    Run $git @('-C',$env:VCPKG_ROOT,'restore','--source=HEAD','--worktree','--','scripts','ports','triplets','versions')
    if (Capture $git @('-C',$env:VCPKG_ROOT,'diff','--name-only','HEAD','--','scripts','ports','triplets','versions')) { throw 'Cached vcpkg recipe files were modified' }
    if (Capture $git @('-C',$env:VCPKG_ROOT,'ls-files','--others','--exclude-standard','--','scripts','ports','triplets','versions')) { throw 'Cached vcpkg recipe has unexpected untracked source files' }
    $vcpkg=Join-Path $env:VCPKG_ROOT 'vcpkg.exe'
    Copy-Item -LiteralPath (Download ($Lock.assets | Where-Object id -eq 'vcpkg-tool')) -Destination $vcpkg -Force
    $manifestRoot=Join-Path $Root 'dependency-manifest'; New-Item -ItemType Directory -Force $manifestRoot | Out-Null
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'vcpkg.json') -Destination (Join-Path $manifestRoot 'vcpkg.json') -Force
    Progress 'Installing FFmpeg, curl and their complete pinned dependency closure'
    # CMake's own 250-character object-directory limit rejects ordinary source
    # trees under the long managed download/cache path on Windows. Use a short
    # per-user build-only directory; the verified vcpkg sources, downloads and
    # installed output remain bound to this exact Scala dependency recipe.
    $buildParent=Join-Path $env:LOCALAPPDATA 'Scala/NativeBuild'
    $shortRoot=Join-Path $buildParent $BundleSha256.Substring(0,16)
    if ($shortRoot.Length -gt 96) { throw 'Windows local application-data path is too long for the selected native vcpkg build layout' }
    Assert-NotLink $shortRoot
    New-Item -ItemType Directory -Force $shortRoot | Out-Null
    $buildOwner=Join-Path $shortRoot '.scala-cache-owner'
    if (Test-Path -LiteralPath $buildOwner) {
        Assert-NotLink $buildOwner
        if ((Get-Content -LiteralPath $buildOwner -Raw) -ne $BundleSha256) { throw 'Managed short build cache ownership changed' }
    } else {
        if (@(Get-ChildItem -LiteralPath $shortRoot -Force).Count -ne 0) { throw 'Refusing an unowned short build cache' }
        [IO.File]::WriteAllText($buildOwner,$BundleSha256,$Utf8)
    }
    $shortBuildTrees=Join-Path $shortRoot 'buildtrees'
    $shortPackages=Join-Path $shortRoot 'packages'
    Progress 'Building pinned native FFmpeg/curl dependencies in the short private Windows build tree'
    Run $vcpkg @('install','--disable-metrics','--triplet=x64-windows',('--x-manifest-root='+$manifestRoot),('--x-install-root='+(Join-Path $env:VCPKG_ROOT 'installed')),('--x-buildtrees-root='+$shortBuildTrees),('--x-packages-root='+$shortPackages),'--clean-after-build')
    $prefix=Join-Path $env:VCPKG_ROOT 'installed/x64-windows'
    $tools=@{git=$git;cmake=$cmake;ninja=$ninja;nvcc=$nvcc;cl=(Join-Path $vs.VCToolsInstallDir 'bin/Hostx64/x64/cl.exe')}
    Progress 'Verifying the prepared compiler, toolkit and library headers'
    $toolchain=Assert-Probes $tools $vs $cudaRoot $prefix
    $toolchain.system_dependencies=@{bundle_sha256=$BundleSha256;cuda_release=[string]$Lock.cuda_release;windows_sdk=[string]$Lock.sdk_version;vcpkg_commit=[string]$Lock.vcpkg_commit;vcpkg_tree=[string]$Lock.vcpkg_tree;vcpkg_version=(Capture $vcpkg @('version','--disable-metrics'));vcpkg_status_sha256=(Hash (Join-Path $env:VCPKG_ROOT 'installed/vcpkg/status'));msvc_installation=[string]$vs.VSINSTALLDIR}
    foreach($asset in $Lock.assets) { $toolchain.system_dependencies['download.'+$asset.id+'.sha256']=[string]$asset.sha256 }
    $crtRoot=Join-Path $vs.VCToolsRedistDir 'x64/Microsoft.VC143.CRT'
    if (!(Test-Path -LiteralPath $crtRoot)) { throw 'Selected Microsoft CRT redistributable files are missing' }
    $runtimeFiles=@(Get-ChildItem -LiteralPath (Join-Path $prefix 'bin') -Filter '*.dll' -File | Select-Object -ExpandProperty FullName)
    $runtimeFiles+=@(Get-ChildItem -LiteralPath $crtRoot -Filter '*.dll' -File | Select-Object -ExpandProperty FullName)
    foreach($glob in @('cublas*.dll','nvrtc*.dll','cudart*.dll')) {
        $runtimeFiles+=@(Get-ChildItem -LiteralPath (Join-Path $cudaRoot 'bin') -Filter $glob -Recurse -File | Select-Object -ExpandProperty FullName)
    }
    foreach($needed in @('avformat*.dll','avcodec*.dll','avutil*.dll','swscale*.dll','libcurl*.dll','msvcp140.dll','vcruntime140.dll','cublas64*.dll')) {
        if (!($runtimeFiles | Where-Object { [IO.Path]::GetFileName($_) -like $needed })) { throw ('Missing runtime DLL dependency: '+$needed) }
    }
    $environment=@{}
    foreach($key in @('PATH','INCLUDE','LIB','LIBPATH','VSINSTALLDIR','VCINSTALLDIR','VCToolsInstallDir','VCToolsRedistDir','VCToolsVersion','WindowsSdkDir','WindowsSDKVersion','WindowsSDKLibVersion','UniversalCRTSdkDir','UCRTVersion','VSCMD_ARG_TGT_ARCH','VSCMD_ARG_HOST_ARCH','CUDA_PATH','VCPKG_ROOT','VCPKG_TARGET_TRIPLET','VCPKG_DEFAULT_TRIPLET','VCPKG_DISABLE_METRICS','GIT_CONFIG_NOSYSTEM','GIT_CONFIG_GLOBAL','GIT_TERMINAL_PROMPT')) {
        $value=[Environment]::GetEnvironmentVariable($key); if($value){$environment[$key]=$value}
    }
    Write-Json $ResultPath ([ordered]@{schema_version=1;bundle_sha256=$BundleSha256;tools=$tools;cuda_root=$cudaRoot;dependency_prefix=$prefix;toolchain=$toolchain;environment=$environment;runtime_files=@($runtimeFiles | Sort-Object -Unique)})
    Progress 'All native build dependencies are ready'
} catch { Write-Error $_ -ErrorAction Continue; exit 1 }
