# Builds release executables and packs them into dist\playterm-<version>-windows-<arch>.zip.
#
#   pwsh scripts\package.ps1                 # ARM64 + x64
#   pwsh scripts\package.ps1 -Targets x64    # x64 only
#
# Requires the x64 Rust target (rustup target add x86_64-pc-windows-msvc) and, on ARM64
# hosts, LLVM/clang (winget install LLVM.LLVM) for the crypto library.
param(
    [ValidateSet("arm64", "x64")]
    [string[]]$Targets = @("arm64", "x64")
)
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root

$llvm = "C:\Program Files\LLVM\bin"
if ((Test-Path $llvm) -and ($env:PATH -notlike "*$llvm*")) { $env:PATH += ";$llvm" }

$version = (Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"(.+)"').Matches[0].Groups[1].Value
$triples = @{ arm64 = "aarch64-pc-windows-msvc"; x64 = "x86_64-pc-windows-msvc" }
$dist = Join-Path $root "dist"
New-Item -ItemType Directory -Force $dist | Out-Null

$sums = @()
foreach ($arch in $Targets) {
    $triple = $triples[$arch]
    Write-Host "== $arch ($triple)"
    cargo build --release -p playterm --target $triple
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed for $triple" }

    $stage = Join-Path $dist "playterm-$version-windows-$arch"
    Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
    New-Item -ItemType Directory $stage | Out-Null
    Copy-Item "target\$triple\release\playterm.exe", "README.md" $stage

    $zip = "$stage.zip"
    Remove-Item -Force $zip -ErrorAction SilentlyContinue
    Compress-Archive -Path "$stage\*" -DestinationPath $zip
    Remove-Item -Recurse -Force $stage

    $hash = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
    $sums += "$hash  $(Split-Path $zip -Leaf)"
    Write-Host ("   {0} ({1:N1} MB)" -f (Split-Path $zip -Leaf), ((Get-Item $zip).Length / 1MB))
}
$sums | Set-Content (Join-Path $dist "SHA256SUMS.txt") -Encoding ascii
Write-Host "done: $dist"
