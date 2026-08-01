param(
    [Parameter(Mandatory = $true)][string]$Binary
)

$ErrorActionPreference = "Stop"
$binaryPath = (Resolve-Path $Binary).Path
$repositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$temporary = Join-Path ([System.IO.Path]::GetTempPath()) ("structurely-windows-installer-" + [Guid]::NewGuid())
$release = Join-Path $temporary "release"
$package = Join-Path $temporary "package"
$install = Join-Path $temporary "install"
$server = $null
$savedEnvironment = @{}

function Set-TestEnvironment {
    param([string]$Name, [AllowNull()][string]$Value)
    if (-not $savedEnvironment.ContainsKey($Name)) {
        $savedEnvironment[$Name] = [Environment]::GetEnvironmentVariable($Name, "Process")
    }
    [Environment]::SetEnvironmentVariable($Name, $Value, "Process")
}

try {
    New-Item -ItemType Directory -Path $release, $package | Out-Null
    Copy-Item $binaryPath (Join-Path $package "structurely.exe")
    Copy-Item (Join-Path $repositoryRoot "README.md"), (Join-Path $repositoryRoot "LICENSE") $package
    $asset = "structurely-windows-x86_64.zip"
    $archive = Join-Path $release $asset
    Compress-Archive -Path (Join-Path $package "*") -DestinationPath $archive
    $digest = (Get-FileHash -Algorithm SHA256 $archive).Hash.ToLowerInvariant()
    Set-Content -NoNewline -Path (Join-Path $release "SHA256SUMS") -Value "$digest  $asset`n"

    $listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0)
    $listener.Start()
    $port = ([System.Net.IPEndPoint]$listener.LocalEndpoint).Port
    $listener.Stop()
    $python = (Get-Command python -ErrorAction Stop).Source
    $server = Start-Process -FilePath $python -ArgumentList @(
        "-m", "http.server", "$port", "--bind", "127.0.0.1", "--directory", $release
    ) -WindowStyle Hidden -PassThru
    $baseUrl = "http://127.0.0.1:$port"
    foreach ($attempt in 1..50) {
        try {
            Invoke-WebRequest -UseBasicParsing "$baseUrl/SHA256SUMS" | Out-Null
            break
        } catch {
            if ($attempt -eq 50) { throw "Local release server did not become ready." }
            Start-Sleep -Milliseconds 100
        }
    }

    Set-TestEnvironment "STRUCTURELY_RELEASE_BASE_URL" $baseUrl
    Set-TestEnvironment "STRUCTURELY_INSTALL_DIR" $install
    Set-TestEnvironment "STRUCTURELY_DASHBOARD_SETUP" "skip"
    Set-TestEnvironment "STRUCTURELY_NO_COLOR" "1"
    Set-TestEnvironment "CI" "true"

    $output = (& (Join-Path $repositoryRoot "install.ps1") | Out-String)
    foreach ($stage in @(
        "[1/4] Detect platform",
        "[2/4] Download release",
        "[3/4] Verify and stage",
        "[4/4] Install atomically"
    )) {
        if (-not $output.Contains($stage)) { throw "Installer output omitted stage: $stage" }
    }
    foreach ($contract in @("verified SHA-256 checksum", "Structurely is ready.", "structurely setup codex")) {
        if (-not $output.Contains($contract)) { throw "Installer output omitted: $contract" }
    }
    if ($output.Contains([char]27)) { throw "Non-interactive output contained ANSI escapes." }

    $destination = Join-Path $install "structurely.exe"
    if (-not (Test-Path -LiteralPath $destination -PathType Leaf)) {
        throw "Installer did not publish structurely.exe."
    }
    $expectedVersion = (& $binaryPath --version | Out-String).Trim()
    $actualVersion = (& $destination --version | Out-String).Trim()
    if ($actualVersion -ne $expectedVersion) {
        throw "Installed version '$actualVersion' did not match '$expectedVersion'."
    }

    $installedDigest = (Get-FileHash -Algorithm SHA256 $destination).Hash
    Set-Content -NoNewline -Path (Join-Path $release "SHA256SUMS") -Value "$('0' * 64)  $asset`n"
    $rejected = $false
    try {
        & (Join-Path $repositoryRoot "install.ps1") | Out-Null
    } catch {
        $rejected = $_.Exception.Message.Contains("Checksum verification failed")
    }
    if (-not $rejected) { throw "Installer accepted a release with an invalid checksum." }
    if ((Get-FileHash -Algorithm SHA256 $destination).Hash -ne $installedDigest) {
        throw "A rejected update changed the existing installation."
    }

    Write-Output "Windows installer round-trip and checksum preservation passed"
} finally {
    if ($server -and -not $server.HasExited) {
        Stop-Process -Id $server.Id -Force -ErrorAction SilentlyContinue
        $server.WaitForExit()
    }
    foreach ($entry in $savedEnvironment.GetEnumerator()) {
        [Environment]::SetEnvironmentVariable($entry.Key, $entry.Value, "Process")
    }
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $temporary
}
