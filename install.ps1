# Hermes Desktop installer for Windows.
#
#   irm https://raw.githubusercontent.com/j4ckxyz/hermes-desktop/main/install.ps1 | iex
#
# Downloads the latest release (or $env:HERMES_VERSION, e.g. v0.6.1) from GitHub Releases, checks
# it against the release's SHA256SUMS, installs it to %LOCALAPPDATA%\Programs\Hermes Desktop and
# adds a Start Menu shortcut. No administrator rights are needed and nothing is launched.
& {
    $ErrorActionPreference = 'Stop'
    $ProgressPreference = 'SilentlyContinue'
    $repo = 'j4ckxyz/hermes-desktop'
    $asset = 'hermes-desktop-windows-x86_64.zip'
    $version = if ($env:HERMES_VERSION) { $env:HERMES_VERSION } else { 'latest' }
    $base = if ($version -eq 'latest') { "https://github.com/$repo/releases/latest/download" } else { "https://github.com/$repo/releases/download/$version" }

    if (-not [Environment]::Is64BitOperatingSystem) { throw 'Hermes Desktop needs 64-bit Windows.' }
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

    $tmp = Join-Path ([IO.Path]::GetTempPath()) ("hermes-desktop-" + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $tmp | Out-Null
    try {
        Write-Host "==> Downloading $asset ($version)"
        $zip = Join-Path $tmp $asset
        Invoke-WebRequest -UseBasicParsing -Uri "$base/$asset" -OutFile $zip
        $sums = (Invoke-WebRequest -UseBasicParsing -Uri "$base/SHA256SUMS").Content
        if ($sums -is [byte[]]) { $sums = [Text.Encoding]::UTF8.GetString($sums) }

        if ($env:HERMES_TEST_CORRUPT) { Add-Content -Path $zip -Value 'x' }  # CI: prove a bad file is refused

        $line = ($sums -split "`n") | Where-Object { $_ -match "^\s*([0-9a-fA-F]{64})\s+\*?$([regex]::Escape($asset))\s*$" } | Select-Object -First 1
        if (-not $line) { throw "$asset is not listed in SHA256SUMS" }
        $expected = ($line.Trim() -split '\s+')[0].ToLower()
        $actual = (Get-FileHash -Algorithm SHA256 -Path $zip).Hash.ToLower()
        if ($actual -ne $expected) { throw "Checksum mismatch for $asset (expected $expected, got $actual); nothing was installed." }
        Write-Host '==> Checksum verified'

        $dest = Join-Path $env:LOCALAPPDATA 'Programs\Hermes Desktop'
        $running = Get-Process -Name 'hermes-desktop' -ErrorAction SilentlyContinue
        if ($running) { throw 'Hermes Desktop is running. Quit it and run the installer again.' }
        if (Test-Path $dest) { Remove-Item -Recurse -Force $dest }
        New-Item -ItemType Directory -Force -Path $dest | Out-Null
        Expand-Archive -Path $zip -DestinationPath $dest -Force
        $exe = Join-Path $dest 'hermes-desktop.exe'
        if (-not (Test-Path $exe)) { throw 'The archive has no hermes-desktop.exe.' }
        # Downloaded with PowerShell, so there is no mark-of-the-web to clear; make sure anyway.
        Get-ChildItem -Recurse $dest | Unblock-File

        $programs = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs'
        $shell = New-Object -ComObject WScript.Shell
        $lnk = $shell.CreateShortcut((Join-Path $programs 'Hermes Desktop.lnk'))
        $lnk.TargetPath = $exe
        $lnk.WorkingDirectory = $dest
        $lnk.Description = 'A fast native desktop client for Hermes Agent'
        $lnk.Save()

        $v = (& $exe --version | Out-String).Trim()
        Write-Host "==> Installed $v to $dest"
        Write-Host '==> Start it from the Start Menu ("Hermes Desktop").'
    }
    finally {
        Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
    }
}
