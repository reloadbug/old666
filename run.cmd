@echo off
rem old666 - local test launcher (see tools\run.ps1 for details)
rem   run.cmd              build (incremental) + launch as admin   [daily use]
rem   run.cmd -NoBuild     launch existing exe, no compile         [fastest]
rem   run.cmd -DryRun      build + check only, do not launch
rem   run.cmd -NoElevate   launch without UAC prompt
rem   run.cmd -ResetData   wipe test data before launch
rem   run.cmd -Release     build with the shipping release profile
rem   run.cmd -Debug       build with the debug profile
setlocal
cd /d "%~dp0"
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0tools\run.ps1" %*
set RC=%ERRORLEVEL%
if not "%RC%"=="0" (
  echo.
  echo  run.ps1 exited with code %RC%
  pause
)
