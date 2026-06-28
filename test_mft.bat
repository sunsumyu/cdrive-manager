@echo off
:: MFT 测试脚本 - 需要以管理员权限运行
:: 右键点击此文件，选择"以管理员身份运行"

echo ========================================
echo C 盘空间管理器 - MFT 高速扫描测试
echo ========================================
echo.

:: 检查是否有管理员权限
net session >nul 2>&1
if %errorLevel% == 0 (
    echo [OK] 已获得管理员权限
) else (
    echo [错误] 请以管理员身份运行此脚本!
    echo 右键点击此文件，选择"以管理员身份运行"
    pause
    exit /b 1
)

echo.
echo 正在启动程序...
echo 请在程序中选择 C:\ 目录，然后点击 "MFT 高速扫描" 按钮
echo.

:: 运行程序
cd /d "%~dp0"
target\release\cdrive-manager.exe

pause