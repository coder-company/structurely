$ErrorActionPreference = "Stop"

$repository = if ($env:STRUCTURELY_REPOSITORY) { $env:STRUCTURELY_REPOSITORY } else { "coder-company/structurely" }
$version = if ($env:STRUCTURELY_VERSION) { $env:STRUCTURELY_VERSION } else { "latest" }
$installDir = if ($env:STRUCTURELY_INSTALL_DIR) {
    $env:STRUCTURELY_INSTALL_DIR
} else {
    Join-Path ([Environment]::GetFolderPath("LocalApplicationData")) "Programs\Structurely\bin"
}

$architecture = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
if ($architecture -ne "X64") {
    throw "Structurely does not yet publish a native Windows $architecture binary."
}

$asset = "structurely-windows-x86_64.zip"
if ($env:STRUCTURELY_RELEASE_BASE_URL) {
    $releaseUrl = $env:STRUCTURELY_RELEASE_BASE_URL.TrimEnd("/")
} elseif ($version -eq "latest") {
    $releaseUrl = "https://github.com/$repository/releases/latest/download"
} else {
    $tag = if ($version.StartsWith("v")) { $version } else { "v$version" }
    $releaseUrl = "https://github.com/$repository/releases/download/$tag"
}

$temporary = Join-Path ([System.IO.Path]::GetTempPath()) ("structurely-install-" + [Guid]::NewGuid())
New-Item -ItemType Directory -Path $temporary | Out-Null
try {
    Write-Host "Downloading Structurely $version for Windows/x86_64..."
    $archive = Join-Path $temporary $asset
    $checksums = Join-Path $temporary "SHA256SUMS"
    Invoke-WebRequest -UseBasicParsing -Uri "$releaseUrl/$asset" -OutFile $archive
    Invoke-WebRequest -UseBasicParsing -Uri "$releaseUrl/SHA256SUMS" -OutFile $checksums

    $escapedAsset = [Regex]::Escape($asset)
    $checksumLine = Get-Content $checksums | Where-Object { $_ -match "^[0-9a-fA-F]{64}\s+\*?$escapedAsset$" } | Select-Object -First 1
    if (-not $checksumLine) {
        throw "SHA256SUMS does not contain $asset."
    }
    $expected = ($checksumLine -split "\s+")[0].ToLowerInvariant()
    $actual = (Get-FileHash -Algorithm SHA256 $archive).Hash.ToLowerInvariant()
    if ($actual -ne $expected) {
        throw "Checksum verification failed for $asset."
    }

    $package = Join-Path $temporary "package"
    Expand-Archive -Path $archive -DestinationPath $package
    $binary = Get-ChildItem -Path $package -Filter "structurely.exe" -File -Recurse | Select-Object -First 1
    if (-not $binary) {
        throw "The release archive does not contain structurely.exe."
    }
    & $binary.FullName --version | Out-Null

    New-Item -ItemType Directory -Force -Path $installDir | Out-Null
    $destination = Join-Path $installDir "structurely.exe"
    $staged = Join-Path $installDir ("structurely.new." + $PID + ".exe")
    Copy-Item $binary.FullName $staged
    Move-Item -Force $staged $destination

    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    $segments = @($userPath -split ";" | Where-Object { $_ })
    if ($segments -notcontains $installDir) {
        $newPath = (@($segments) + $installDir) -join ";"
        [Environment]::SetEnvironmentVariable("Path", $newPath, "User")
        Write-Host "Added $installDir to your user PATH. Open a new terminal to use it."
    }
    if (($env:Path -split ";") -notcontains $installDir) {
        $env:Path = "$installDir;$env:Path"
    }

    Write-Host "Installed $(& $destination --version)"
    Write-Host "Binary: $destination"
    Write-Host ""
    Write-Host "Next: cd your-project; structurely setup codex"

    $dashboardSetup = $env:STRUCTURELY_DASHBOARD_SETUP
    if (-not $dashboardSetup) {
        $isCi = -not [string]::IsNullOrWhiteSpace($env:CI)
        $isInteractive = [Environment]::UserInteractive -and
            -not [Console]::IsInputRedirected -and
            -not [Console]::IsOutputRedirected
        $dashboardSetup = if ($isCi -or -not $isInteractive) { "skip" } else { "prompt" }
    }
    $dashboardSetup = $dashboardSetup.ToLowerInvariant()

    if ($dashboardSetup -eq "prompt") {
        Write-Host ""
        Write-Host "Optional private dashboard (repository data remains on this machine):"
        Write-Host "  1) Deploy static shell to Cloudflare Pages"
        Write-Host "  2) Deploy static shell to Vercel"
        Write-Host "  3) Use locally only"
        Write-Host "  4) Skip"
        $choice = Read-Host "Choose [1-4, default 4]"
        $dashboardSetup = switch ($choice.ToLowerInvariant()) {
            { $_ -in @("1", "cloudflare") } { "cloudflare"; break }
            { $_ -in @("2", "vercel") } { "vercel"; break }
            { $_ -in @("3", "local") } { "local"; break }
            default { "skip" }
        }
    }

    switch ($dashboardSetup) {
        { $_ -in @("cloudflare", "vercel") } {
            Write-Host ""
            Write-Host "Deploying the static dashboard shell to $dashboardSetup."
            Write-Host "This installer will not install npm packages or provider CLIs."
            Write-Host "The selected provider CLI must already be installed and authenticated."
            try {
                & $destination dashboard deploy $dashboardSetup
                $dashboardStatus = $LASTEXITCODE
            } catch {
                $dashboardStatus = 1
                Write-Warning "Dashboard command could not run: $($_.Exception.Message)"
            }
            if ($dashboardStatus -eq 0) {
                Write-Host "Dashboard deployment completed."
            } else {
                Write-Warning "Dashboard deployment failed (exit $dashboardStatus); Structurely itself remains installed."
                Write-Host "Retry later: structurely dashboard deploy $dashboardSetup"
            }
            break
        }
        "local" {
            Write-Host ""
            Write-Host "Dashboard selected for local-only use."
            Write-Host "Start it when ready: structurely dashboard serve"
            break
        }
        "skip" { break }
        default {
            Write-Warning "Ignoring invalid STRUCTURELY_DASHBOARD_SETUP=$dashboardSetup (expected cloudflare, vercel, local, skip, or prompt)."
        }
    }
} finally {
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $temporary
}
