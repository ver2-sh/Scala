#requires -Version 5.1
# Runs only after the user selects Install. No model, driver or global PATH changes.
param([string]$Root, [string]$ResultPath, [string]$BundleSha256,
      [string]$EnvironmentSha256, [string]$DownloadRoot, [string]$BuildRoot, [switch]$SelfTest)
. (Join-Path $PSScriptRoot 'windows-tools.ps1')
function Assert-Probes($Tools,$Environment,[string]$CudaRoot,[string]$Prefix) {
    $cmake=Capture $Tools.cmake @('--version');if($cmake -notmatch 'cmake version 3\.31\.8'){throw 'Unexpected managed CMake version'}
    $ninja=Capture $Tools.ninja @('--version');if($ninja -ne '1.13.1'){throw 'Unexpected managed Ninja version'}
    $nvcc=Capture $Tools.nvcc @('--version');if($nvcc -notmatch 'release 13\.4,'){throw 'Selected CUDA 13.4 toolkit is unavailable'}
    $targets=Capture $Tools.nvcc @('--help');if($targets -notmatch '\bsm_120a\b' -or $targets -notmatch '\bcompute_120a\b'){throw 'Selected CUDA compiler lacks sm_120a'}
    $probe=Join-Path $BuildRoot 'msvc-probe.cpp'
    [IO.File]::WriteAllText($probe,"#include <span>`n#if _MSC_VER != 1944`n#error Wrong MSVC generation`n#endif`nint main(){int a[1]{};std::span<int> s{a};return int(s.size())-1;}",$Utf8)
    try{Run $Tools.cl @('/nologo','/std:c++20','/Zc:__cplusplus','/Zs',$probe)}finally{Remove-Item -LiteralPath $probe -Force}
    foreach($p in @('include/cuda.h','include/cuda_runtime.h','include/cublas_v2.h','include/nvrtc.h','include/nvml.h','include/cccl/cuda/std/span','include/nvtx3/nvToolsExt.h')){
        if(!(Test-Path -LiteralPath (Join-Path $CudaRoot $p))){throw ('Incomplete CUDA toolkit: '+$p)}
    }
    foreach($p in @('include/libavformat/avformat.h','include/curl/curl.h','lib/avformat.lib','lib/avcodec.lib','lib/avutil.lib','lib/swscale.lib','share/curl/CURLConfig.cmake')){
        if(!(Test-Path -LiteralPath (Join-Path $Prefix $p))){throw ('Incomplete FFmpeg/curl dependencies: '+$p)}
    }
    @{cmake_version=$cmake;ninja_version=$ninja;make_version='not required';cpp_compiler=('MSVC '+$Environment.VCToolsVersion);nvcc_version=$nvcc;pkg_config_version='not required';system_dependencies=@{}}
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
        $script:Root=Join-Path $tmp 'environment';New-Item -ItemType Directory $Root|Out-Null;$script:DownloadRoot=Join-Path $tmp 'downloads';New-Item -ItemType Directory $DownloadRoot|Out-Null
        $sha=Hash $safe;$asset=[pscustomobject]@{id='fixture';url='https://github.com/example/fixture.zip';sha256=$sha;size=(Get-Item $safe).Length;root=''}
        Copy-Item $safe (Join-Path $DownloadRoot ($sha+'.zip'))
        $one=Package $asset;$two=Package $asset;if($one -ne $two -or @($one).Count -ne 1){throw 'Cache did not return one stable path'}
        $firstRoot=$Root;$script:Root=Join-Path $tmp 'other-environment';New-Item -ItemType Directory $Root|Out-Null
        $shared=Package $asset
        if($shared -ne $one){throw 'Different runtime dependency sets duplicated an identical tool'}
        $script:Root=$firstRoot
        $different=Join-Path $tmp 'different.zip';$zip=[IO.Compression.ZipFile]::Open($different,[IO.Compression.ZipArchiveMode]::Create)
        $e=$zip.CreateEntry('dir/tool.txt');$w=New-Object IO.StreamWriter($e.Open());$w.Write('different-version');$w.Dispose();$zip.Dispose()
        $otherSha=Hash $different;$other=[pscustomobject]@{id='fixture';url='https://github.com/example/different.zip';sha256=$otherSha;size=(Get-Item $different).Length;root=''}
        Copy-Item $different (Join-Path $DownloadRoot ($otherSha+'.zip'))
        $otherPath=Package $other
        if($otherPath -eq $one -or (Get-Content (Join-Path $one 'dir/tool.txt') -Raw) -ne 'fixture'){throw 'Incompatible tool versions were conflated'}
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
        Write-Output 'Native provisioning self-test: 13 passed (no downloads, installations or compiled-code execution)'
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
    # Header-only NVTX is required by upstream core/nvtx.h. Keep its pin in this
    # fully hashed recipe while reusing the existing compiler/library environment.
    $Lock.assets+=([pscustomobject]@{
        id='cuda_nvtx'
        url='https://developer.download.nvidia.com/compute/cuda/redist/cuda_nvtx/windows-x86_64/cuda_nvtx-windows-x86_64-13.4.92-archive.zip'
        sha256='a3bd5041778b63a95e63009c044e93b4d569b7dca5693d6b12884e04d1d39d06'
        size=153681
        root='cuda_nvtx-windows-x86_64-13.4.92-archive'
        kind='cuda'
    })
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
