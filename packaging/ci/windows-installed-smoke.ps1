param(
    [Parameter(Mandatory = $true)][string]$Installer,
    [string]$LegacyInstaller,
    [string]$StemsProbe,
    [string]$QualityProbe,
    [switch]$ExerciseUpdater,
    [ValidateSet('yt-ultimate.exe', 'YT Ultimate.exe')][string]$InstalledExecutableName = 'yt-ultimate.exe',
    [string]$ResultsDirectory = 'target/ci-results/windows-installed'
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$Installer = (Resolve-Path -LiteralPath $Installer).Path
$ResultsDirectory = [IO.Path]::GetFullPath($ResultsDirectory)
New-Item -ItemType Directory -Force -Path $ResultsDirectory | Out-Null
$temporaryRoot = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [IO.Path]::GetTempPath() }
$testRoot = Join-Path $temporaryRoot "ytu-smoke-$([Guid]::NewGuid().ToString('N'))"
$installDirectory = Join-Path $testRoot 'App with spaces'
$profilePath = Join-Path $testRoot 'isolated-profile'
$libraryPath = Join-Path $testRoot 'isolated-library'
$legacyRegistryKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\d5cdec52-ef34-5136-a778-fe870ed73415'
if ($LegacyInstaller) {
    if ($env:GITHUB_ACTIONS -ne 'true') { throw 'Legacy upgrade testing is restricted to a disposable GitHub Actions runner' }
    $LegacyInstaller = (Resolve-Path -LiteralPath $LegacyInstaller).Path
    if (Test-Path -LiteralPath $legacyRegistryKey) { throw 'Refusing to disturb an existing Electron installation' }
    $profilePath = Join-Path $env:APPDATA 'YT Ultimate'
    if (Test-Path -LiteralPath $profilePath) { throw 'Refusing to replace an existing default profile' }
}
$registryKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\YT Ultimate'
if (Test-Path -LiteralPath $registryKey) { throw 'Refusing to disturb an existing YT Ultimate installation' }
foreach ($association in @('YTUltimate.Audio', 'YTUltimate.Video', 'YTUltimate.Playlist')) {
    if (Test-Path -LiteralPath "HKCU:\Software\Classes\$association") { throw "An existing association must not be overwritten by this test: $association" }
}
New-Item -ItemType Directory -Force -Path $testRoot, $profilePath, $libraryPath | Out-Null
$result = [ordered]@{
    installer = $Installer
    installer_sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $Installer).Hash.ToLowerInvariant()
    installed_executable_name = $InstalledExecutableName
    os = [Environment]::OSVersion.VersionString
    legacy_upgrade_requested = [bool]$LegacyInstaller
    legacy_installation_completed = $false
    legacy_profile_migrated = $false
    default_profile_discovered = $false
    shortcuts_verified = $false
    profile_preserved_after_uninstall = $false
    installation_completed = $false
    isolated_gui_started = $false
    webview2_child_started = $false
    database_created = $false
    single_instance_handoff = $null
    compatibility_executable_verified = $null
    associations_verified = $false
    cpu_stems_passed = $false
    cpu_quality_passed = $false
    graceful_exit = $false
    updater_handoff_passed = $null
    uninstall_completed = $false
}
$appProcess = $null
$failure = $null

function New-IsolatedProcess([string]$Executable, [string[]]$Arguments = @()) {
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $Executable
    $start.WorkingDirectory = $installDirectory
    $start.UseShellExecute = $false
    foreach ($argument in $Arguments) { $start.ArgumentList.Add($argument) }
    foreach ($key in @('LIBTORCH', 'LIBTORCH_USE_PYTORCH', 'LIBTORCH_BYPASS_VERSION_CHECK', 'LIBCLANG_PATH', 'PYTHONPATH', 'PYTHONHOME', 'CARGO_MANIFEST_DIR', 'DYLD_LIBRARY_PATH')) {
        [void]$start.Environment.Remove($key)
    }
    $start.Environment['PATH'] = "$env:WINDIR\System32;$env:WINDIR"
    if ($LegacyInstaller) { [void]$start.Environment.Remove('YTU_DATA_DIR') } else { $start.Environment['YTU_DATA_DIR'] = $profilePath }
    $start.Environment['YTU_LIBRARY_DIR'] = $libraryPath
    $start.Environment['OMP_NUM_THREADS'] = '2'
    $start.Environment['MKL_NUM_THREADS'] = '2'
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    return [Diagnostics.Process]::Start($start)
}

function Invoke-InstalledProbe([string]$Source, [string]$Name, [string[]]$Arguments) {
    $destination = Join-Path $installDirectory "$Name.exe"
    Copy-Item -LiteralPath (Resolve-Path -LiteralPath $Source).Path -Destination $destination
    $probe = New-IsolatedProcess $destination $Arguments
    $stdout = $probe.StandardOutput.ReadToEndAsync()
    $stderr = $probe.StandardError.ReadToEndAsync()
    try {
        if (-not $probe.WaitForExit(300000)) { $probe.Kill($true); throw "$Name exceeded five minutes" }
        $stdout.Result | Out-File -LiteralPath (Join-Path $ResultsDirectory "$Name.stdout.log") -Encoding utf8
        $stderr.Result | Out-File -LiteralPath (Join-Path $ResultsDirectory "$Name.stderr.log") -Encoding utf8
        if ($probe.ExitCode -ne 0) { throw "$Name failed with exit code $($probe.ExitCode): $($stderr.Result)" }
        Write-Output $stdout.Result
    } finally {
        if (-not $probe.HasExited) { $probe.Kill($true); $probe.WaitForExit() }
        Remove-Item -LiteralPath $destination -Force
        $probe.Dispose()
    }
}

function Invoke-LegacyFixture([string]$Phase) {
    python packaging/ci/legacy-profile.py $Phase --profile $profilePath --library $libraryPath --evidence $ResultsDirectory
    if ($LASTEXITCODE -ne 0) { throw "Legacy profile verification failed: $Phase" }
}

try {
    if ($LegacyInstaller) {
        $legacySetup = Start-Process -FilePath $LegacyInstaller -ArgumentList "/S /D=$installDirectory" -PassThru
        if (-not $legacySetup.WaitForExit(300000)) { $legacySetup.Kill($true); throw 'Legacy installation timed out' }
        if ($legacySetup.ExitCode -ne 0) { throw "Legacy installation failed: $($legacySetup.ExitCode)" }
        if (-not (Test-Path -LiteralPath (Join-Path $installDirectory 'YT Ultimate.exe'))) { throw 'Legacy executable is missing' }
        if ((Get-ItemPropertyValue -LiteralPath $legacyRegistryKey -Name DisplayVersion) -ne '4.4.1') { throw 'Unexpected legacy version' }
        $result.legacy_display_name = Get-ItemPropertyValue -LiteralPath $legacyRegistryKey -Name DisplayName
        $result.legacy_installation_directory = Get-ItemPropertyValue -LiteralPath 'HKCU:\Software\d5cdec52-ef34-5136-a778-fe870ed73415' -Name InstallLocation
        $result.legacy_installation_completed = $true
        Invoke-LegacyFixture 'seed'
    }
    # NSIS requires /D to be last and unquoted; the remainder is the path.
    $setupArguments = if ($LegacyInstaller) { '/S' } else { "/S /D=$installDirectory" }
    $setup = Start-Process -FilePath $Installer -ArgumentList $setupArguments -PassThru
    if (-not $setup.WaitForExit(300000)) { $setup.Kill($true); throw 'Silent installation exceeded five minutes' }
    if ($setup.ExitCode -ne 0) { throw "Silent installation failed: $($setup.ExitCode)" }
    if (Test-Path -LiteralPath $registryKey) {
        $result.registered_installation_directory = Get-ItemPropertyValue -LiteralPath $registryKey -Name InstallLocation
    }
    $executable = Join-Path $installDirectory $InstalledExecutableName
    $model = Join-Path $installDirectory 'models/bs-roformer-v1.pt'
    if (-not (Test-Path -LiteralPath $executable)) { throw 'Installed executable is missing' }
    if ($InstalledExecutableName -eq 'YT Ultimate.exe') {
        $compatibilityExecutable = Join-Path $installDirectory 'yt-ultimate.exe'
        if (-not (Test-Path -LiteralPath $compatibilityExecutable)) { throw 'Rust preview compatibility executable is missing' }
        if ((Get-FileHash -LiteralPath $compatibilityExecutable -Algorithm SHA256).Hash -ne (Get-FileHash -LiteralPath $executable -Algorithm SHA256).Hash) { throw 'Compatibility executable differs from the installed application' }
        $result.compatibility_executable_verified = $true
    }
    if ((Get-FileHash -Algorithm SHA256 -LiteralPath $model).Hash.ToLowerInvariant() -ne '31d794ab054bd70964b6165ceca3b94e00f344f8a0b8bc325def47ce9aa14926') { throw 'Installed model hash mismatch' }
    $result.installation_completed = $true
    if ($LegacyInstaller) {
        foreach ($old in @('Uninstall YT Ultimate.exe', 'resources/app.asar')) {
            if (Test-Path -LiteralPath (Join-Path $installDirectory $old)) { throw "Legacy application file remains: $old" }
        }
        if (Test-Path -LiteralPath $legacyRegistryKey) { throw 'Legacy uninstall registration remains' }
        Invoke-LegacyFixture 'before'
        $shell = New-Object -ComObject WScript.Shell
        $explorer = New-Object -ComObject Shell.Application
        $shortcuts = @(Get-ChildItem -LiteralPath ([Environment]::GetFolderPath('Desktop')), ([Environment]::GetFolderPath('Programs')) -Filter 'YT Ultimate.lnk' -Recurse)
        if ($shortcuts.Count -lt 2) { throw 'Expected desktop and Start Menu shortcuts' }
        foreach ($shortcut in $shortcuts) {
            if ($shell.CreateShortcut($shortcut.FullName).TargetPath -ne $executable) { throw "Shortcut points to a previous application: $($shortcut.FullName)" }
            $item = $explorer.Namespace($shortcut.DirectoryName).ParseName($shortcut.Name)
            if ($item.ExtendedProperty('System.AppUserModel.ID') -ne 'com.ytultimate.desktop') { throw 'Shortcut AppUserModelID changed' }
        }
        $result.shortcuts_verified = $true
    }
    foreach ($association in @('YTUltimate.Audio', 'YTUltimate.Video', 'YTUltimate.Playlist')) {
        $command = (Get-Item -LiteralPath "HKCU:\Software\Classes\$association\shell\open\command").GetValue('')
        if ($command -ne "`"$executable`" `"%1`"") { throw "Incorrect association command: $command" }
    }
    $result.associations_verified = $true
    $appProcess = New-IsolatedProcess $executable
    $appStdout = $appProcess.StandardOutput.ReadToEndAsync()
    $appStderr = $appProcess.StandardError.ReadToEndAsync()
    $deadline = [DateTime]::UtcNow.AddSeconds(45)
    do {
        Start-Sleep -Milliseconds 500
        $appProcess.Refresh()
        if ($appProcess.HasExited) { throw "Installed application exited early: $($appProcess.ExitCode); $($appStderr.Result)" }
        $children = @(Get-CimInstance Win32_Process -Filter "ParentProcessId=$($appProcess.Id)")
        $result.webview2_child_started = @($children | Where-Object Name -eq 'msedgewebview2.exe').Count -gt 0
        $result.database_created = Test-Path -LiteralPath (Join-Path $profilePath 'yt-ultimate.sqlite3')
        $result.isolated_gui_started = $appProcess.MainWindowHandle -ne [IntPtr]::Zero
    } until (($result.isolated_gui_started -and $result.webview2_child_started -and $result.database_created) -or [DateTime]::UtcNow -ge $deadline)
    if (-not ($result.isolated_gui_started -and $result.webview2_child_started -and $result.database_created)) { throw 'Expected window, WebView2 child and isolated SQLite database were not all observed' }
    $result.window_title = $appProcess.MainWindowTitle
    Start-Sleep -Seconds 5
    if ($appProcess.HasExited) { throw 'Application exited during the startup observation period' }
    # The immutable preview predates single-instance support. Require it for
    # newly built source packages and the Electron upgrade path only.
    if ($StemsProbe -or $LegacyInstaller -or $ExerciseUpdater) {
        $secondaryPath = if ($InstalledExecutableName -eq 'YT Ultimate.exe') { $compatibilityExecutable } else { $executable }
        $secondary = New-IsolatedProcess $secondaryPath
        $secondaryStdout = $secondary.StandardOutput.ReadToEndAsync()
        $secondaryStderr = $secondary.StandardError.ReadToEndAsync()
        try {
            if (-not $secondary.WaitForExit(15000)) { throw 'Second instance did not hand off to the primary window' }
            if ($secondary.ExitCode -ne 0) { throw "Second instance failed: $($secondary.ExitCode); $($secondaryStderr.Result)" }
            $appProcess.Refresh()
            if ($appProcess.HasExited) { throw 'Primary instance exited during handoff' }
            $result.single_instance_handoff = $true
        } finally {
            if (-not $secondary.HasExited) { $secondary.Kill($true); $secondary.WaitForExit() }
            $secondaryStdout.Result | Out-File -LiteralPath (Join-Path $ResultsDirectory 'secondary.stdout.log') -Encoding utf8
            $secondaryStderr.Result | Out-File -LiteralPath (Join-Path $ResultsDirectory 'secondary.stderr.log') -Encoding utf8
            $secondary.Dispose()
        }
    }
    if ($ExerciseUpdater) {
        . (Join-Path $PSScriptRoot 'windows-updater-smoke.ps1')
        $previousProcess = $appProcess
        $appProcess = Invoke-InstalledUpdateHandoff -Application $previousProcess -Executable $executable -Installer $Installer -Profile $profilePath -EvidenceDirectory $ResultsDirectory
        $previousProcess.Dispose()
        $result.updater_handoff_passed = $true
    }
    $appProcess.Refresh()
    $finalProcessHandle = $appProcess.Handle
    $result.final_window_handle = $appProcess.MainWindowHandle.ToInt64()
    $result.final_window_title = $appProcess.MainWindowTitle
    if (-not $appProcess.CloseMainWindow()) { throw 'Could not request a normal window close' }
    if (-not $appProcess.WaitForExit(30000)) {
        $appProcess.Refresh()
        $result.window_handle_after_close = $appProcess.MainWindowHandle.ToInt64()
        $result.window_title_after_close = $appProcess.MainWindowTitle
        $result.responding_after_close = $appProcess.Responding
        throw 'Application did not exit after closing its main window'
    }
    $finalExitCode = if ($ExerciseUpdater) { [YtuSmokeNativePaths]::ExitCode($finalProcessHandle) } else { $appProcess.ExitCode }
    $result.final_exit_code = $finalExitCode
    if ($null -eq $finalExitCode -or $finalExitCode -ne 0) { throw "Application closed with exit code $finalExitCode" }
    $result.graceful_exit = $true
    if ($LegacyInstaller) {
        Invoke-LegacyFixture 'after'
        $result.legacy_profile_migrated = $true
        $result.default_profile_discovered = $true
    }
    $appStdout.Result | Out-File -LiteralPath (Join-Path $ResultsDirectory 'app.stdout.log') -Encoding utf8
    $appStderr.Result | Out-File -LiteralPath (Join-Path $ResultsDirectory 'app.stderr.log') -Encoding utf8
    if ($StemsProbe) {
        Invoke-InstalledProbe $StemsProbe 'stems-probe' @($model, '9')
        $result.cpu_stems_passed = $true
    }
    if ($QualityProbe) {
        Invoke-InstalledProbe $QualityProbe 'quality-probe' @($model)
        $result.cpu_quality_passed = $true
    }
} catch {
    $failure = $_
    $result.failure = $_.Exception.ToString()
} finally {
    if ($null -ne $appProcess -and -not $appProcess.HasExited) {
        [void]$appProcess.CloseMainWindow()
        if (-not $appProcess.WaitForExit(10000)) { $appProcess.Kill($true); $appProcess.WaitForExit() }
    }
    try {
        $uninstaller = Join-Path $installDirectory 'uninstall.exe'
        if (Test-Path -LiteralPath $uninstaller) {
            $remove = Start-Process -FilePath $uninstaller -ArgumentList "/S _?=$installDirectory" -PassThru
            if (-not $remove.WaitForExit(120000)) { $remove.Kill($true); throw 'Uninstaller timed out' }
            if ($remove.ExitCode -ne 0) { throw "Uninstaller failed: $($remove.ExitCode)" }
            foreach ($name in @('yt-ultimate.exe', 'YT Ultimate.exe')) {
                if (Test-Path -LiteralPath (Join-Path $installDirectory $name)) { throw "Uninstaller left the application executable: $name" }
            }
            if (Test-Path -LiteralPath $registryKey) { throw 'Uninstaller left the application registration' }
            foreach ($association in @('YTUltimate.Audio', 'YTUltimate.Video', 'YTUltimate.Playlist')) {
                if (Test-Path -LiteralPath "HKCU:\Software\Classes\$association") { throw "Uninstaller left association $association" }
            }
            $result.uninstall_completed = $true
            if ($LegacyInstaller -and $result.legacy_profile_migrated) {
                Invoke-LegacyFixture 'after'
                $result.profile_preserved_after_uninstall = $true
            }
        }
    } catch {
        $result.cleanup_failure = $_.Exception.ToString()
        if ($null -eq $failure) { $failure = $_ }
    }
    try {
        $logs = Join-Path $profilePath 'Logs'
        if (Test-Path -LiteralPath $logs -PathType Container) {
            Copy-Item -LiteralPath $logs -Destination (Join-Path $ResultsDirectory 'application-logs') -Recurse -Force
        }
    } catch {
        $result.log_copy_failure = $_.Exception.ToString()
    }
    $result | ConvertTo-Json -Depth 5 | Out-File -LiteralPath (Join-Path $ResultsDirectory 'result.json') -Encoding utf8
    $result | ConvertTo-Json -Depth 5 | Write-Output
}
if ($null -ne $failure) { throw $failure }
if (-not $result.uninstall_completed) { throw 'Successful installation/uninstallation was not demonstrated' }
