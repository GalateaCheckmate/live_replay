@echo off
setlocal EnableExtensions EnableDelayedExpansion
chcp 65001 >nul
cd /d "%~dp0"

title Live Replay - Local Full

echo ============================================================
echo [Live Replay] Local Full Validation
echo [Mode] Cached frontend + Rust tests + Release build
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
echo [WARN] GitHub Full uses Node.js 20. Local Node.js major is %NODE_MAJOR%.
echo [WARN] Validation will continue. Switch to Node.js 20 only if compatibility issues remain.
echo.
:node_version_ok

echo [1/4] Checking frontend dependency cache...
call :ensure_frontend_dependencies
if errorlevel 1 goto :failed

echo.
echo [2/4] Building Next.js frontend...
call npm.cmd run build
if errorlevel 1 goto :failed

echo.
echo [3/4] Incremental Rust workspace tests...
set "SQLX_OFFLINE=true"
cargo test --workspace --locked
if errorlevel 1 goto :failed

echo.
echo [4/4] Incremental Windows Release build...
cargo build --release --locked --bin biliup
if errorlevel 1 goto :failed

if not exist "%CD%\target\release\biliup.exe" (
    echo [FAIL] Cargo succeeded but target\release\biliup.exe was not found.
    goto :failed
)

echo.
echo ============================================================
echo [PASS] Live Replay Local Full passed.
echo [OUTPUT] %CD%\target\release\biliup.exe
echo [CACHE] node_modules and Cargo target/release are kept for next run.
echo [NOTE] Portable packaging with FFmpeg is separate for now.
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

set "LOCK_CACHE=node_modules\.live-replay-package-lock-v2.sha256"

if not exist "node_modules\" goto :deps_refresh
if not exist "!LOCK_CACHE!" goto :deps_adopt_existing

set "CACHED_HASH="
set /p "CACHED_HASH="<"!LOCK_CACHE!"
if /I not "!CACHED_HASH!"=="!LOCK_HASH!" goto :deps_refresh

call :validate_frontend_tree
if errorlevel 1 goto :deps_refresh

echo [DEPS] package-lock.json unchanged. Using cached node_modules.
exit /b 0

:deps_adopt_existing
call :validate_frontend_tree
if errorlevel 1 goto :deps_refresh
>"!LOCK_CACHE!" echo !LOCK_HASH!
echo [DEPS] Existing dependency tree is valid. Cache marker created; no reinstall needed.
exit /b 0

:deps_refresh
echo [DEPS] Refreshing exact dependency tree from package-lock.json...
echo [DEPS] npm cache will be preferred; this only runs when the lock file changes or cache integrity fails.
call npm.cmd ci --prefer-offline --no-audit --no-fund
if errorlevel 1 exit /b 1

call :validate_frontend_tree
if errorlevel 1 goto :deps_invalid

>"!LOCK_CACHE!" echo !LOCK_HASH!
echo [DEPS] Exact dependency tree restored and cache marker updated.
exit /b 0

:validate_frontend_tree
if not exist "node_modules\next\package.json" exit /b 1
if not exist "node_modules\@douyinfe\semi-ui\package.json" exit /b 1
if not exist "node_modules\react-resizable\package.json" exit /b 1
rem react-draggable is intentionally nested under react-resizable in package-lock.json.
rem Ask Node's resolver instead of assuming it must exist at node_modules\react-draggable.
node -e "const p=require('path'); require.resolve('react-draggable',{paths:[p.dirname(require.resolve('react-resizable'))]});" >nul 2>nul
if errorlevel 1 exit /b 1
exit /b 0

:deps_invalid
echo [FAIL] npm ci completed but Node still cannot resolve a required frontend dependency.
echo [FAIL] Send this output for diagnosis; do not run npm install manually.
exit /b 1

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
echo [FAIL] Local Full validation failed.
echo Please send the first error shown above.
echo ============================================================
echo.
pause
exit /b 1
