@echo off
setlocal
cd /d "%~dp0"
title ImeFlow - Build and Run

echo ========================================
echo   ImeFlow 0.1.0-m1 - test, build, PNG check, then run
echo ========================================
echo.

powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\build-and-run.ps1" -Run
set "code=%errorlevel%"

echo.
if not "%code%"=="0" (
    echo ImeFlow did not start successfully. Exit code: %code%
    echo Check the messages above.
    pause
)
exit /b %code%
