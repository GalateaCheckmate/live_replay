@echo off
setlocal
chcp 65001 >nul
cd /d "%~dp0"

echo [Live Replay] 正在启动，请勿关闭此窗口...
echo [Live Replay] 启动成功后会自动打开浏览器。
echo.

live-replay.exe
set "EXIT_CODE=%ERRORLEVEL%"

if not "%EXIT_CODE%"=="0" (
    echo.
    echo [Live Replay] 启动失败，错误代码：%EXIT_CODE%
    echo 请保留此窗口并截图其中的错误信息。
    pause
)

endlocal
