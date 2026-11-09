@echo off
setlocal
cd /d "%~dp0"
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
where cargo >nul 2>nul
if errorlevel 1 (
  echo Rust is missing. Install Rust with rustup and the MSVC build tools listed in README.md.
  exit /b 1
)
cargo build --release --locked
exit /b %errorlevel%
