param([switch]$Native)
$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
$evidence = Join-Path $repo ('target/readonly-config-' + (Get-Date -Format 'yyyyMMdd-HHmmss'))
$fixture = Join-Path $evidence 'config'
New-Item -ItemType Directory -Path $fixture -Force | Out-Null
$utf8 = [System.Text.UTF8Encoding]::new($false)
[IO.File]::WriteAllText((Join-Path $fixture 'settings.toml'), "check_updates = false`nlanguage = 'ko'`nfont_size = 23`n", $utf8)
[IO.File]::WriteAllText((Join-Path $fixture 'sessions.toml'), "[[session]]`nid = 'readonly-test'`nname = '읽기 전용 테스트 사이트'`nhost = 'example.test'`nusername = 'tester'`n", $utf8)
$before = Get-FileHash (Join-Path $fixture '*.toml')
$originalAcl = Get-Acl -LiteralPath $fixture
$acl = Get-Acl -LiteralPath $fixture
$sid = [Security.Principal.WindowsIdentity]::GetCurrent().User
$rule = [Security.AccessControl.FileSystemAccessRule]::new($sid, 'Write', 'ContainerInherit,ObjectInherit', 'None', 'Deny')
$acl.AddAccessRule($rule)
$oldTestDir = $env:PLAYTERM_READONLY_TEST_DIR
$oldConfigDir = $env:PLAYTERM_CONFIG_DIR
$appProcess = $null
try {
    Set-Acl -LiteralPath $fixture -AclObject $acl
    $env:PLAYTERM_READONLY_TEST_DIR = $fixture
    Push-Location $repo
    try {
        & cargo test --locked -p playterm readonly_config_starts_lists_sites_and_rejects_save -- --ignored --nocapture
        if ($LASTEXITCODE -ne 0) { throw 'Read-only UI test failed' }
    } finally { Pop-Location }
    if ($Native) {
        Add-Type -AssemblyName System.Drawing
        Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class ReadonlyWindow {
    [StructLayout(LayoutKind.Sequential)] public struct Rect { public int Left, Top, Right, Bottom; }
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr window, out Rect rect);
    public delegate bool EnumProc(IntPtr window, IntPtr param);
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc callback, IntPtr param);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr window, out uint pid);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr window);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr window, uint msg, IntPtr w, IntPtr l);
    public static IntPtr Find(int pid) {
        IntPtr found = IntPtr.Zero;
        EnumWindows((w, p) => {
            uint owner; Rect rect;
            GetWindowThreadProcessId(w, out owner);
            GetWindowRect(w, out rect);
            if (owner == pid && rect.Right - rect.Left > 600 && rect.Bottom - rect.Top > 400) found = w;
            return true;
        }, IntPtr.Zero);
        return found;
    }
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr window, IntPtr dc, uint flags);
}
'@
        $env:PLAYTERM_CONFIG_DIR = $fixture
        $appProcess = Start-Process -FilePath (Join-Path $repo 'target/debug/playterm.exe') -PassThru
        $deadline = (Get-Date).AddSeconds(30)
        do {
            Start-Sleep -Milliseconds 250
            $appProcess.Refresh()
            if ($appProcess.HasExited) { throw 'Native app exited during startup' }
            $window = [ReadonlyWindow]::Find($appProcess.Id)
        } while ($window -eq [IntPtr]::Zero -and (Get-Date) -lt $deadline)
        if ($window -eq [IntPtr]::Zero) { throw 'Native window did not appear' }
        [void][ReadonlyWindow]::SetForegroundWindow($window)
        Start-Sleep -Seconds 3
        $rect = [ReadonlyWindow+Rect]::new()
        [void][ReadonlyWindow]::GetWindowRect($window, [ref]$rect)
        $bitmap = [Drawing.Bitmap]::new($rect.Right - $rect.Left, $rect.Bottom - $rect.Top)
        $graphics = [Drawing.Graphics]::FromImage($bitmap)
        $graphics.CopyFromScreen($rect.Left, $rect.Top, 0, 0, $bitmap.Size)
        $captured = $true
        try {
            if (-not $captured) { throw 'Window capture failed' }
            $bitmap.Save((Join-Path $evidence 'native-startup.png'))
        } finally { $graphics.Dispose(); $bitmap.Dispose() }
        [void][ReadonlyWindow]::PostMessage($window, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero)
        if (-not $appProcess.WaitForExit(10000)) { throw 'App did not close' }
        if ($appProcess.ExitCode -ne 0) { throw "App exit code: $($appProcess.ExitCode)" }
    }
    foreach ($file in $before) {
        if ((Get-FileHash -LiteralPath $file.Path).Hash -ne $file.Hash) { throw "Configuration changed: $($file.Path)" }
    }
    'PASS: read-only startup, settings, site list, failed save, unchanged files.' | Tee-Object -FilePath (Join-Path $evidence 'result.txt')
    Write-Output "Evidence: $evidence"
} finally {
    if ($appProcess -and -not $appProcess.HasExited) { Stop-Process -Id $appProcess.Id }
    Set-Acl -LiteralPath $fixture -AclObject $originalAcl
    $env:PLAYTERM_READONLY_TEST_DIR = $oldTestDir
    $env:PLAYTERM_CONFIG_DIR = $oldConfigDir
}
