# Build helper for this Windows machine.
#
# It sets a workspace-local CARGO_HOME, loads the MSVC environment (so the shell
# does not have to be a "x64 Native Tools" prompt), and forwards the rest of the
# arguments to cargo. `run` additionally builds both binaries first.
#
#   powershell -ExecutionPolicy Bypass -File tools/build.ps1 build
#   powershell -ExecutionPolicy Bypass -File tools/build.ps1 test --lib
#   powershell -ExecutionPolicy Bypass -File tools/build.ps1 run
#
# Network note: cargo reaches crates.io directly. Downloads of large crates can be
# slow enough to trip the 30s-per-crate timeout, so `net.retry` is raised rather
# than routing through a local mirror.

param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]] $CargoArgs
)

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$env:CARGO_HOME = Join-Path $root '.cargo-home'

if (-not $CargoArgs -or $CargoArgs.Count -eq 0) {
    $CargoArgs = @('build')
}

$vcvars = 'C:\Program Files (x86)\Microsoft Visual Studio\18\BuildTools\VC\Auxiliary\Build\vcvars64.bat'
$script:LastExitCode = 0

function Invoke-Cargo([string[]] $Arguments) {
    $quoted = $Arguments | ForEach-Object {
        if ($_ -match '\s') { '"' + $_ + '"' } else { $_ }
    }
    $line = 'cargo ' + ($quoted -join ' ')
    if (Test-Path $vcvars) {
        & cmd /c ('call "' + $vcvars + '" >nul && ' + $line)
    } else {
        Invoke-Expression $line
    }
    $script:LastExitCode = $LASTEXITCODE
}

# `run` must build both binaries first (surf + surf-vst-host), then run the
# UI binary explicitly; plain `cargo run` is ambiguous with two bins.
if ($CargoArgs[0] -eq 'run') {
    $rest = @($CargoArgs | Select-Object -Skip 1)
    $buildArgs = @('build')
    if ($rest -contains '--release') {
        $buildArgs += '--release'
    }
    Invoke-Cargo $buildArgs
    if ($script:LastExitCode -ne 0) {
        exit $script:LastExitCode
    }
    Invoke-Cargo (@('run', '--bin', 'surf') + $rest)
    exit $script:LastExitCode
}

Invoke-Cargo $CargoArgs
exit $script:LastExitCode
