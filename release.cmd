@echo off
rem Checks, tests and packages playterm into dist\:
rem   dist\playterm-<version>-windows-<arch>.zip and dist\SHA256SUMS.txt
rem
rem   release.cmd              ARM64 and x64, standard and optional UPX executables
rem   release.cmd x64          one architecture (arm64 or x64)
rem   release.cmd noupx        without UPX compression (also: release.cmd x64 noupx)
rem   release.cmd dirty        package even with uncommitted changes (not for a real release)
rem
rem Builds use --locked: Cargo.lock must already match the manifests, so no dependency
rem version changes quietly during a release.
rem
rem The x64 build needs its Rust target once: rustup target add x86_64-pc-windows-msvc
rem UPX: %UPX_EXE% (a path or a command on PATH) if set, else the pinned release in tools\upx
rem (downloaded once; SHA-256 checked).
setlocal
cd /d "%~dp0"

set "ARCH="
set "UPX_FLAG=-BothVariants"
set "ALLOW_DIRTY="
:args
if "%~1"=="" goto :args_done
if /i "%~1"=="arm64" (
    if defined ARCH goto :usage
    set "ARCH=arm64"
) else if /i "%~1"=="x64" (
    if defined ARCH goto :usage
    set "ARCH=x64"
) else if /i "%~1"=="noupx" (
    set "UPX_FLAG="
) else if /i "%~1"=="dirty" (
    set "ALLOW_DIRTY=1"
) else (
    goto :usage
)
shift
goto :args
:args_done

call :tools || exit /b 1

rem A release is built from committed code (the zip name says nothing about local changes).
set "DIRTY="
for /f %%i in ('git status --porcelain 2^>nul') do set "DIRTY=1"
if defined DIRTY if not defined ALLOW_DIRTY (
    echo There are uncommitted changes. Commit them, or run "release.cmd dirty" for a test package.
    exit /b 1
)
if defined DIRTY echo warning: there are uncommitted changes; the packages will include them.
for /f %%i in ('git rev-parse --short HEAD 2^>nul') do echo == Commit %%i

echo == Checking (clippy)
cargo clippy --locked --workspace --all-targets -- -D warnings || goto :failed

echo == Testing
cargo test --locked --workspace || goto :failed

echo == Packaging
set "PS=powershell"
where pwsh >nul 2>nul && set "PS=pwsh"
if "%ARCH%"=="" (
    %PS% -NoProfile -ExecutionPolicy Bypass -File scripts\package.ps1 %UPX_FLAG%
) else (
    %PS% -NoProfile -ExecutionPolicy Bypass -File scripts\package.ps1 -Targets %ARCH% %UPX_FLAG%
)
if errorlevel 1 goto :failed

echo.
echo == Release packages in %CD%\dist
dir /b dist\*.zip
echo.
type dist\SHA256SUMS.txt
exit /b 0

:usage
echo Usage: release.cmd [arm64^|x64] [noupx] [dirty]
echo   One architecture, or none for both.
exit /b 2

:failed
echo.
echo Release failed.
exit /b 1

:tools
rem ring (SSH crypto) needs clang on Windows ARM64.
if exist "C:\Program Files\LLVM\bin\clang.exe" set "PATH=%PATH%;C:\Program Files\LLVM\bin"
where cargo >nul 2>nul || (
    echo cargo not found. Install Rust from https://rustup.rs
    exit /b 1
)
where clang >nul 2>nul || echo warning: clang not found; on ARM64 install it with: winget install LLVM.LLVM
exit /b 0
