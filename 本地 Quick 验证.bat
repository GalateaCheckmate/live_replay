@echo off
setlocal EnableExtensions
chcp 65001 >nul
cd /d "%~dp0"

title Live Replay - 本地 Quick 验证

echo ============================================================
echo [Live Replay] 本地 Quick 验证
echo [Live Replay] 前端构建 + Rust 格式检查 + Workspace 编译检查
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
if not "%NODE_MAJOR%"=="20" (
    echo [提醒] GitHub Quick 当前使用 Node.js 20；本机是 Node.js %NODE_MAJOR%。
    echo [提醒] 先继续验证；若前端出现兼容问题，再切换到 Node.js 20。
    echo.
)

echo [1/4] 安装前端依赖 npm ci...
call npm.cmd ci
if errorlevel 1 goto :failed

echo.
echo [2/4] 编译 Next.js 前端...
call npm.cmd run build
if errorlevel 1 goto :failed

echo.
echo [3/4] 检查 Rust 格式...
cargo fmt --all -- --check
if errorlevel 1 goto :failed

echo.
echo [4/4] 检查 Rust Workspace...
set "SQLX_OFFLINE=true"
cargo check --workspace --all-targets --locked
if errorlevel 1 goto :failed

echo.
echo ============================================================
echo [成功] Live Replay 本地 Quick 验证全部通过。
echo ============================================================
echo.
pause
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

:failed
echo.
echo ============================================================
echo [失败] Quick 验证未通过。请把上方第一个报错位置截图或复制给我。
echo ============================================================
echo.
pause
exit /b 1
