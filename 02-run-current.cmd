@echo off
setlocal
cd /d "%~dp0"
title ImeFlow - Run Current Build

echo ========================================
echo   ImeFlow M1.1 - run current build
echo   No compile, no tests.
echo ========================================
echo.

pwsh.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\run-current.ps1"
set "code=%errorlevel%"

if not "%code%"=="0" (
    echo.
    echo ImeFlow did not start successfully. Exit code: %code%
    pause
)
exit /b %code%
