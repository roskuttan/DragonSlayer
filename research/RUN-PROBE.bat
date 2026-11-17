@echo off
rem Double-click launcher for the READ-ONLY MSI probe.
rem Asks Windows for admin, then runs probe-readonly.ps1 -Explore.

net session >nul 2>&1
if %errorlevel% neq 0 (
    powershell -NoProfile -Command "Start-Process -FilePath '%~f0' -Verb RunAs"
    exit /b
)

cd /d "%~dp0"
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0probe-readonly.ps1" -Explore
echo.
echo Done. You can close this window.
pause
