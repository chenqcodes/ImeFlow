@echo off
setlocal
cd /d "%~dp0"

if not exist "%~dp0.diagnostics" (
    mkdir "%~dp0.diagnostics"
)

start "" explorer.exe "%~dp0.diagnostics"
