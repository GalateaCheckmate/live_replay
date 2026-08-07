@echo off
setlocal EnableExtensions EnableDelayedExpansion
chcp 65001 >nul
cd /d "%~dp0"

title Live Replay - Local Quick

echo ============================================================
echo [Live Replay] Local Quick Validation
echo [Mode] Incremental frontend + Rust workspace check
echo ============================================================
echo.

set "VSWHERE=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe"
set "VSROOT="
if exist "%VSWHERE%" (
    for /f "usebackq tokens=*" %%I in (`"%VSWHERE%" -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath`) do set "VSROOT=%%I"
)
if defined VSROOT if exist "%VSROOT%\Common7\Tools\VsDevCmd.bat" (
    echo [ENV] Visual Studio C++: %VSROOT%
    call "%VSROOT%\Common7\Tools\VsDevCmd.bat" -arch=x64 -host_arch=x64 >nul
) else (
    echo [WARN] Visual Studio C++ environment was not loaded automatically.
)

where node >nul 2>nul || goto :node_missing
where npm.cmd >nul 2>nul || goto :npm_missing
where rustc >nul 2>nul || goto :rust_missing
where cargo >nul 2>nul || goto :cargo_missing
where powershell >nul 2>nul || goto :powershell_missing

echo.
echo [ENV] Node:
node --version
echo [ENV] npm:
call npm.cmd --version
echo [ENV] Rust:
rustc --version
echo [ENV] Cargo:
cargo --version
echo.

for /f %%N in ('node -p "process.versions.node.split('.')[0]"') do set "NODE_MAJOR=%%N"
if "%NODE_MAJOR%"=="20" goto :node_version_ok
echo [WARN] GitHub Quick uses Node.js 20. Local Node.js major is %NODE_MAJOR%.
echo [WARN] Validation will continue. Switch to Node.js 20 only if compatibility issues remain.
echo.
:node_version_ok

echo [1/3] Checking frontend dependency cache...
call :ensure_frontend_dependencies
if errorlevel 1 goto :failed

echo.
echo [2/3] Building Next.js frontend...
call npm.cmd run build
if errorlevel 1 goto :failed

echo.
echo [3/3] Incremental Rust workspace check...
set "SQLX_OFFLINE=true"
cargo check --workspace --all-targets --locked
if errorlevel 1 goto :failed

echo.
echo ============================================================
echo [PASS] Live Replay Local Quick passed.
echo [CACHE] node_modules and Cargo target are kept for next run.
echo ============================================================
echo.
pause
exit /b 0

:ensure_frontend_dependencies
if not exist "package-lock.json" (
    echo [FAIL] package-lock.json was not found.
    exit /b 1
)

set "LOCK_HASH="
for /f %%H in ('powershell -NoProfile -NonInteractive -Command "(Get-FileHash -Algorithm SHA256 'package-lock.json').Hash"') do set "LOCK_HASH=%%H"
if not defined LOCK_HASH (
    echo [FAIL] Could not calculate package-lock.json hash.
    exit /b 1
)

set "LOCK_CACHE=node_modules\.live-replay-package-lock.sha256"

if not exist "node_modules\" goto :deps_clean_install

if not exist "!LOCK_CACHE!" (
    echo [DEPS] Existing node_modules found. Creating cache marker...
    call npm.cmd ls --depth=0 --silent >nul 2>nul
    if errorlevel 1 goto :deps_sync
    >"!LOCK_CACHE!" echo !LOCK_HASH!
    echo [DEPS] Existing dependency tree accepted.
    goto :deps_peer_check
)

set "CACHED_HASH="
set /p "CACHED_HASH="<"!LOCK_CACHE!"
if /I "!CACHED_HASH!"=="!LOCK_HASH!" (
    echo [DEPS] package-lock.json unchanged. Skipping npm install.
    goto :deps_peer_check
)

echo [DEPS] package-lock.json changed. Syncing dependencies using local cache first...
goto :deps_sync

:deps_clean_install
echo [DEPS] First run. Running npm ci...
call npm.cmd ci
if errorlevel 1 exit /b 1
>"!LOCK_CACHE!" echo !LOCK_HASH!
goto :deps_peer_check

:deps_sync
call npm.cmd install --prefer-offline --no-audit --no-fund
if errorlevel 1 exit /b 1
for /f %%H in ('powershell -NoProfile -NonInteractive -Command "(Get-FileHash -Algorithm SHA256 'package-lock.json').Hash"') do set "LOCK_HASH=%%H"
>"!LOCK_CACHE!" echo !LOCK_HASH!
goto :deps_peer_check

:deps_peer_check
if exist "node_modules\react-draggable\package.json" (
    echo [DEPS] react-draggable peer dependency is present.
    exit /b 0
)

echo [DEPS] npm did not install react-draggable peer dependency. Repairing once...
call npm.cmd install --no-save --package-lock=false --prefer-offline --no-audit --no-fund react-draggable@4.7.0
if errorlevel 1 exit /b 1
if not exist "node_modules\react-draggable\package.json" (
    echo [FAIL] react-draggable is still missing after repair.
    exit /b 1
)
echo [DEPS] react-draggable repaired successfully.
exit /b 0

:node_missing
echo [FAIL] Node.js was not found.
goto :failed

:npm_missing
echo [FAIL] npm.cmd was not found.
goto :failed

:rust_missing
echo [FAIL] rustc was not found.
goto :failed

:cargo_missing
echo [FAIL] cargo was not found.
goto :failed

:powershell_missing
echo [FAIL] PowerShell was not found.
goto :failed

:failed
echo.
echo ============================================================
echo [FAIL] Local Quick validation failed.
echo Please send the first error shown above.
echo ============================================================
echo.
pause
exit /b 1
