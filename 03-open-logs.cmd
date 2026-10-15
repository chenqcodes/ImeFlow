@echo off
setlocal
cd /d "%~dp0"

pwsh.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\open-logs.ps1"
