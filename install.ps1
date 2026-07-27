$ErrorActionPreference = "Stop"

$repository = if ($env:STRUCTURELY_REPOSITORY) {
    $env:STRUCTURELY_REPOSITORY
} else {
    "https://github.com/coder-company/structurely"
}

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw @"
Structurely currently installs through Cargo, but cargo was not found.
Install Rust from https://rustup.rs and run this installer again.
"@
}

$arguments = @(
    "install",
    "--locked",
    "--force",
    "--git",
    $repository
)

if ($env:STRUCTURELY_VERSION) {
    $arguments += @("--tag", $env:STRUCTURELY_VERSION)
}

$arguments += "structurely"

Write-Host "Installing Structurely from $repository"
& cargo @arguments
if ($LASTEXITCODE -ne 0) {
    throw "cargo install failed with exit code $LASTEXITCODE"
}

Write-Host "Structurely installed successfully."
Write-Host "Run: structurely --help"

