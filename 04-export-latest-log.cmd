@echo off
setlocal
cd /d "%~dp0"
title ImeFlow - Export Latest Key Log

powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\export-latest-key-log.ps1"
set "code=%errorlevel%"

echo.
if not "%code%"=="0" (
    echo Export failed. Exit code: %code%
) else (
    echo Key log is ready for ChatGPT.
)
pause
exit /b %code%
