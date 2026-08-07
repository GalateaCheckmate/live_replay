@echo off
setlocal EnableExtensions EnableDelayedExpansion
chcp 65001 >nul
cd /d "%~dp0"

title Live Replay - 本地 Full 验证

echo ============================================================
echo [Live Replay] 本地 Full 验证
echo [Live Replay] 增量依赖 + 前端构建 + Rust 全测试 + Release 编译
echo ============================================================
echo.

rem 自动加载 Visual Studio C++ 编译环境（如果已安装）
set "VSWHERE=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe"
set "VSROOT="
if exist "%VSWHERE%" (
    for /f "usebackq tokens=*" %%I in (`"%VSWHERE%" -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath`) do set "VSROOT=%%I"
)
if defined VSROOT if exist "%VSROOT%\Common7\Tools\VsDevCmd.bat" (
    echo [环境] Visual Studio C++: %VSROOT%
    call "%VSROOT%\Common7\Tools\VsDevCmd.bat" -arch=x64 -host_arch=x64 >nul
) else (
    echo [提醒] 未自动加载 Visual Studio C++ 环境；如果 Cargo 报 linker/cl 错误，请检查 VS C++ 工作负载。
)

where node >nul 2>nul || goto :node_missing
where npm.cmd >nul 2>nul || goto :npm_missing
where rustc >nul 2>nul || goto :rust_missing
where cargo >nul 2>nul || goto :cargo_missing
where powershell >nul 2>nul || goto :powershell_missing

echo.
echo [环境] Node:
node --version
echo [环境] npm:
call npm.cmd --version
echo [环境] Rust:
rustc --version
echo [环境] Cargo:
cargo --version
echo.

for /f %%N in ('node -p "process.versions.node.split('.')[0]"') do set "NODE_MAJOR=%%N"
if "%NODE_MAJOR%"=="20" goto :node_version_ok
echo [提醒] GitHub Full 当前使用 Node.js 20；本机是 Node.js %NODE_MAJOR%。
echo [提醒] 当前先继续验证；只有遇到前端兼容问题时才需要切换 Node.js 20。
echo.
:node_version_ok

echo [1/4] 检查前端依赖缓存...
call :ensure_frontend_dependencies
if errorlevel 1 goto :failed

echo.
echo [2/4] 编译 Next.js 前端...
call npm.cmd run build
if errorlevel 1 goto :failed

echo.
echo [3/4] 增量运行 Rust Workspace 全部测试...
set "SQLX_OFFLINE=true"
cargo test --workspace --locked
if errorlevel 1 goto :failed

echo.
echo [4/4] 增量编译 Windows Release EXE...
cargo build --release --locked --bin biliup
if errorlevel 1 goto :failed

if not exist "%CD%\target\release\biliup.exe" (
    echo [失败] Cargo 返回成功，但没有找到 target\release\biliup.exe。
    goto :failed
)

echo.
echo ============================================================
echo [成功] Live Replay 本地 Full 验证全部通过。
echo [产物] %CD%\target\release\biliup.exe
echo [缓存] node_modules 和 Cargo target/release 都会保留供下次增量验证。
echo [说明] GitHub Full 便携包还会额外打包 ffmpeg.exe / ffprobe.exe。
echo ============================================================
echo.
pause
exit /b 0

:ensure_frontend_dependencies
if not exist "package-lock.json" (
    echo [失败] 没有找到 package-lock.json。
    exit /b 1
)

set "LOCK_HASH="
for /f %%H in ('powershell -NoProfile -NonInteractive -Command "(Get-FileHash -Algorithm SHA256 'package-lock.json').Hash"') do set "LOCK_HASH=%%H"
if not defined LOCK_HASH (
    echo [失败] 无法计算 package-lock.json 哈希。
    exit /b 1
)

set "LOCK_CACHE=node_modules\.live-replay-package-lock.sha256"

if not exist "node_modules\" goto :deps_clean_install

if not exist "!LOCK_CACHE!" (
    echo [依赖] 已有 node_modules，首次建立本地依赖缓存标记...
    call npm.cmd ls --depth=0 --silent >nul 2>nul
    if errorlevel 1 goto :deps_sync
    >"!LOCK_CACHE!" echo !LOCK_HASH!
    echo [依赖] 当前依赖可用，本次不重新下载。
    exit /b 0
)

set "CACHED_HASH="
set /p "CACHED_HASH="<"!LOCK_CACHE!"
if /I "!CACHED_HASH!"=="!LOCK_HASH!" (
    echo [依赖] package-lock.json 未变化，跳过 npm 安装。
    exit /b 0
)

echo [依赖] package-lock.json 已变化，只同步新增/变化的依赖。
goto :deps_sync

:deps_clean_install
echo [依赖] 首次运行，执行 npm ci...
call npm.cmd ci
if errorlevel 1 exit /b 1
>"!LOCK_CACHE!" echo !LOCK_HASH!
exit /b 0

:deps_sync
call npm.cmd install --prefer-offline --no-audit --no-fund
if errorlevel 1 exit /b 1
for /f %%H in ('powershell -NoProfile -NonInteractive -Command "(Get-FileHash -Algorithm SHA256 'package-lock.json').Hash"') do set "LOCK_HASH=%%H"
>"!LOCK_CACHE!" echo !LOCK_HASH!
exit /b 0

:node_missing
echo [失败] 未找到 Node.js。请先安装 Node.js。
goto :failed

:npm_missing
echo [失败] 未找到 npm.cmd。请检查 Node.js 安装。
goto :failed

:rust_missing
echo [失败] 未找到 rustc。请先安装 Rust stable。
goto :failed

:cargo_missing
echo [失败] 未找到 cargo。请先安装 Rust stable。
goto :failed

:powershell_missing
echo [失败] 未找到 PowerShell，无法检查依赖缓存。
goto :failed

:failed
echo.
echo ============================================================
echo [失败] Full 验证未通过。请把上方第一个报错位置截图或复制给我。
echo ============================================================
echo.
pause
exit /b 1
