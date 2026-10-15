@echo off
setlocal
cd /d "%~dp0"
title ImeFlow - Install Local

echo ========================================
echo   Install dist\ImeFlow to LocalAppData
echo   + Start Menu shortcut, then launch
echo ========================================
echo.

if not exist "%~dp0dist\ImeFlow\imeflow.exe" (
    echo dist\ImeFlow missing. Run 06-release-package.cmd first.
    pause
    exit /b 1
)

pwsh.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\install-local.ps1" -From "%~dp0dist\ImeFlow"
set "code=%errorlevel%"

if not "%code%"=="0" (
    echo Install failed. Exit code: %code%
    pause
)
exit /b %code%
