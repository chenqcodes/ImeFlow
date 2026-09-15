@echo off
setlocal
cd /d "%~dp0"
title ImeFlow - Verify Only
echo ========================================
echo   ImeFlow m1.1-assets1 - verification only
echo   Tests, build and PNG checks. No IME automation.
echo ========================================
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\build-and-run.ps1"
set "code=%errorlevel%"
echo.
if not "%code%"=="0" (
    echo Verification failed. Exit code: %code%
) else (
    echo Verification completed. Run 02-run-current.cmd to start.
)
pause
exit /b %code%
