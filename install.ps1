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
} finally {
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $temporary
}
