@echo off
setlocal
cd /d "%~dp0"
title ImeFlow - Release Package

echo ========================================
echo   ImeFlow - release test + package
echo   Output: dist\ImeFlow\
echo ========================================
echo.

powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\release-package.ps1"
set "code=%errorlevel%"

echo.
if not "%code%"=="0" (
    echo Release package failed. Exit code: %code%
    pause
) else (
    echo Done. Double-click dist\ImeFlow\启动 ImeFlow.cmd or run 07-install-local.cmd
    pause
)
exit /b %code%
