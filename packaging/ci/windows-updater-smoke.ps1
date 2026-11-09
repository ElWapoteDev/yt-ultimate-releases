# Companion to windows-installed-smoke.ps1; runs only against its disposable
# installation. The payload is the already-built installer, not a fake download.
function Invoke-InstalledUpdateHandoff {
    param(
        [Parameter(Mandatory = $true)][Diagnostics.Process]$Application,
        [Parameter(Mandatory = $true)][string]$Executable,
        [Parameter(Mandatory = $true)][string]$Installer,
        [Parameter(Mandatory = $true)][string]$Profile,
        [Parameter(Mandatory = $true)][string]$EvidenceDirectory
    )
    if ($env:GITHUB_ACTIONS -ne 'true') { throw 'Updater handoff testing requires a disposable Actions runner' }
    $name = [IO.Path]::GetFileName($Installer)
    if ($name -notmatch '^yt-ultimate_(\d+\.\d+\.\d+)_x64_full-setup\.exe$') { throw 'Unexpected Full installer filename' }
    $version = $Matches[1]
    $cache = Join-Path $Profile 'ci-update-cache'
    $transaction = Join-Path $cache "install-$([Guid]::NewGuid().ToString('N'))"
    New-Item -ItemType Directory -Path $transaction -Force | Out-Null
    $payload = Join-Path $cache $name
    try { New-Item -ItemType HardLink -Path $payload -Target $Installer -ErrorAction Stop | Out-Null }
    catch { Copy-Item -LiteralPath $Installer -Destination $payload }
    # Rust canonicalize uses the extended Win32 spelling, including correct case.
    if (-not ('YtuSmokeNativePaths' -as [type])) {
        Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using Microsoft.Win32.SafeHandles;
public static class YtuSmokeNativePaths {
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern uint GetFinalPathNameByHandle(SafeFileHandle file, StringBuilder path, uint size, uint flags);
    public static string Canonical(string path) {
        using (var file = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.ReadWrite | FileShare.Delete)) {
            var buffer = new StringBuilder(32768);
            var length = GetFinalPathNameByHandle(file.SafeFileHandle, buffer, (uint)buffer.Capacity, 0);
            if (length == 0) throw new Win32Exception(Marshal.GetLastWin32Error());
            if (length >= buffer.Capacity) throw new IOException("Canonical path exceeded the buffer");
            return buffer.ToString();
        }
    }
}
'@
    }
    $lockPath = Join-Path $transaction 'parent.lock'
    $parentLock = [IO.File]::Open($lockPath, [IO.FileMode]::CreateNew, [IO.FileAccess]::ReadWrite, [IO.FileShare]::ReadWrite)
    $parentLock.Lock(0, [long]::MaxValue)
    $manifest = [ordered]@{
        # A local test supplies the previous version; the executable under test
        # remains the new build. No GitHub request or release mutation occurs.
        current_version = '0.0.0'
        repository = 'ElWapoteDev/yt-ultimate-releases'
        edition = 'Full'
        cache = $cache
        executable = [YtuSmokeNativePaths]::Canonical($Executable)
        installer = $payload
        update = [ordered]@{
            version = $version
            name = $name
            download_url = "https://github.com/ElWapoteDev/yt-ultimate-releases/releases/download/v$version/$name"
            release_url = "https://github.com/ElWapoteDev/yt-ultimate-releases/releases/tag/v$version"
            sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $payload).Hash.ToLowerInvariant()
            size = (Get-Item -LiteralPath $payload).Length
            platform = 'WindowsX64'
            edition = 'Full'
        }
        restart = $true
        parent_pid = $Application.Id
    }
    $manifestPath = Join-Path $transaction 'install.json'
    $manifest | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $manifestPath -Encoding utf8NoBOM
    $helper = $null
    $restarted = $null
    $completed = $false
    $evidence = [ordered]@{ acknowledged = $false; remained_waiting_with_parent_lock = $false; waited_for_parent_after_unlock = $false; installer_handoff = $false; restarted_window = $false; profile_reused = $false }
    try {
        $helper = New-IsolatedProcess $Executable @('--yt-apply-update', $manifestPath)
        $stdout = $helper.StandardOutput.ReadToEndAsync()
        $stderr = $helper.StandardError.ReadToEndAsync()
        $deadline = [DateTime]::UtcNow.AddSeconds(20)
        while (-not (Test-Path -LiteralPath (Join-Path $transaction 'ready'))) {
            if ($helper.HasExited -or [DateTime]::UtcNow -ge $deadline) { throw 'Installed update helper did not acknowledge the transaction' }
            Start-Sleep -Milliseconds 100
        }
        $evidence.acknowledged = $true
        Start-Sleep -Milliseconds 500
        if ($helper.HasExited -or (Test-Path -LiteralPath (Join-Path $cache 'last-install.json'))) { throw 'Helper did not wait for its parent lock' }
        $evidence.remained_waiting_with_parent_lock = $true
        $parentLock.Dispose()
        $parentLock = $null
        Start-Sleep -Milliseconds 500
        $Application.Refresh()
        if ($Application.HasExited -or $helper.HasExited -or (Test-Path -LiteralPath (Join-Path $cache 'last-install.json'))) { throw 'Helper did not wait for the live application after releasing the lock' }
        $evidence.waited_for_parent_after_unlock = $true
        if (-not $Application.CloseMainWindow() -or -not $Application.WaitForExit(30000) -or $Application.ExitCode -ne 0) { throw 'Application did not close normally before updating' }
        if (-not $helper.WaitForExit(30000) -or $helper.ExitCode -ne 0) { throw 'Installed helper failed to hand off the verified installer' }
        $outcome = Get-Content -LiteralPath (Join-Path $cache 'last-install.json') -Raw | ConvertFrom-Json
        if (-not $outcome.success) { throw 'Helper reported a failed installation handoff' }
        $evidence.installer_handoff = $true
        $deadline = [DateTime]::UtcNow.AddSeconds(180)
        do {
            $candidates = @(Get-CimInstance Win32_Process -Filter "Name='YT Ultimate.exe'" | Where-Object { $_.ExecutablePath -eq $Executable -and $_.ProcessId -ne $Application.Id -and $_.ProcessId -ne $helper.Id })
            foreach ($candidate in $candidates) {
                try {
                    $process = [Diagnostics.Process]::GetProcessById($candidate.ProcessId)
                    if (-not $process.HasExited -and $process.MainWindowHandle -ne [IntPtr]::Zero) { $restarted = $process; break }
                    $process.Dispose()
                } catch [ArgumentException] { }
            }
            if ($null -eq $restarted) { Start-Sleep -Milliseconds 500 }
        } until ($null -ne $restarted -or [DateTime]::UtcNow -ge $deadline)
        if ($null -eq $restarted) { throw 'NSIS did not restart the installed application after the helper handoff' }
        $evidence.restarted_window = $true
        $deadline = [DateTime]::UtcNow.AddSeconds(45)
        $profileReady = $false
        do {
            if ($restarted.HasExited) { throw 'Updated application exited during observation' }
            $logPath = Join-Path $Profile 'Logs/application.log'
            if (Test-Path -LiteralPath $logPath) {
                $log = Get-Content -LiteralPath $logPath -Raw
                $profileReady = @([regex]::Matches($log, 'Started YT Ultimate')).Count -ge 2 -and (Test-Path -LiteralPath (Join-Path $Profile 'yt-ultimate.sqlite3'))
            }
            if (-not $profileReady) { Start-Sleep -Milliseconds 500 }
        } until ($profileReady -or [DateTime]::UtcNow -ge $deadline)
        if (-not $profileReady) { throw 'Restart did not reuse the isolated application profile and database' }
        $evidence.profile_reused = $true
        $completed = $true
        return $restarted
    } finally {
        if ($null -ne $parentLock) { $parentLock.Dispose() }
        if ($null -ne $helper) {
            if (-not $helper.HasExited) { $helper.Kill($true); $helper.WaitForExit() }
            $stdout.Result | Out-File -LiteralPath (Join-Path $EvidenceDirectory 'updater-helper.stdout.log') -Encoding utf8
            $stderr.Result | Out-File -LiteralPath (Join-Path $EvidenceDirectory 'updater-helper.stderr.log') -Encoding utf8
            $helper.Dispose()
        }
        if (-not $completed) {
            # Only processes in this test's unique installation/cache are ours.
            foreach ($candidate in @(Get-CimInstance Win32_Process | Where-Object { $_.ExecutablePath -eq $Executable -or $_.ExecutablePath -eq $payload })) {
                try {
                    $process = [Diagnostics.Process]::GetProcessById($candidate.ProcessId)
                    [void]$process.CloseMainWindow()
                    if (-not $process.WaitForExit(3000)) { $process.Kill($true); $process.WaitForExit() }
                    $process.Dispose()
                } catch [ArgumentException] { }
            }
        }
        $evidence | ConvertTo-Json | Out-File -LiteralPath (Join-Path $EvidenceDirectory 'updater-handoff.json') -Encoding utf8
    }
}
