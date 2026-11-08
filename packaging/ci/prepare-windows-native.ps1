param([switch]$ReleaseOnly)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$projectRoot = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$nativeRoot = Join-Path $projectRoot 'target/native-deps/ci'
New-Item -ItemType Directory -Force -Path $nativeRoot | Out-Null

function Get-VerifiedDownload([string]$Url, [string]$Destination, [string]$Sha256) {
    if (-not (Test-Path -LiteralPath $Destination)) {
        $partial = "$Destination.download"
        Invoke-WebRequest -Uri $Url -OutFile $partial -MaximumRetryCount 3 -RetryIntervalSec 3
        if ((Get-FileHash -Algorithm SHA256 -LiteralPath $partial).Hash.ToLowerInvariant() -ne $Sha256) {
            throw "Unexpected SHA-256 for $Url"
        }
        Move-Item -LiteralPath $partial -Destination $Destination
    }
    if ((Get-FileHash -Algorithm SHA256 -LiteralPath $Destination).Hash.ToLowerInvariant() -ne $Sha256) {
        throw "Unexpected cached SHA-256: $Destination"
    }
}

function Export-CiEnvironment([string]$Name, [string]$Value) {
    [Environment]::SetEnvironmentVariable($Name, $Value, 'Process')
    if ($env:GITHUB_ENV) {
        "$Name=$Value" | Out-File -FilePath $env:GITHUB_ENV -Encoding utf8 -Append
    }
}

$publishedInstaller = Join-Path $nativeRoot 'yt-ultimate_5.0.0_x64_full-setup.exe'
Get-VerifiedDownload `
    'https://github.com/ElWapoteDev/yt-ultimate-releases/releases/download/v5.0.0/yt-ultimate_5.0.0_x64_full-setup.exe' `
    $publishedInstaller '39a0a4ffc8222af8909ad35f6b3d49a376afd1e5de8290d5d38c00151299568c'
Export-CiEnvironment 'YTU_PUBLISHED_INSTALLER' $publishedInstaller
if ($ReleaseOnly) { return }

$sevenZip = (Get-Command 7z -ErrorAction Stop).Source
$modelRoot = Join-Path $nativeRoot 'release-model'
$model = Join-Path $modelRoot 'models/bs-roformer-v1.pt'
if (-not (Test-Path -LiteralPath $model)) {
    & $sevenZip x -y $publishedInstaller "-o$modelRoot" 'models/bs-roformer-v1.pt'
    if ($LASTEXITCODE -ne 0) { throw 'Could not extract the pinned model from the published installer' }
}
if ((Get-FileHash -Algorithm SHA256 -LiteralPath $model).Hash.ToLowerInvariant() -ne '31d794ab054bd70964b6165ceca3b94e00f344f8a0b8bc325def47ce9aa14926') {
    throw 'Unexpected BS-RoFormer model hash'
}
Export-CiEnvironment 'YTU_PACKAGE_STEMS_MODEL' $model

$torchArchive = Join-Path $nativeRoot 'libtorch-win-2.12.0-cpu.zip'
Get-VerifiedDownload `
    'https://download.pytorch.org/libtorch/cpu/libtorch-win-shared-with-deps-2.12.0%2Bcpu.zip' `
    $torchArchive '8f8f018ca1cf372158916192dc1e97219983c047cdcd7cd9b11d21e64a2e93bf'
$torchRoot = Join-Path $nativeRoot 'libtorch'
if (-not (Test-Path -LiteralPath (Join-Path $torchRoot 'lib/torch_cpu.dll'))) {
    & $sevenZip x -y $torchArchive "-o$nativeRoot" 'libtorch/include/*' 'libtorch/lib/*' 'libtorch/build-version' '-x!libtorch/lib/torch_python*'
    if ($LASTEXITCODE -ne 0) { throw 'Could not extract the official Windows LibTorch distribution' }
}
Export-CiEnvironment 'LIBTORCH' $torchRoot

$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
$visualStudio = (& $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath).Trim()
if ($LASTEXITCODE -ne 0 -or -not $visualStudio) { throw 'Visual Studio C++ runtime files were not found' }
$crtCandidates = @(Get-Item -Path (Join-Path $visualStudio 'VC/Redist/MSVC/*/x64/Microsoft.VC143.CRT'))
if ($crtCandidates.Count -eq 0) { throw 'The x64 Microsoft VC143 redistributable is missing' }
$crtSource = ($crtCandidates | Sort-Object FullName -Descending | Select-Object -First 1).FullName
$crtDestination = Join-Path $nativeRoot 'vc-runtime-x64'
New-Item -ItemType Directory -Force -Path $crtDestination | Out-Null
foreach ($name in @('msvcp140.dll', 'vcruntime140.dll', 'vcruntime140_1.dll')) {
    Copy-Item -LiteralPath (Join-Path $crtSource $name) -Destination (Join-Path $crtDestination $name) -Force
}
Export-CiEnvironment 'YTU_VC_RUNTIME_DIR' $crtDestination
if ($env:GITHUB_PATH) {
    (Join-Path $torchRoot 'lib') | Out-File -FilePath $env:GITHUB_PATH -Encoding utf8 -Append
}
Write-Output 'Prepared SHA-256 verified Windows LibTorch, model and app-local CRT for native Full builds.'
