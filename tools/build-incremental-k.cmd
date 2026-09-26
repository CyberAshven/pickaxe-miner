@echo off
setlocal
call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat"
if errorlevel 1 exit /b 1
cd /d "%~dp0.."
set "KERNEL_OUT=%~1"
if not defined KERNEL_OUT set "KERNEL_OUT=artifacts\incremental-k\ptx"
if not exist "%KERNEL_OUT%" mkdir "%KERNEL_OUT%"
if errorlevel 1 exit /b 1
nvcc -ptx -O3 -arch=sm_120 -o "%KERNEL_OUT%\stage_a_rfc6979.ptx" cuda\stage_a_rfc6979.cu
if errorlevel 1 exit /b 1
for %%K in (photon_stage_b16 photon_c1_schnorr photon_c3_dual photon_incremental_k) do (
  nvcc -ptx -O3 -arch=sm_120 -maxrregcount=128 -o "%KERNEL_OUT%\%%K.ptx" cuda\%%K.cu
  if errorlevel 1 exit /b 1
)
nvcc -ptx -O3 -arch=sm_120 -maxrregcount=128 -DPICKAXE_INCREMENTAL_K_EXPERIMENT -o "%KERNEL_OUT%\photon_incremental_c3.ptx" cuda\photon_c3_dual.cu
if errorlevel 1 exit /b 1
