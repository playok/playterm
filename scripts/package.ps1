# Builds release executables and packs them into dist\playterm-<version>-windows-<arch>.zip.
#
#   pwsh scripts\package.ps1                 # ARM64 + x64
#   pwsh scripts\package.ps1 -Targets x64    # x64 only
#   pwsh scripts\package.ps1 -Upx            # executables compressed with UPX
#   pwsh scripts\package.ps1 -Sign           # SHA256SUMS.txt must be signed (fails without a key)
#   pwsh scripts\package.ps1 -CodeSign       # playterm.exe code-signed (fails without a certificate)
#
# -CodeSign signs each playterm.exe with signtool (SHA-256, RFC 3161 timestamp) after UPX, so the
# signature covers the compressed file: the certificate is a .pfx in $env:WINDOWS_CERT_PFX_FILE
# or base64 in $env:WINDOWS_CERT_PFX, its password in $env:WINDOWS_CERT_PASSWORD, the timestamp
# server $env:WINDOWS_TIMESTAMP_URL (default DigiCert's). signtool comes from the Windows SDK.
#
# SHA256SUMS.txt is signed (SHA256SUMS.txt.sig, Ed25519, by crates/playterm-sign) whenever the
# private key is given in $env:PLAYTERM_SIGNING_KEY (hex) or $env:PLAYTERM_SIGNING_KEY_FILE; the
# in-app updater installs only signed releases. The signature is then checked against the public
# key built into the app (crates/playterm/src/update.rs), so a wrong key stops the run.
#
# Requires the x64 Rust target (rustup target add x86_64-pc-windows-msvc) and, on ARM64
# hosts, LLVM/clang (winget install LLVM.LLVM) for the crypto library.
#
# -Upx uses $env:UPX_EXE (a path to upx.exe or a command on PATH; not $env:UPX, which UPX
# reads as default options) together with $env:UPX_SHA256, its expected SHA-256, otherwise the
# pinned UPX release in tools\upx: downloaded from GitHub on first use, the zip and the upx.exe
# in it checked against their SHA-256. Each run copies upx.exe to a new temporary folder and
# checks that copy right before running it (the builds in between run every dependency's build
# script, which could otherwise swap the file). ARM64 executables need UPX 5.3.0 or later.
param(
    [ValidateSet("arm64", "x64")]
    [string[]]$Targets = @("arm64", "x64"),
    [switch]$Upx,
    [switch]$Sign,
    [switch]$CodeSign
)
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root

$llvm = "C:\Program Files\LLVM\bin"
if ((Test-Path $llvm) -and ($env:PATH -notlike "*$llvm*")) { $env:PATH += ";$llvm" }

$UpxVersion = "5.3.0"
$UpxZipSha256 = "d127996c76615e431be810e9c8d2910dbe3dd400600c5c0c50276fc5a17a9705"   # upx-5.3.0-win64.zip (GitHub release)
$UpxExeSha256 = "ff30a651c9e41a2bbb363825fac9e7e23d92871c1ac82813a264e3b8d1cd302b"   # upx.exe in that zip

function Test-Sha256([string]$Path, [string]$Expected) {
    (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLower() -eq $Expected
}

# Path and expected SHA-256 of upx.exe: $env:UPX_EXE with $env:UPX_SHA256, or the pinned
# release in tools\upx.
function Get-Upx {
    if ($env:UPX_EXE) {
        $path = $null
        if (Test-Path -LiteralPath $env:UPX_EXE -PathType Leaf) { $path = (Resolve-Path -LiteralPath $env:UPX_EXE).Path }
        else {
            $cmd = Get-Command $env:UPX_EXE -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
            if ($cmd) { $path = $cmd.Source }
        }
        if (-not $path) { throw "UPX_EXE=$env:UPX_EXE is neither a file nor a command on PATH" }
        $actual = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLower()
        if (-not $env:UPX_SHA256) { throw "UPX_EXE=${path}: set UPX_SHA256 to its expected SHA-256 (this file's is $actual)" }
        if ($actual -ne $env:UPX_SHA256.ToLower()) { throw "UPX_EXE=$path does not match UPX_SHA256 (it is $actual)" }
        return @{ Path = $path; Sha256 = $actual }
    }
    $dir = Join-Path $root "tools\upx"
    $exe = Join-Path $dir "upx-$UpxVersion-win64\upx.exe"
    if (Test-Path -LiteralPath $exe -PathType Leaf) {
        # Checked on every run: tools\ is not tracked, and a broken or replaced packer would end
        # up in the release.
        if (Test-Sha256 $exe $UpxExeSha256) { return @{ Path = $exe; Sha256 = $UpxExeSha256 } }
        Write-Host "   $exe does not match its SHA-256; downloading UPX again"
        Remove-Item -Recurse -Force (Split-Path $exe -Parent)
    }

    $zip = Join-Path $dir "upx-$UpxVersion-win64.zip"
    $url = "https://github.com/upx/upx/releases/download/v$UpxVersion/upx-$UpxVersion-win64.zip"
    Write-Host "   downloading UPX $UpxVersion"
    New-Item -ItemType Directory -Force $dir | Out-Null
    # The progress bar makes Windows PowerShell 5.1 downloads many times slower.
    $progress = $ProgressPreference
    $ProgressPreference = "SilentlyContinue"
    try {
        Invoke-WebRequest -Uri $url -OutFile $zip -UseBasicParsing
    } finally {
        $ProgressPreference = $progress
    }
    if (-not (Test-Sha256 $zip $UpxZipSha256)) {
        Remove-Item -Force $zip
        throw "the UPX download does not match its SHA-256; not using it"
    }
    Expand-Archive -Path $zip -DestinationPath $dir -Force
    Remove-Item -Force $zip
    if (-not (Test-Path -LiteralPath $exe -PathType Leaf)) { throw "upx.exe not found at $exe after unpacking" }
    if (-not (Test-Sha256 $exe $UpxExeSha256)) {
        Remove-Item -Recurse -Force (Split-Path $exe -Parent)
        throw "the unpacked upx.exe does not match its SHA-256; not using it"
    }
    return @{ Path = $exe; Sha256 = $UpxExeSha256 }
}

# Runs UPX from a fresh copy that is checked right before it runs.
function Invoke-Upx([hashtable]$Upx, [string[]]$Arguments) {
    $tmp = Join-Path ([IO.Path]::GetTempPath()) ("playterm-upx-" + [guid]::NewGuid())
    New-Item -ItemType Directory $tmp | Out-Null
    try {
        $copy = Join-Path $tmp "upx.exe"
        Copy-Item -LiteralPath $Upx.Path $copy
        if (-not (Test-Sha256 $copy $Upx.Sha256)) { throw "upx.exe changed since it was checked; not running it" }
        & $copy @Arguments | Out-Null
        return $LASTEXITCODE
    } finally {
        Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
    }
}

$version = (Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"(.+)"').Matches[0].Groups[1].Value
$triples = @{ arm64 = "aarch64-pc-windows-msvc"; x64 = "x86_64-pc-windows-msvc" }
$dist = Join-Path $root "dist"
New-Item -ItemType Directory -Force $dist | Out-Null

# signtool.exe of the newest Windows SDK (x64 tools; they sign ARM64 files too).
function Get-SignTool {
    $found = Get-Command signtool.exe -ErrorAction SilentlyContinue
    if ($found) { return $found.Source }
    $kits = "${env:ProgramFiles(x86)}\Windows Kits\10\bin"
    $tool = Get-ChildItem -Path $kits -Filter signtool.exe -Recurse -ErrorAction SilentlyContinue |
        Where-Object { $_.FullName -like "*\x64\signtool.exe" } |
        Sort-Object FullName -Descending | Select-Object -First 1
    if (-not $tool) { throw "-CodeSign: signtool.exe not found (install the Windows SDK)" }
    return $tool.FullName
}

# The certificate for -CodeSign as a file (a temporary one for $env:WINDOWS_CERT_PFX).
function Get-CodeSignCert {
    if ($env:WINDOWS_CERT_PFX_FILE) {
        if (-not (Test-Path -LiteralPath $env:WINDOWS_CERT_PFX_FILE -PathType Leaf)) { throw "-CodeSign: $env:WINDOWS_CERT_PFX_FILE not found" }
        return @{ Path = $env:WINDOWS_CERT_PFX_FILE; Temporary = $false }
    }
    if (-not $env:WINDOWS_CERT_PFX) { throw "-CodeSign: set WINDOWS_CERT_PFX (base64) or WINDOWS_CERT_PFX_FILE" }
    $path = Join-Path ([IO.Path]::GetTempPath()) ("playterm-cert-" + [guid]::NewGuid() + ".pfx")
    [IO.File]::WriteAllBytes($path, [Convert]::FromBase64String($env:WINDOWS_CERT_PFX))
    return @{ Path = $path; Temporary = $true }
}

function Invoke-CodeSign([string]$SignTool, [hashtable]$Cert, [string]$Exe) {
    $url = if ($env:WINDOWS_TIMESTAMP_URL) { $env:WINDOWS_TIMESTAMP_URL } else { "http://timestamp.digicert.com" }
    $arguments = @("sign", "/fd", "SHA256", "/tr", $url, "/td", "SHA256", "/f", $Cert.Path)
    if ($env:WINDOWS_CERT_PASSWORD) { $arguments += @("/p", $env:WINDOWS_CERT_PASSWORD) }
    & $SignTool @arguments $Exe | Out-Host
    if ($LASTEXITCODE -ne 0) { throw "signtool sign failed for $Exe" }
    & $SignTool verify /pa $Exe | Out-Host
    if ($LASTEXITCODE -ne 0) { throw "signtool verify failed for $Exe" }
}

# Before any build, so a missing or bad UPX stops the run early.
$upxExe = if ($Upx) { Get-Upx } else { $null }
# UPX takes default options from $env:UPX (e.g. -k leaves a backup file that would be zipped).
Remove-Item Env:UPX -ErrorAction SilentlyContinue
# Likewise for code signing: no certificate or signtool stops the run before building.
$signTool = if ($CodeSign) { Get-SignTool } else { $null }
$cert = if ($CodeSign) { Get-CodeSignCert } else { $null }

$sums = @()
foreach ($arch in $Targets) {
    $triple = $triples[$arch]
    Write-Host "== $arch ($triple)"
    cargo build --release --locked -p playterm --target $triple
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed for $triple" }

    $stage = Join-Path $dist "playterm-$version-windows-$arch"
    Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
    New-Item -ItemType Directory $stage | Out-Null
    $docs = @("README.md", "README.en.md", "LICENSE")
    Copy-Item "target\$triple\release\playterm.exe" $stage
    Copy-Item $docs $stage

    if ($upxExe) {
        $exe = Join-Path $stage "playterm.exe"
        $before = (Get-Item $exe).Length
        if ((Invoke-Upx $upxExe @("--best", "-q", $exe)) -ne 0) { throw "UPX failed for $arch" }
        if ((Invoke-Upx $upxExe @("-t", "-q", $exe)) -ne 0) { throw "UPX test failed for $arch" }
        Write-Host ("   UPX: {0:N1} MB -> {1:N1} MB" -f ($before / 1MB), ((Get-Item $exe).Length / 1MB))
    }
    if ($signTool) {
        Write-Host "   code signing"
        Invoke-CodeSign $signTool $cert (Join-Path $stage "playterm.exe")
    }
    $extra = @(Get-ChildItem $stage | Where-Object { $_.Name -notin (@("playterm.exe") + $docs) })
    if ($extra) { throw "unexpected files in the package: $($extra.Name -join ', ')" }

    $zip = "$stage.zip"
    Remove-Item -Force $zip -ErrorAction SilentlyContinue
    Compress-Archive -Path "$stage\*" -DestinationPath $zip
    Remove-Item -Recurse -Force $stage

    $hash = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
    $sums += "$hash  $(Split-Path $zip -Leaf)"
    Write-Host ("   {0} ({1:N1} MB)" -f (Split-Path $zip -Leaf), ((Get-Item $zip).Length / 1MB))
}
$sumsFile = Join-Path $dist "SHA256SUMS.txt"
$sums | Set-Content $sumsFile -Encoding ascii
Remove-Item -Force "$sumsFile.sig" -ErrorAction SilentlyContinue
$haveKey = [bool]($env:PLAYTERM_SIGNING_KEY -or $env:PLAYTERM_SIGNING_KEY_FILE)
if ($Sign -and -not $haveKey) { throw "-Sign: set PLAYTERM_SIGNING_KEY or PLAYTERM_SIGNING_KEY_FILE" }
if ($haveKey) {
    Write-Host "== Signing SHA256SUMS.txt"
    cargo run --release --locked -q -p playterm-sign -- sign $sumsFile
    if ($LASTEXITCODE -ne 0) { throw "signing failed" }
    $public = (Select-String -Path "crates\playterm\src\update.rs" -Pattern 'const PUBLIC_KEY: &str = "([0-9a-f]{64})"').Matches[0].Groups[1].Value
    cargo run --release --locked -q -p playterm-sign -- verify $sumsFile $public
    if ($LASTEXITCODE -ne 0) { throw "the signing key does not match PUBLIC_KEY in crates\playterm\src\update.rs" }
} else {
    Write-Host "   SHA256SUMS.txt not signed (no PLAYTERM_SIGNING_KEY): the in-app updater will not install these packages"
}
if ($cert -and $cert.Temporary) { Remove-Item -Force -LiteralPath $cert.Path -ErrorAction SilentlyContinue }
Write-Host "done: $dist"
