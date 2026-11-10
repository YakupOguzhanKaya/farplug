# Install the usbridge client on Windows (x64 or ARM64). Installs per user
# without admin rights; only the usbip-win2 driver, if missing, asks for
# administrator approval.
#
#   irm https://raw.githubusercontent.com/YakupOguzhanKaya/usbridge/master/install/install-client.ps1 | iex
#
# Uninstall (usbip-win2 stays; remove it under Settings > Apps if you want):
#
#   & ([scriptblock]::Create((irm https://raw.githubusercontent.com/YakupOguzhanKaya/usbridge/master/install/install-client.ps1))) -Uninstall
#
# For a private repository, set a token with 'repo' scope first:
#   $env:GITHUB_TOKEN = 'ghp_...'
# and fetch the script itself with: irm -Headers @{Authorization="Bearer $env:GITHUB_TOKEN"} <url>
param([switch]$Uninstall)

$ErrorActionPreference = 'Stop'
# The progress bar makes Invoke-WebRequest many times slower on Windows PowerShell 5.
$ProgressPreference = 'SilentlyContinue'

$Repo = if ($env:USBRIDGE_REPO) { $env:USBRIDGE_REPO } else { 'YakupOguzhanKaya/usbridge' }
# usbridge talks to the usbip-win2 driver directly, so pin the version it was built against.
$UsbipVersion = if ($env:USBIP_WIN2_VERSION) { $env:USBIP_WIN2_VERSION } else { '0.9.8.1' }

$Dir = Join-Path $env:LOCALAPPDATA 'Programs\usbridge'
$DataDir = Join-Path $env:LOCALAPPDATA 'usbridge'
$StartMenuLink = Join-Path ([Environment]::GetFolderPath('Programs')) 'usbridge.lnk'
$StartupLink = Join-Path ([Environment]::GetFolderPath('Startup')) 'usbridge tray.lnk'

# Exit code of a console program, without PowerShell 5 turning its stderr
# into terminating errors under ErrorActionPreference=Stop.
function Invoke-Quiet([string]$Exe, [string[]]$Arguments) {
    (Start-Process -FilePath $Exe -ArgumentList $Arguments -Wait -PassThru -WindowStyle Hidden).ExitCode
}

function Stop-Usbridge {
    Get-Process -Name usbridge-gui, usbridge -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -and $_.Path.StartsWith($Dir, [StringComparison]::OrdinalIgnoreCase) } |
        Stop-Process -Force
    Start-Sleep -Milliseconds 500
}

function Set-UserPath([bool]$Present) {
    $path = [Environment]::GetEnvironmentVariable('Path', 'User')
    $parts = @(if ($path) { $path -split ';' | Where-Object { $_ -and $_ -ne $Dir } })
    if ($Present) { $parts += $Dir }
    [Environment]::SetEnvironmentVariable('Path', ($parts -join ';'), 'User')
}

function New-Shortcut([string]$Link, [string]$Target, [string]$Arguments) {
    New-Item -ItemType Directory -Force -Path (Split-Path $Link) | Out-Null
    $s = (New-Object -ComObject WScript.Shell).CreateShortcut($Link)
    $s.TargetPath = $Target
    $s.Arguments = $Arguments
    $s.WorkingDirectory = $Dir
    $s.Save()
}

if ($Uninstall) {
    $cli = Join-Path $Dir 'usbridge.exe'
    if (Test-Path $cli) {
        # Let the agent detach cleanly before it is killed.
        Invoke-Quiet $cli @('detach', '--all') | Out-Null
        Start-Sleep -Seconds 2
    }
    Stop-Usbridge
    Remove-Item -Force -ErrorAction SilentlyContinue $StartMenuLink, $StartupLink
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $Dir, $DataDir
    Set-UserPath $false
    Write-Host 'usbridge removed. usbip-win2 is still installed; remove it under Settings > Apps if you no longer need it.'
    return
}

$arch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
switch ($arch) {
    'AMD64' { $Target = 'x86_64-pc-windows-msvc'; $UsbipArch = 'x64' }
    'ARM64' { $Target = 'aarch64-pc-windows-msvc'; $UsbipArch = 'arm64' }
    default { throw "unsupported architecture: $arch (usbridge needs 64-bit Windows)" }
}

$headers = @{ 'User-Agent' = 'usbridge-installer' }
if ($env:GITHUB_TOKEN) { $headers.Authorization = "Bearer $env:GITHUB_TOKEN" }

$tag = $env:USBRIDGE_VERSION
if (-not $tag) {
    try {
        $tag = (Invoke-RestMethod -Headers $headers "https://api.github.com/repos/$Repo/releases/latest").tag_name
    } catch {
        throw "could not find the latest usbridge release; is the repository private? Set `$env:GITHUB_TOKEN. ($_)"
    }
}
$asset = "usbridge-$Target.zip"
Write-Host "installing usbridge $tag ($Target)"

$tmp = Join-Path ([IO.Path]::GetTempPath()) ("usbridge-" + [Guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    $zip = Join-Path $tmp $asset
    if ($env:GITHUB_TOKEN) {
        # Private release assets are only downloadable through the API.
        $release = Invoke-RestMethod -Headers $headers "https://api.github.com/repos/$Repo/releases/tags/$tag"
        $a = $release.assets | Where-Object name -eq $asset
        if (-not $a) { throw "asset $asset not found in $tag" }
        Invoke-WebRequest -Headers ($headers + @{ Accept = 'application/octet-stream' }) -OutFile $zip $a.url
    } else {
        Invoke-WebRequest -Headers $headers -OutFile $zip "https://github.com/$Repo/releases/download/$tag/$asset"
    }
    Expand-Archive -Path $zip -DestinationPath $tmp
    $src = Join-Path $tmp "usbridge-$Target"

    Stop-Usbridge
    New-Item -ItemType Directory -Force -Path $Dir | Out-Null
    Copy-Item -Force (Join-Path $src '*.exe') $Dir
    Set-UserPath $true
    $env:Path = "$env:Path;$Dir"

    $cli = Join-Path $Dir 'usbridge.exe'
    $gui = Join-Path $Dir 'usbridge-gui.exe'

    if ((Invoke-Quiet $cli @('check')) -ne 0) {
        $setup = "USBip-$UsbipVersion-$UsbipArch.exe"
        Write-Host "installing the usbip-win2 $UsbipVersion driver; approve the administrator prompt"
        $setupPath = Join-Path $tmp $setup
        Invoke-WebRequest -Headers @{ 'User-Agent' = 'usbridge-installer' } -OutFile $setupPath `
            "https://github.com/vadimgrn/usbip-win2/releases/download/v.$UsbipVersion/$setup"
        $p = Start-Process -FilePath $setupPath -ArgumentList '/SILENT', '/NORESTART' -Verb RunAs -Wait -PassThru
        if ($p.ExitCode -ne 0) { throw "the usbip-win2 installer failed (exit code $($p.ExitCode))" }
        if ((Invoke-Quiet $cli @('check')) -ne 0) {
            Write-Warning 'usbip-win2 is installed but its driver is not running yet; restart Windows, then open usbridge.'
        }
    }

    New-Shortcut $StartMenuLink $gui ''
    New-Shortcut $StartupLink $gui '--tray'
    # The tray starts the agent, which keeps devices attached in the background.
    Start-Process -FilePath $gui -ArgumentList '--tray'
} finally {
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $tmp
}

Write-Host ''
Write-Host "installed to $Dir"
Write-Host 'The usbridge tray icon is running and starts at every login.'
Write-Host 'Open "usbridge" from the Start menu to add a server, or in a new terminal:'
Write-Host '  usbridge --server <server-host> list'
Write-Host '  usbridge --server <server-host> attach <device-id>'
