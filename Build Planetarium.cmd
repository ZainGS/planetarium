@echo off
rem Builds Planetarium (Tauri) in release mode: a standalone .exe plus an installer.
rem The first build compiles all Rust dependencies and takes several minutes.
cd /d "%~dp0"
cargo tauri build
if errorlevel 1 (
  echo.
  echo Build failed. Copy the first error above and paste it to Claude.
  pause
  exit /b 1
)
echo.
echo App:       %~dp0src-tauri\target\release\planetarium.exe
echo Installer: %~dp0src-tauri\target\release\bundle\nsis\
explorer "%~dp0src-tauri\target\release"
pause
