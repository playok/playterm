@echo off
rem Builds playterm for this PC.
rem
rem   build.cmd            release build  -> target\release\playterm.exe
rem   build.cmd debug      debug build    -> target\debug\playterm.exe
setlocal
cd /d "%~dp0"

set "PROFILE=%~1"
if "%PROFILE%"=="" set "PROFILE=release"
if /i not "%PROFILE%"=="release" if /i not "%PROFILE%"=="debug" (
    echo Usage: build.cmd [release^|debug]
    exit /b 2
)

call :tools || exit /b 1

echo == Building playterm (%PROFILE%)
if /i "%PROFILE%"=="release" (
    cargo build --release -p playterm
) else (
    cargo build -p playterm
)
if errorlevel 1 (
    echo.
    echo Build failed. If the error says "access is denied", close the running playterm.exe and try again.
    exit /b 1
)
echo.
echo Built: %CD%\target\%PROFILE%\playterm.exe
exit /b 0

:tools
rem ring (SSH crypto) needs clang on Windows ARM64.
if exist "C:\Program Files\LLVM\bin\clang.exe" set "PATH=%PATH%;C:\Program Files\LLVM\bin"
where cargo >nul 2>nul || (
    echo cargo not found. Install Rust from https://rustup.rs
    exit /b 1
)
where clang >nul 2>nul || echo warning: clang not found; on ARM64 install it with: winget install LLVM.LLVM
exit /b 0
