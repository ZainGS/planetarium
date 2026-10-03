@echo off
rem Runs Planetarium (Tauri) in development mode, rebuilding the Rust side when it changes.
cd /d "%~dp0"
cargo tauri dev
pause
