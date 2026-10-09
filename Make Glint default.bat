@echo off
setlocal
cd /d "%~dp0"
title Glint - make default
echo.
echo  Make Glint your default file explorer
echo  ======================================
echo  Folders and drives you double-click (desktop, shortcuts, Win+R) will open
echo  in Glint. Control Panel, Recycle Bin, phones and similar still open in
echo  File Explorer. Only your own account changes, nothing else in Windows.
echo  To undo it: "Restore File Explorer.bat" (or SETTINGS in Glint).
echo.

rem glint.exe next to this file, or the one cargo built.
set "EXE=glint.exe"
if not exist "%EXE%" set "EXE=target\release\glint.exe"
if not exist "%EXE%" (
  echo  glint.exe is missing. Build it first with: cargo build --release
  echo.
  pause
  exit /b 1
)

set "WINKEY="
choice /c YN /m " Should Win+E open Glint too"
if not errorlevel 2 set "WINKEY=--with-win-e"

start "" /wait "%EXE%" --set-default %WINKEY%
if errorlevel 1 (
  echo.
  echo  Something went wrong. The details are in error.log, in
  echo  %LOCALAPPDATA%\Glint\logs  ^(or the logs folder next to glint.exe^)
  echo.
  pause
  exit /b 1
)

echo.
echo  Done. Try double-clicking a folder on your desktop.
if defined WINKEY echo  Win+E opens Glint now too.
echo  Keep glint.exe where it is; if you move it, start it once from the new place.
echo.
pause
