$ErrorActionPreference = "Stop"

$repository = if ($env:STRUCTURELY_REPOSITORY) { $env:STRUCTURELY_REPOSITORY } else { "coder-company/structurely" }
$version = if ($env:STRUCTURELY_VERSION) { $env:STRUCTURELY_VERSION } else { "latest" }
$installDir = if ($env:STRUCTURELY_INSTALL_DIR) {
    $env:STRUCTURELY_INSTALL_DIR
} else {
    Join-Path ([Environment]::GetFolderPath("LocalApplicationData")) "Programs\Structurely\bin"
}

$useColor = [Environment]::UserInteractive -and
    -not [Console]::IsOutputRedirected -and
    [string]::IsNullOrWhiteSpace($env:NO_COLOR) -and
    [string]::IsNullOrWhiteSpace($env:STRUCTURELY_NO_COLOR)

function Write-StyledLine {
    param(
        [Parameter(Mandatory = $true)][string]$Text,
        [ConsoleColor]$Color = [ConsoleColor]::Gray
    )
    if ($useColor) {
        Write-Host $Text -ForegroundColor $Color
    } else {
        Write-Output $Text
    }
}

function Write-Step {
    param([int]$Number, [string]$Label)
    Write-StyledLine "[$Number/4] $Label" Cyan
}

function Write-Detail {
    param([string]$Text)
    Write-StyledLine "      $Text" DarkGray
}

function Write-Verified {
    param([string]$Text)
    Write-StyledLine "      verified $Text" Green
}

function Get-ReleaseFile {
    param([string]$Uri, [string]$OutFile)
    $lastError = $null
    foreach ($attempt in 1..3) {
        try {
            Invoke-WebRequest -UseBasicParsing -Uri $Uri -OutFile $OutFile
            return
        } catch {
            $lastError = $_
            if ($attempt -lt 3) { Start-Sleep -Seconds $attempt }
        }
    }
    throw "Could not download $Uri after three attempts: $($lastError.Exception.Message)"
}

Write-Output ""
Write-StyledLine "  Structurely" White
Write-StyledLine "  Local-first code intelligence for coding agents" DarkGray
Write-Output ""

$architecture = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
if ($architecture -ne "X64") {
    throw "No native Structurely release is available for Windows/$architecture."
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
    Write-Step 1 "Detect platform"
    Write-Detail "Target       windows/x86_64"
    Write-Detail "Destination  $(Join-Path $installDir 'structurely.exe')"
    Write-Detail "Release      $version"

    Write-Step 2 "Download release"
    Write-Detail $asset
    $archive = Join-Path $temporary $asset
    $checksums = Join-Path $temporary "SHA256SUMS"
    Get-ReleaseFile "$releaseUrl/$asset" $archive
    Get-ReleaseFile "$releaseUrl/SHA256SUMS" $checksums

    Write-Step 3 "Verify and stage"
    $escapedAsset = [Regex]::Escape($asset)
    $checksumLine = Get-Content $checksums | Where-Object { $_ -match "^[0-9a-fA-F]{64}\s+\*?$escapedAsset$" } | Select-Object -First 1
    if (-not $checksumLine) {
        throw "SHA256SUMS does not contain $asset. The existing installation was not changed."
    }
    $expected = ($checksumLine -split "\s+")[0].ToLowerInvariant()
    $actual = (Get-FileHash -Algorithm SHA256 $archive).Hash.ToLowerInvariant()
    if ($actual -ne $expected) {
        throw "Checksum verification failed for $asset. The existing installation was not changed."
    }
    Write-Verified "SHA-256 checksum"

    $package = Join-Path $temporary "package"
    Expand-Archive -Path $archive -DestinationPath $package
    $binary = Join-Path $package "structurely.exe"
    if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) {
        throw "The verified release archive does not contain a regular Structurely binary."
    }
    $installedVersion = (& $binary --version | Out-String).Trim()
    if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($installedVersion)) {
        throw "The downloaded binary could not start on this machine."
    }
    Write-Verified "$installedVersion starts correctly"

    $destination = Join-Path $installDir "structurely.exe"
    $previousVersion = ""
    if (Test-Path -LiteralPath $destination -PathType Leaf) {
        try { $previousVersion = (& $destination --version | Out-String).Trim() } catch { $previousVersion = "" }
    }

    Write-Step 4 "Install atomically"
    New-Item -ItemType Directory -Force -Path $installDir | Out-Null
    $staged = Join-Path $installDir ("structurely.new." + $PID + ".exe")
    Copy-Item $binary $staged
    Move-Item -Force $staged $destination
    if ($previousVersion -and $previousVersion -ne $installedVersion) {
        Write-Detail "Updated      $previousVersion -> $installedVersion"
    } elseif ($previousVersion) {
        Write-Detail "Reinstalled  $installedVersion"
    } else {
        Write-Detail "Installed    $installedVersion"
    }
    Write-Detail "Binary       $destination"

    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    $segments = @($userPath -split ";" | Where-Object { $_ })
    if ($segments -notcontains $installDir) {
        $newPath = (@($segments) + $installDir) -join ";"
        [Environment]::SetEnvironmentVariable("Path", $newPath, "User")
        $pathUpdated = $true
    } else {
        $pathUpdated = $false
    }
    if (($env:Path -split ";") -notcontains $installDir) {
        $env:Path = "$installDir;$env:Path"
    }

    Write-Output ""
    Write-StyledLine "  Structurely is ready." Green
    if ($pathUpdated) {
        Write-Output ""
        Write-StyledLine "  PATH updated" White
        Write-Detail "Open a new terminal before using structurely elsewhere."
    }
    Write-Output ""
    Write-StyledLine "  Start in a repository" White
    Write-Output "    cd your-project"
    Write-Output "    structurely setup codex"

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
        Write-Output ""
        Write-StyledLine "  Optional private dashboard" White
        Write-Detail "The shell may be hosted; repository data always stays on this machine."
        Write-Output ""
        Write-Output "    1  Local only        Start it yourself when needed"
        Write-Output "    2  Cloudflare Pages  Deploy the static shell"
        Write-Output "    3  Vercel           Deploy the static shell"
        Write-Output "    4  Not now          Finish installation"
        Write-Output ""
        $choice = Read-Host "  Choose [1-4, default 4]"
        $dashboardSetup = switch ($choice.ToLowerInvariant()) {
            { $_ -in @("1", "local") } { "local"; break }
            { $_ -in @("2", "cloudflare") } { "cloudflare"; break }
            { $_ -in @("3", "vercel") } { "vercel"; break }
            default { "skip" }
        }
    }

    switch ($dashboardSetup) {
        { $_ -in @("cloudflare", "vercel") } {
            Write-Output ""
            Write-StyledLine "  Dashboard deployment" White
            Write-Detail "Provider      $dashboardSetup"
            Write-Detail "Upload        Static shell only; no repository data"
            Write-Detail "Requirement   Authenticated provider CLI already installed"
            try {
                & $destination dashboard deploy $dashboardSetup
                $dashboardStatus = $LASTEXITCODE
            } catch {
                $dashboardStatus = 1
                Write-Warning "Dashboard command could not run: $($_.Exception.Message)"
            }
            if ($dashboardStatus -eq 0) {
                Write-Verified "dashboard deployment"
            } else {
                Write-Warning "Dashboard deployment failed (exit $dashboardStatus); Structurely remains installed."
                Write-Detail "Retry with: structurely dashboard deploy $dashboardSetup"
            }
            break
        }
        "local" {
            Write-Output ""
            Write-StyledLine "  Dashboard ready for local use" White
            Write-Output "    structurely dashboard serve"
            break
        }
        "skip" { break }
        default {
            Write-Warning "Ignoring STRUCTURELY_DASHBOARD_SETUP=$dashboardSetup; expected cloudflare, vercel, local, skip, or prompt."
        }
    }
    Write-Output ""
} finally {
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $temporary
}
