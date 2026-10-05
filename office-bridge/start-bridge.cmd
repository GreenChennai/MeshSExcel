@echo off
rem MeshSExcel 桌面桥启动器:用 LibreOffice 自带的 Python(自带 UNO 模块)运行桥
rem 用法:start-bridge.cmd [--node http://localhost:8443] [--doc <doc_id>]

setlocal
set "LO_PYTHON="
for %%P in (
  "C:\Program Files\LibreOffice\program\python.exe"
  "C:\Program Files (x86)\LibreOffice\program\python.exe"
  "D:\LibreOffice\program\python.exe"
  "D:\tools\LibreOffice\program\python.exe"
) do (
  if exist %%P set "LO_PYTHON=%%~P"
)

rem 注册表兜底(自定义安装路径)
if not defined LO_PYTHON (
  for /f "tokens=2,*" %%A in ('reg query "HKLM\SOFTWARE\LibreOffice\UNO\InstallPath" 2^>nul') do (
    if exist "%%B\python.exe" set "LO_PYTHON=%%B\python.exe"
  )
)

if not defined LO_PYTHON (
  echo [错误] 未找到 LibreOffice 自带的 python.exe。
  echo 请先安装 LibreOffice(https://www.libreoffice.org/),或把 program 目录加入查找路径。
  exit /b 1
)

echo [bridge] 使用 %LO_PYTHON%
"%LO_PYTHON%" "%~dp0mesh_bridge.py" %*
