@echo off
rem Records which controller-chip bytes change while you click things in
rem Dragon Center. READ-ONLY: it changes nothing. Runs for 3 minutes.
rem Result is saved to watch-log.txt in this folder.
cd /d "%~dp0"
"%~dp0..\target\release\dragonslayer-cli.exe" watch 180
echo.
echo Saved to %~dp0watch-log.txt - you can close this window.
pause
