#requires -Version 5.1
# Runs only after the user selects Install. No model, driver or global PATH changes.
param([string]$Root, [string]$ResultPath, [string]$BundleSha256,
      [string]$EnvironmentSha256, [string]$DownloadRoot, [string]$BuildRoot, [switch]$SelfTest)
$ErrorActionPreference='Stop'
$ProgressPreference='SilentlyContinue'
$Utf8=New-Object Text.UTF8Encoding($false)
Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem
Add-Type -AssemblyName System.Net.Http
function Progress([string]$Message) { [Console]::Error.WriteLine('Native dependencies: '+$Message) }
function Hash([string]$Path) {
    $stream=[IO.File]::OpenRead($Path); $sha=[Security.Cryptography.SHA256]::Create()
    try { [BitConverter]::ToString($sha.ComputeHash($stream)).Replace('-','').ToLowerInvariant() }
    finally { $sha.Dispose(); $stream.Dispose() }
}
function Write-Json([string]$Path,$Value) {
    $partial=$Path+'.partial'
    [IO.File]::WriteAllText($partial,($Value|ConvertTo-Json -Depth 12 -Compress),$Utf8)
    Move-Item -LiteralPath $partial -Destination $Path -Force
}
function Assert-Under([string]$Parent,[string]$Path) {
    $p=[IO.Path]::GetFullPath($Parent).TrimEnd('\','/')+[IO.Path]::DirectorySeparatorChar
    $v=[IO.Path]::GetFullPath($Path)
    if(!$v.StartsWith($p,[StringComparison]::OrdinalIgnoreCase)){throw "Path escapes managed dependency root: $Path"}
    $v
}
function Assert-NotLink([string]$Path) {
    $p=[IO.Path]::GetFullPath($Path)
    while($p){
        if((Test-Path -LiteralPath $p) -and ((Get-Item -LiteralPath $p -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)){
            throw "Managed dependency path contains a junction or symbolic link: $p"
        }
        $next=[IO.Path]::GetDirectoryName($p);if($next -eq $p){break};$p=$next
    }
}
function Assert-Asset($Asset) {
    $uri=[Uri]$Asset.url
    if($uri.Scheme -ne 'https' -or $uri.UserInfo -or
       $uri.Host -notin @('github.com','developer.download.nvidia.com','download.visualstudio.microsoft.com') -or
       $Asset.sha256 -notmatch '^[a-f0-9]{64}$' -or [long]$Asset.size -le 0 -or
       $Asset.id -notmatch '^[a-z0-9_-]+$'){throw 'Invalid pinned native dependency asset'}
}
function Download($Asset) {
    Assert-Asset $Asset
    $path=Join-Path $DownloadRoot ($Asset.sha256+[IO.Path]::GetExtension(([Uri]$Asset.url).AbsolutePath))
    Assert-NotLink $path
    if((Test-Path -LiteralPath $path) -and (Get-Item -LiteralPath $path).Length -eq [long]$Asset.size -and (Hash $path) -eq $Asset.sha256){return $path}
    Progress ('Downloading '+$Asset.id)
    $part=$path+'.partial';Assert-NotLink $part
    $client=New-Object Net.Http.HttpClient;$client.Timeout=[TimeSpan]::FromMinutes(30)
    $response=$null;$stream=$null;$output=$null
    try{
        $response=$client.GetAsync([string]$Asset.url,[Net.Http.HttpCompletionOption]::ResponseHeadersRead).GetAwaiter().GetResult()
        $response.EnsureSuccessStatusCode()|Out-Null
        if($response.RequestMessage.RequestUri.Scheme -ne 'https'){throw 'Dependency download redirected away from HTTPS'}
        $stream=$response.Content.ReadAsStreamAsync().GetAwaiter().GetResult()
        $output=[IO.File]::Open($part,[IO.FileMode]::Create,[IO.FileAccess]::Write,[IO.FileShare]::None)
        $buffer=New-Object byte[] 1048576;[long]$total=0
        while(($count=$stream.Read($buffer,0,$buffer.Length)) -gt 0){
            $total+=$count;if($total -gt [long]$Asset.size){throw ('Oversized download: '+$Asset.id)}
            $output.Write($buffer,0,$count)
        }
        $output.Dispose();$output=$null
        if($total -ne [long]$Asset.size -or (Hash $part) -ne $Asset.sha256){throw ('Dependency checksum/size mismatch: '+$Asset.id)}
        Move-Item -LiteralPath $part -Destination $path -Force
    }finally{
        if($output){$output.Dispose()};if($stream){$stream.Dispose()};if($response){$response.Dispose()};$client.Dispose()
        if(Test-Path -LiteralPath $part){Remove-Item -LiteralPath $part -Force}
    }
    $path
}
function Expand-Safe([string]$Archive,[string]$Destination) {
    New-Item -ItemType Directory -Force $Destination|Out-Null
    $zip=[IO.Compression.ZipFile]::OpenRead($Archive)
    try{
        if($zip.Entries.Count -gt 100000){throw 'Dependency archive has too many entries'}
        [long]$total=0
        foreach($entry in $zip.Entries){
            $name=$entry.FullName.Replace('\','/')
            if($name.StartsWith('/') -or $name.Contains(':') -or $name.Split('/') -contains '..' -or
                (($entry.ExternalAttributes -shr 16) -band 61440) -eq 40960){throw 'Unsafe dependency archive entry'}
            $target=Assert-Under $Destination (Join-Path $Destination $name)
            $total+=$entry.Length;if($total -gt 32GB){throw 'Dependency archive exceeds extraction limit'}
            if($name.EndsWith('/')){New-Item -ItemType Directory -Force $target|Out-Null;continue}
            New-Item -ItemType Directory -Force ([IO.Path]::GetDirectoryName($target))|Out-Null
            [IO.Compression.ZipFileExtensions]::ExtractToFile($entry,$target,$false)
        }
    }finally{$zip.Dispose()}
}
function Inventory([string]$Folder) {
    @(Get-ChildItem -LiteralPath $Folder -Recurse -File|Where-Object {$_.Name -ne 'scala-extraction.json'}|ForEach-Object {
        [ordered]@{path=$_.FullName.Substring($Folder.Length+1).Replace('\','/');sha256=(Hash $_.FullName)}
    })
}
function Valid-Inventory([string]$Folder,[string]$Key) {
    $receipt=Join-Path $Folder 'scala-extraction.json'
    if(!(Test-Path -LiteralPath $receipt)){return $false}
    try{
        Assert-NotLink $receipt
        $saved=Get-Content -LiteralPath $receipt -Raw|ConvertFrom-Json
        if($saved.sha256 -ne $Key -or @($saved.files).Count -eq 0){return $false}
        foreach($file in $saved.files){
            $p=Assert-Under $Folder (Join-Path $Folder $file.path);Assert-NotLink $p
            if(!(Test-Path -LiteralPath $p -PathType Leaf) -or (Hash $p) -ne $file.sha256){return $false}
        }
        if(@(Get-ChildItem -LiteralPath $Folder -Recurse -File).Count -ne @($saved.files).Count+1){return $false}
        return $true
    }catch{return $false}
}
function Package($Asset) {
    Assert-Asset $Asset
    $folder=Join-Path $Root ('packages/'+$Asset.id);Assert-NotLink $folder
    if(!(Valid-Inventory $folder $Asset.sha256)){
        $archive=Download $Asset
        if(Test-Path -LiteralPath $folder){Remove-Item -LiteralPath $folder -Recurse -Force}
        Progress ('Extracting '+$Asset.id);Expand-Safe $archive $folder
        Write-Json (Join-Path $folder 'scala-extraction.json') ([ordered]@{sha256=$Asset.sha256;files=(Inventory $folder)})
    }else{Progress ('Reusing verified '+$Asset.id)}
    if($Asset.root){Join-Path $folder $Asset.root}else{$folder}
}
function Prepare-Cuda {
    $cudaRoot=Join-Path $Root 'cuda';Assert-NotLink $cudaRoot
    $cudaAssets=@($Lock.assets|Where-Object kind -eq 'cuda')
    $input=($cudaAssets|ForEach-Object {$_.id+':'+$_.sha256}) -join ';'
    $sha=[Security.Cryptography.SHA256]::Create()
    try{$key=[BitConverter]::ToString($sha.ComputeHash($Utf8.GetBytes($input))).Replace('-','').ToLowerInvariant()}finally{$sha.Dispose()}
    if(Valid-Inventory $cudaRoot $key){Progress 'Reusing verified CUDA components';return $cudaRoot}
    if(Test-Path -LiteralPath $cudaRoot){Remove-Item -LiteralPath $cudaRoot -Recurse -Force}
    New-Item -ItemType Directory -Force $cudaRoot|Out-Null
    foreach($asset in $cudaAssets){
        $stage=Join-Path $BuildRoot ('cuda-'+$asset.id)
        try{
            Expand-Safe (Download $asset) $stage
            $package=if($asset.root){Join-Path $stage $asset.root}else{$stage}
            foreach($file in Get-ChildItem -LiteralPath $package -Recurse -File){
                $relative=$file.FullName.Substring($package.Length+1)
                if($relative -match '^(LICENSE|EULA|version|README)'){$relative='licenses/'+$asset.id+'/'+$relative}
                $dest=Assert-Under $cudaRoot (Join-Path $cudaRoot $relative)
                New-Item -ItemType Directory -Force ([IO.Path]::GetDirectoryName($dest))|Out-Null
                if(Test-Path -LiteralPath $dest){
                    if((Hash $dest) -ne (Hash $file.FullName)){throw ('Conflicting pinned CUDA file: '+$relative)}
                }else{Move-Item -LiteralPath $file.FullName -Destination $dest}
            }
        }finally{if(Test-Path -LiteralPath $stage){Remove-Item -LiteralPath $stage -Recurse -Force}}
    }
    Write-Json (Join-Path $cudaRoot 'scala-extraction.json') ([ordered]@{sha256=$key;files=(Inventory $cudaRoot)})
    $cudaRoot
}
function Run([string]$Program,[string[]]$Arguments) {
    $old=$ErrorActionPreference
    try{
        $ErrorActionPreference='Continue'
        & $Program @Arguments 2>&1|ForEach-Object {[Console]::Error.WriteLine([string]$_)}
        if($LASTEXITCODE -ne 0){throw ('Dependency command failed ('+$LASTEXITCODE+'): '+[IO.Path]::GetFileName($Program))}
    }finally{$ErrorActionPreference=$old}
}
function Capture([string]$Program,[string[]]$Arguments) {
    $old=$ErrorActionPreference
    try{
        $ErrorActionPreference='Continue';$text=& $Program @Arguments 2>&1
        if($LASTEXITCODE -ne 0){throw ('Dependency verification failed: '+[IO.Path]::GetFileName($Program)+': '+($text -join ' '))}
        ($text -join "`n").Trim()
    }finally{$ErrorActionPreference=$old}
}
function Vs-Environment([string]$Installation) {
    $batch=Join-Path $Installation 'VC/Auxiliary/Build/vcvarsall.bat'
    if(!(Test-Path -LiteralPath $batch)){return $null}
    $runner=Join-Path $PSScriptRoot ('scala-msvc-'+[guid]::NewGuid().ToString('N')+'.cmd')
    $nl=[string][char]13+[char]10
    $body='@echo off'+$nl+'call "'+$batch+'" x64 '+$Lock.sdk_version+' -vcvars_ver='+$Lock.msvc_version+' >nul'+$nl+'if errorlevel 1 exit /b 1'+$nl+'set'+$nl
    [IO.File]::WriteAllText($runner,$body,[Text.Encoding]::ASCII)
    $process=New-Object Diagnostics.Process
    $process.StartInfo.FileName=$env:ComSpec
    $process.StartInfo.Arguments='/d /s /c ""'+$runner+'""'
    $process.StartInfo.UseShellExecute=$false;$process.StartInfo.CreateNoWindow=$true
    $process.StartInfo.RedirectStandardOutput=$true;$process.StartInfo.RedirectStandardError=$true
    try{
        $process.Start()|Out-Null;$stdout=$process.StandardOutput.ReadToEnd();$stderr=$process.StandardError.ReadToEnd();$process.WaitForExit()
        if($process.ExitCode -ne 0){return $null};$values=$stdout -split ([string][char]13+[char]10)
    }finally{$process.Dispose();Remove-Item -LiteralPath $runner -Force -ErrorAction SilentlyContinue}
    $map=@{};foreach($value in $values){if($value -match '^([^=]+)=(.*)$'){$map[$matches[1]]=$matches[2]}}
    if(!$map.VCToolsVersion -or !$map.VCToolsVersion.StartsWith($Lock.msvc_version+'.') -or
        !$map.WindowsSDKVersion -or $map.WindowsSDKVersion.TrimEnd('\') -ne $Lock.sdk_version){return $null}
    if(!(Test-Path -LiteralPath (Join-Path $map.VCToolsInstallDir 'bin/Hostx64/x64/cl.exe'))){return $null}
    $map
}
function Msvc-Arguments([string]$Installation,[string]$Manifest,[string]$Channel) {
    @('--productId',$Lock.vs_product,'--channelId','VisualStudio.17.Release','--installPath',$Installation,
      '--installCatalogUri',$Manifest,'--installChannelUri',$Channel,'--channelUri',$Channel,
      '--add',$Lock.vs_components[0],'--add',$Lock.vs_components[1],'--passive','--wait','--norestart','--nocache')
}
function Ensure-Msvc {
    $where=Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
    if(Test-Path -LiteralPath $where){
        $instances=(& $where -products '*' -version '[17.14,17.15)' -format json|ConvertFrom-Json)
        foreach($instance in $instances){$captured=Vs-Environment $instance.installationPath;if($captured){Progress 'Reusing compatible MSVC and Windows SDK';return $captured}}
    }
    $installation=Join-Path ${env:ProgramFiles(x86)} 'Scala/BuildTools-17.14'
    $existing=Vs-Environment $installation;if($existing){return $existing}
    $setup=Download ($Lock.assets|Where-Object id -eq 'vs-setup')
    $manifest=Download ($Lock.assets|Where-Object id -eq 'vs-manifest')
    $channel=Join-Path $PSScriptRoot 'vs-channel.json'
    if((Hash $channel) -ne $Lock.vs_channel_sha256){throw 'Pinned Visual Studio channel changed'}
    $signature=Get-AuthenticodeSignature -LiteralPath $setup
    if($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Subject -notmatch 'O=Microsoft Corporation'){throw 'Microsoft build-tools installer signature is invalid'}
    Progress 'Installing Microsoft C++ tools and SDK; Windows approval may be required'
    $quoted=((Msvc-Arguments $installation $manifest $channel)|ForEach-Object {if($_ -match '["\r\n]'){throw 'Invalid installer argument'};'"'+$_+'"'}) -join ' '
    $admin=([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
    try{
        if($admin){$process=Start-Process -FilePath $setup -ArgumentList $quoted -Wait -PassThru -NoNewWindow}
        else{$process=Start-Process -FilePath $setup -ArgumentList $quoted -Wait -PassThru -Verb RunAs}
    }catch{throw ('Windows build-tools approval was cancelled or unavailable: '+$_.Exception.Message)}
    if($process.ExitCode -eq 3010){throw '[restart-required] Windows requires a restart before continuing; verified dependencies will be reused.'}
    if($process.ExitCode -ne 0){throw ('Microsoft build-tools installation failed: '+$process.ExitCode)}
    for($attempt=0;$attempt -lt 30;$attempt++){$captured=Vs-Environment $installation;if($captured){return $captured};Start-Sleep -Seconds 2}
    throw 'Installed Microsoft tools did not pass developer environment verification'
}
function Assert-Probes($Tools,$Environment,[string]$CudaRoot,[string]$Prefix) {
    $cmake=Capture $Tools.cmake @('--version');if($cmake -notmatch 'cmake version 3\.31\.8'){throw 'Unexpected managed CMake version'}
    $ninja=Capture $Tools.ninja @('--version');if($ninja -ne '1.13.1'){throw 'Unexpected managed Ninja version'}
    $nvcc=Capture $Tools.nvcc @('--version');if($nvcc -notmatch 'release 13\.4,'){throw 'Selected CUDA 13.4 toolkit is unavailable'}
    $targets=Capture $Tools.nvcc @('--help');if($targets -notmatch '\bsm_120a\b' -or $targets -notmatch '\bcompute_120a\b'){throw 'Selected CUDA compiler lacks sm_120a'}
    $probe=Join-Path $BuildRoot 'msvc-probe.cpp'
    [IO.File]::WriteAllText($probe,"#include <span>`n#if _MSC_VER != 1944`n#error Wrong MSVC generation`n#endif`nint main(){int a[1]{};std::span<int> s{a};return int(s.size())-1;}",$Utf8)
    try{Run $Tools.cl @('/nologo','/std:c++20','/Zc:__cplusplus','/Zs',$probe)}finally{Remove-Item -LiteralPath $probe -Force}
    foreach($p in @('include/cuda.h','include/cuda_runtime.h','include/cublas_v2.h','include/nvrtc.h','include/nvml.h','include/cccl/cuda/std/span')){
        if(!(Test-Path -LiteralPath (Join-Path $CudaRoot $p))){throw ('Incomplete CUDA toolkit: '+$p)}
    }
    foreach($p in @('include/libavformat/avformat.h','include/curl/curl.h','lib/avformat.lib','lib/avcodec.lib','lib/avutil.lib','lib/swscale.lib','share/curl/CURLConfig.cmake')){
        if(!(Test-Path -LiteralPath (Join-Path $Prefix $p))){throw ('Incomplete FFmpeg/curl dependencies: '+$p)}
    }
    @{cmake_version=$cmake;ninja_version=$ninja;make_version='not required';cpp_compiler=('MSVC '+$Environment.VCToolsVersion);nvcc_version=$nvcc;pkg_config_version='not required';system_dependencies=@{}}
}
function Trim-Downloads([string]$Directory,[long]$Limit) {
    if(!(Test-Path -LiteralPath $Directory)){return}
    $files=@(Get-ChildItem -LiteralPath $Directory -File|Where-Object {$_.Name -match '^[a-f0-9]{64}\.(zip|exe|vsman)$'}|Sort-Object LastWriteTime)
    [long]$total=0;foreach($file in $files){$total+=$file.Length}
    foreach($file in $files){if($total -le $Limit){break};Assert-NotLink $file.FullName;Remove-Item -LiteralPath $file.FullName -Force;$total-=$file.Length}
}
function Test-Provisioner {
    $tmp=Join-Path ([IO.Path]::GetTempPath()) ('scala-source-test-'+[guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory $tmp|Out-Null
    try{
        $failed=$false;try{Assert-Under $tmp (Join-Path $tmp '../escape')|Out-Null}catch{$failed=$true};if(!$failed){throw 'Traversal was accepted'}
        $failed=$false;try{Assert-Asset ([pscustomobject]@{id='x';url='http://github.com/x';sha256=('a'*64);size=1})}catch{$failed=$true};if(!$failed){throw 'HTTP was accepted'}
        $zipPath=Join-Path $tmp 'unsafe.zip';$zip=[IO.Compression.ZipFile]::Open($zipPath,[IO.Compression.ZipArchiveMode]::Create);$zip.CreateEntry('../outside')|Out-Null;$zip.Dispose()
        $failed=$false;try{Expand-Safe $zipPath (Join-Path $tmp 'expanded')}catch{$failed=$true};if(!$failed){throw 'Unsafe zip accepted'}
        $safe=Join-Path $tmp 'safe.zip';$zip=[IO.Compression.ZipFile]::Open($safe,[IO.Compression.ZipArchiveMode]::Create)
        $e=$zip.CreateEntry('dir/tool.txt');$w=New-Object IO.StreamWriter($e.Open());$w.Write('fixture');$w.Dispose();$zip.Dispose()
        Expand-Safe $safe (Join-Path $tmp 'ok');if((Get-Content (Join-Path $tmp 'ok/dir/tool.txt') -Raw) -ne 'fixture'){throw 'Safe zip failed'}
        $script:Lock=[pscustomobject]@{vs_product='BuildTools';vs_components=@('C++','SDK')}
        $a=Msvc-Arguments 'C:\Build Tools' 'C:\manifest' 'C:\channel'
        if($a -notcontains '--channelUri' -or $a -notcontains '--norestart' -or $a -contains '--includeRecommended' -or $a -notcontains 'SDK'){throw 'Invalid installer plan'}
        $script:Root=$tmp;$script:DownloadRoot=Join-Path $tmp 'downloads';New-Item -ItemType Directory $DownloadRoot|Out-Null
        $sha=Hash $safe;$asset=[pscustomobject]@{id='fixture';url='https://github.com/example/fixture.zip';sha256=$sha;size=(Get-Item $safe).Length;root=''}
        Copy-Item $safe (Join-Path $DownloadRoot ($sha+'.zip'))
        $one=Package $asset;$two=Package $asset;if($one -ne $two -or @($one).Count -ne 1){throw 'Cache did not return one stable path'}
        [IO.File]::WriteAllText((Join-Path $one 'dir/tool.txt'),'damaged',$Utf8);$three=Package $asset
        if((Get-Content (Join-Path $three 'dir/tool.txt') -Raw) -ne 'fixture'){throw 'Damaged extraction not repaired'}
        Remove-Item (Join-Path $DownloadRoot ($sha+'.zip'))
        if((Package $asset) -ne $one){throw 'Verified extracted tool was redownloaded after archive eviction'}
        $vsRoot=Join-Path $tmp 'Build Tools';$vcTools=Join-Path $vsRoot 'VC/Tools/MSVC/14.44.0';$batchDir=Join-Path $vsRoot 'VC/Auxiliary/Build'
        New-Item -ItemType Directory -Force $batchDir,(Join-Path $vcTools 'bin/Hostx64/x64')|Out-Null
        [IO.File]::WriteAllText((Join-Path $vcTools 'bin/Hostx64/x64/cl.exe'),'synthetic compiler, never executed',$Utf8)
        $batch="@echo off`r`nset VCToolsVersion=14.44.0`r`nset WindowsSDKVersion=10.0.26100.0\`r`nset VCToolsInstallDir=$vcTools\`r`nexit /b 0`r`n"
        [IO.File]::WriteAllText((Join-Path $batchDir 'vcvarsall.bat'),$batch,$Utf8)
        $script:Lock=[pscustomobject]@{msvc_version='14.44';sdk_version='10.0.26100.0'}
        $captured=Vs-Environment $vsRoot;if(!$captured -or $captured.VCToolsVersion -ne '14.44.0'){throw 'Developer environment capture failed'}
        [IO.File]::WriteAllText((Join-Path $batchDir 'vcvarsall.bat'),$batch.Replace('10.0.26100.0','10.0.22621.0'),$Utf8)
        if(Vs-Environment $vsRoot){throw 'Incompatible SDK reused'}
        $cached=Join-Path $DownloadRoot (('a'*64)+'.zip');[IO.File]::WriteAllText($cached,'disposable',$Utf8)
        Trim-Downloads $DownloadRoot 0;if(Test-Path $cached){throw 'Download cap did not reclaim archive'}
        Write-Output 'Native provisioning self-test: 11 passed (no downloads, installations or compiled-code execution)'
    }finally{Remove-Item -LiteralPath $tmp -Recurse -Force}
}
if($SelfTest){Test-Provisioner;exit 0}
try{
    if(!$Root -or !$ResultPath -or !$BuildRoot -or !$DownloadRoot -or $BundleSha256 -notmatch '^[a-f0-9]{64}$' -or $EnvironmentSha256 -notmatch '^[a-f0-9]{64}$'){throw 'Missing managed installation inputs'}
    $Root=[IO.Path]::GetFullPath($Root).TrimEnd('\','/');Assert-NotLink $Root;Assert-NotLink $DownloadRoot;Assert-NotLink $BuildRoot
    if((Get-Content -LiteralPath (Join-Path $Root '.scala-cache-owner') -Raw) -ne $EnvironmentSha256){throw 'Dependency cache has no matching Scala ownership'}
    Assert-Under ([IO.Path]::GetDirectoryName($Root)) $DownloadRoot|Out-Null
    Assert-Under (Join-Path $env:LOCALAPPDATA 'Scala/NativeBuild') $BuildRoot|Out-Null
    $script:Lock=Get-Content -LiteralPath (Join-Path $PSScriptRoot 'windows.lock.json') -Raw|ConvertFrom-Json
    if($Lock.schema_version -ne 1){throw 'Unknown dependency lock schema'}
    foreach($asset in $Lock.assets){Assert-Asset $asset}
    [Net.ServicePointManager]::SecurityProtocol=[Net.SecurityProtocolType]::Tls12
    New-Item -ItemType Directory -Force $DownloadRoot,$BuildRoot|Out-Null
    foreach($dir in @('packages','vcpkg-downloads','vcpkg-cache')){New-Item -ItemType Directory -Force (Join-Path $Root $dir)|Out-Null}
    $env:PATH=$env:SystemRoot+'\System32;'+$env:SystemRoot+';'+$env:SystemRoot+'\System32\WindowsPowerShell\v1.0'
    $gitRoot=Package ($Lock.assets|Where-Object id -eq 'git');$cmakeRoot=Package ($Lock.assets|Where-Object id -eq 'cmake');$ninjaRoot=Package ($Lock.assets|Where-Object id -eq 'ninja')
    $git=Join-Path $gitRoot 'cmd/git.exe';$cmake=Join-Path $cmakeRoot 'bin/cmake.exe';$ninja=Join-Path $ninjaRoot 'ninja.exe'
    $env:PATH=[IO.Path]::GetDirectoryName($git)+';'+[IO.Path]::GetDirectoryName($cmake)+';'+$ninjaRoot+';'+$env:PATH
    $vs=Ensure-Msvc
    foreach($key in @('PATH','INCLUDE','LIB','LIBPATH','VSINSTALLDIR','VCINSTALLDIR','VCToolsInstallDir','VCToolsRedistDir','VCToolsVersion','WindowsSdkDir','WindowsSDKVersion','WindowsSDKLibVersion','UniversalCRTSdkDir','UCRTVersion','VSCMD_ARG_TGT_ARCH','VSCMD_ARG_HOST_ARCH')){if($vs[$key]){Set-Item -Path ('Env:'+$key) -Value $vs[$key]}}
    $cudaRoot=Prepare-Cuda;$nvcc=Join-Path $cudaRoot 'bin/nvcc.exe';$env:CUDA_PATH=$cudaRoot
    $env:PATH=[IO.Path]::GetDirectoryName($nvcc)+';'+(Join-Path $cudaRoot 'bin/x64')+';'+[IO.Path]::GetDirectoryName($git)+';'+[IO.Path]::GetDirectoryName($cmake)+';'+$ninjaRoot+';'+$env:PATH
    $env:VCPKG_ROOT=Join-Path $Root 'vcpkg';$env:VCPKG_TARGET_TRIPLET='x64-windows';$env:VCPKG_DEFAULT_TRIPLET='x64-windows'
    $env:VCPKG_DOWNLOADS=Join-Path $Root 'vcpkg-downloads';$env:VCPKG_DEFAULT_BINARY_CACHE=Join-Path $Root 'vcpkg-cache'
    $env:VCPKG_DISABLE_METRICS='1';$env:VCPKG_MAX_CONCURRENCY=[string][Math]::Min(8,[Environment]::ProcessorCount)
    $env:VCPKG_VISUAL_STUDIO_PATH=$vs.VSINSTALLDIR.TrimEnd('\')
    $env:GIT_CONFIG_NOSYSTEM='1';$env:GIT_CONFIG_GLOBAL='NUL';$env:GIT_TERMINAL_PROMPT='0'
    if(!(Test-Path -LiteralPath (Join-Path $env:VCPKG_ROOT '.git'))){
        New-Item -ItemType Directory -Force $env:VCPKG_ROOT|Out-Null
        Progress 'Acquiring pinned vcpkg recipes';Run $git @('init','--quiet',$env:VCPKG_ROOT)
        Run $git @('-C',$env:VCPKG_ROOT,'remote','add','origin','https://github.com/microsoft/vcpkg.git')
    }
    Run $git @('-C',$env:VCPKG_ROOT,'config','--local','core.longpaths','true')
    Run $git @('-C',$env:VCPKG_ROOT,'config','--local','core.autocrlf','false')
    try{$head=Capture $git @('-C',$env:VCPKG_ROOT,'rev-parse','HEAD')}catch{$head=''}
    if($head -ne $Lock.vcpkg_commit){
        Run $git @('-C',$env:VCPKG_ROOT,'fetch','--quiet','--depth=1','origin',$Lock.vcpkg_commit)
        Run $git @('-C',$env:VCPKG_ROOT,'-c','core.autocrlf=false','-c','core.eol=lf','checkout','--quiet','--detach','FETCH_HEAD')
    }
    if((Capture $git @('-C',$env:VCPKG_ROOT,'rev-parse','HEAD^{tree}')) -ne $Lock.vcpkg_tree){throw 'Pinned vcpkg tree changed'}
    Run $git @('-C',$env:VCPKG_ROOT,'restore','--source=HEAD','--worktree','--','scripts','ports','triplets','versions')
    if(Capture $git @('-C',$env:VCPKG_ROOT,'diff','--name-only','HEAD','--','scripts','ports','triplets','versions')){throw 'Cached vcpkg recipes were modified'}
    if(Capture $git @('-C',$env:VCPKG_ROOT,'ls-files','--others','--exclude-standard','--','scripts','ports','triplets','versions')){throw 'Unexpected untracked vcpkg recipes'}
    $vcpkg=Join-Path $env:VCPKG_ROOT 'vcpkg.exe';$toolAsset=$Lock.assets|Where-Object id -eq 'vcpkg-tool'
    if(!(Test-Path $vcpkg) -or (Hash $vcpkg) -ne $toolAsset.sha256){Copy-Item -LiteralPath (Download $toolAsset) -Destination $vcpkg -Force}
    $manifestRoot=Join-Path $Root 'dependency-manifest';New-Item -ItemType Directory -Force $manifestRoot|Out-Null
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'vcpkg.json') -Destination (Join-Path $manifestRoot 'vcpkg.json') -Force
    $dependencyBuild=Join-Path $BuildRoot 'deps'
    Progress 'Installing or reusing FFmpeg/curl dependencies in the private short build directory'
    Run $vcpkg @('install','--disable-metrics','--triplet=x64-windows',('--x-manifest-root='+$manifestRoot),('--x-install-root='+(Join-Path $env:VCPKG_ROOT 'installed')),('--x-buildtrees-root='+(Join-Path $dependencyBuild 'buildtrees')),('--x-packages-root='+(Join-Path $dependencyBuild 'packages')),'--clean-after-build')
    $prefix=Join-Path $env:VCPKG_ROOT 'installed/x64-windows'
    $tools=@{git=$git;cmake=$cmake;ninja=$ninja;nvcc=$nvcc;cl=(Join-Path $vs.VCToolsInstallDir 'bin/Hostx64/x64/cl.exe')}
    $toolchain=Assert-Probes $tools $vs $cudaRoot $prefix
    $toolchain.system_dependencies=@{bundle_sha256=$BundleSha256;environment_sha256=$EnvironmentSha256;cuda_release=[string]$Lock.cuda_release;windows_sdk=[string]$Lock.sdk_version;vcpkg_commit=[string]$Lock.vcpkg_commit;vcpkg_tree=[string]$Lock.vcpkg_tree;vcpkg_version=(Capture $vcpkg @('version','--disable-metrics'));vcpkg_status_sha256=(Hash (Join-Path $env:VCPKG_ROOT 'installed/vcpkg/status'));msvc_installation=[string]$vs.VSINSTALLDIR}
    foreach($asset in $Lock.assets){$toolchain.system_dependencies['download.'+$asset.id+'.sha256']=[string]$asset.sha256}
    $crtRoot=Join-Path $vs.VCToolsRedistDir 'x64/Microsoft.VC143.CRT'
    if(!(Test-Path -LiteralPath $crtRoot)){throw 'Selected Microsoft CRT redistributable files missing'}
    $runtimeFiles=@(Get-ChildItem -LiteralPath (Join-Path $prefix 'bin') -Filter '*.dll' -File|Select-Object -ExpandProperty FullName)
    $runtimeFiles+=@(Get-ChildItem -LiteralPath $crtRoot -Filter '*.dll' -File|Select-Object -ExpandProperty FullName)
    foreach($glob in @('cublas*.dll','nvrtc*.dll','cudart*.dll')){$runtimeFiles+=@(Get-ChildItem -LiteralPath (Join-Path $cudaRoot 'bin') -Filter $glob -Recurse -File|Select-Object -ExpandProperty FullName)}
    foreach($needed in @('avformat*.dll','avcodec*.dll','avutil*.dll','swscale*.dll','libcurl*.dll','msvcp140.dll','vcruntime140.dll','cublas64*.dll')){
        if(!($runtimeFiles|Where-Object {[IO.Path]::GetFileName($_) -like $needed})){throw ('Missing runtime dependency '+$needed)}
    }
    $environment=@{}
    foreach($key in @('PATH','INCLUDE','LIB','LIBPATH','VSINSTALLDIR','VCINSTALLDIR','VCToolsInstallDir','VCToolsRedistDir','VCToolsVersion','WindowsSdkDir','WindowsSDKVersion','WindowsSDKLibVersion','UniversalCRTSdkDir','UCRTVersion','VSCMD_ARG_TGT_ARCH','VSCMD_ARG_HOST_ARCH','CUDA_PATH','VCPKG_ROOT','VCPKG_TARGET_TRIPLET','VCPKG_DEFAULT_TRIPLET','VCPKG_DISABLE_METRICS','GIT_CONFIG_NOSYSTEM','GIT_CONFIG_GLOBAL','GIT_TERMINAL_PROMPT')){$value=[Environment]::GetEnvironmentVariable($key);if($value){$environment[$key]=$value}}
    Write-Json $ResultPath ([ordered]@{schema_version=1;bundle_sha256=$BundleSha256;tools=$tools;cuda_root=$cudaRoot;dependency_prefix=$prefix;toolchain=$toolchain;environment=$environment;runtime_files=@($runtimeFiles|Sort-Object -Unique)})
    Progress 'All native dependencies verified; reusable tools retained, build scratch removed'
}catch{Write-Error $_ -ErrorAction Continue;exit 1}
finally{
    if($BuildRoot -and (Test-Path -LiteralPath (Join-Path $BuildRoot 'deps'))){Remove-Item -LiteralPath (Join-Path $BuildRoot 'deps') -Recurse -Force -ErrorAction SilentlyContinue}
    if($DownloadRoot){try{Trim-Downloads $DownloadRoot 2GB}catch{Progress ('Download cache cleanup: '+$_.Exception.Message)}}
}
