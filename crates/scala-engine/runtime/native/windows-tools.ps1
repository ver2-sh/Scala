# Shared Windows acquisition primitives. Executed only by explicit Scala Install recipes.
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
function Shared-Root([string]$Kind) {
    if($Kind -notin @('packages','toolkits')){throw 'Unknown shared native store'}
    $folder=Join-Path ([IO.Path]::GetDirectoryName($Root)) $Kind
    Assert-NotLink $folder
    $owner=Join-Path $folder '.scala-shared-native-owner'
    $identity='scala-native-shared-'+$Kind+'-v1'
    if(Test-Path -LiteralPath $folder){
        if(!(Test-Path -LiteralPath $owner) -or (Get-Content -LiteralPath $owner -Raw) -ne $identity){throw 'Refusing unowned shared native store'}
        Assert-NotLink $owner
    }else{
        New-Item -ItemType Directory $folder|Out-Null
        [IO.File]::WriteAllText($owner,$identity,$Utf8)
    }
    $folder
}
function Package($Asset) {
    Assert-Asset $Asset
    $folder=Join-Path (Shared-Root 'packages') $Asset.sha256;Assert-NotLink $folder
    $legacy=Join-Path $Root ('packages/'+$Asset.id)
    if(!(Test-Path -LiteralPath $folder) -and (Valid-Inventory $legacy $Asset.sha256)){Move-Item -LiteralPath $legacy -Destination $folder}
    if(!(Valid-Inventory $folder $Asset.sha256)){
        $archive=Download $Asset
        if(Test-Path -LiteralPath $folder){Remove-Item -LiteralPath $folder -Recurse -Force}
        Progress ('Extracting '+$Asset.id);Expand-Safe $archive $folder
        Write-Json (Join-Path $folder 'scala-extraction.json') ([ordered]@{sha256=$Asset.sha256;files=(Inventory $folder)})
    }else{Progress ('Reusing verified '+$Asset.id)}
    if($Asset.root){Join-Path $folder $Asset.root}else{$folder}
}
function Prepare-Cuda {
    $cudaAssets=@($Lock.assets|Where-Object kind -eq 'cuda')
    $input=($cudaAssets|ForEach-Object {$_.id+':'+$_.sha256}) -join ';'
    $sha=[Security.Cryptography.SHA256]::Create()
    try{$key=[BitConverter]::ToString($sha.ComputeHash($Utf8.GetBytes($input))).Replace('-','').ToLowerInvariant()}finally{$sha.Dispose()}
    $cudaRoot=Join-Path (Shared-Root 'toolkits') $key;Assert-NotLink $cudaRoot
    $legacy=Join-Path $Root 'cuda'
    if(!(Test-Path -LiteralPath $cudaRoot) -and (Valid-Inventory $legacy $key)){Move-Item -LiteralPath $legacy -Destination $cudaRoot}
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

function Trim-Downloads([string]$Directory,[long]$Limit) {
    if(!(Test-Path -LiteralPath $Directory)){return}
    $files=@(Get-ChildItem -LiteralPath $Directory -File|Where-Object {$_.Name -match '^[a-f0-9]{64}\.(zip|exe|vsman)$'}|Sort-Object LastWriteTime)
    [long]$total=0;foreach($file in $files){$total+=$file.Length}
    foreach($file in $files){if($total -le $Limit){break};Assert-NotLink $file.FullName;Remove-Item -LiteralPath $file.FullName -Force;$total-=$file.Length}
}
