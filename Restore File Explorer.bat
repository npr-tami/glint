@echo off
setlocal
cd /d "%~dp0"
title Glint - restore File Explorer
echo.
echo  Restore File Explorer
echo  =====================
echo  Folders, drives and Win+E go back to opening in File Explorer.
echo.

rem glint.exe next to this file, or the one cargo built. It puts back what was there before.
set "EXE=glint.exe"
if not exist "%EXE%" set "EXE=target\release\glint.exe"
if exist "%EXE%" (
  start "" /wait "%EXE%" --restore-explorer
  if not errorlevel 1 goto done
)

rem glint.exe is missing or failed: remove Glint's registry entries directly (your account only).
call :cleanup Directory
call :cleanup Drive
set "WINEKEY=HKCU\Software\Classes\CLSID\{52205fd8-5dfb-447d-801a-d0b52f2e83e1}\shell\opennewwindow"
reg query "%WINEKEY%\command" /v GlintOwned >nul 2>nul && reg delete "%WINEKEY%" /f >nul

:done
echo  Done. Folders open in File Explorer again.
echo.
pause
exit /b 0

:cleanup
reg delete "HKCU\Software\Classes\%1\shell\glint" /f >nul 2>nul
reg query "HKCU\Software\Classes\%1\shell" /ve 2>nul | find /i "glint" >nul && reg delete "HKCU\Software\Classes\%1\shell" /ve /f >nul
exit /b 0
