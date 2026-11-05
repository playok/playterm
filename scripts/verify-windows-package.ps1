# Verify the final uploaded ZIP, including the executable extracted from that ZIP.
param(
    [Parameter(Mandatory)][string]$Path,
    [Parameter(Mandatory)][ValidatePattern('^[0-9a-fA-F]{64}$')][string]$ExpectedSha256,
    [switch]$RequireCodeSignature,
    [string]$ReportPath
)
$ErrorActionPreference = 'Stop'
$archive = (Resolve-Path -LiteralPath $Path).Path
$hash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
if ($hash -ne $ExpectedSha256.ToLowerInvariant()) { throw "Package checksum mismatch: $archive" }
$temporaryRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
$temporary = Join-Path $temporaryRoot ("playterm-verify-" + [guid]::NewGuid())
try {
    Expand-Archive -LiteralPath $archive -DestinationPath $temporary
    $executables = @(Get-ChildItem -LiteralPath $temporary -Filter playterm.exe -File -Recurse)
    if ($executables.Count -ne 1) { throw 'The ZIP must contain exactly one playterm.exe' }
    $signature = Get-AuthenticodeSignature -LiteralPath $executables[0].FullName
    if ($RequireCodeSignature) {
        if ($signature.Status -ne 'Valid') { throw "Invalid Authenticode signature: $($signature.Status)" }
        if (-not $signature.SignerCertificate -or -not $signature.TimeStamperCertificate) {
            throw 'A verified publisher and timestamp are required'
        }
    }
    $report = [ordered]@{
        package = [IO.Path]::GetFileName($archive)
        sha256 = $hash
        signature = [string]$signature.Status
        publisher = if ($signature.SignerCertificate) { $signature.SignerCertificate.Subject } else { $null }
        timestamp_authority = if ($signature.TimeStamperCertificate) { $signature.TimeStamperCertificate.Subject } else { $null }
        checked_utc = [DateTime]::UtcNow.ToString('o')
    } | ConvertTo-Json
    if ($ReportPath) { $report | Set-Content -LiteralPath $ReportPath -Encoding utf8 }
    $report
} finally {
    $resolvedTemporary = [IO.Path]::GetFullPath($temporary)
    if (-not $resolvedTemporary.StartsWith($temporaryRoot, [StringComparison]::OrdinalIgnoreCase) -or
        [IO.Path]::GetFileName($resolvedTemporary) -notlike 'playterm-verify-*') { throw 'Unsafe temporary cleanup path' }
    if (Test-Path -LiteralPath $resolvedTemporary) { Remove-Item -LiteralPath $resolvedTemporary -Recurse -Force }
}
