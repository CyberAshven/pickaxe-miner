@echo off
setlocal
cd /d "%~dp0..\.."
set "UPSTREAM=%~1"
if not defined UPSTREAM set "UPSTREAM=artifacts\ultrafast-evaluation\upstream"
for /f %%H in ('git -C "%UPSTREAM%" rev-parse HEAD') do set "UPSTREAM_HEAD=%%H"
if not "%UPSTREAM_HEAD%"=="540ac5b9c910f089c449177ebf000e4c130cab19" (
  echo Expected pinned UltrafastSecp256k1 v4.6.0 checkout.
  exit /b 1
)
call "%~dp0..\build-incremental-k.cmd" "cuda\build"
if errorlevel 1 exit /b 1
call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat"
if errorlevel 1 exit /b 1
set "UF_FLAGS=-DPICKAXE_ULTRAFAST_EXPERIMENT -DPICKAXE_C1_SCALAR_CHECK"
nvcc -ptx -O3 -arch=sm_120 -maxrregcount=128 %UF_FLAGS% -I"%UPSTREAM%\src\cuda\include" -I"%UPSTREAM%\src\cpu\include" -o cuda\build\ultrafast_walk.ptx cuda\photon_incremental_k.cu
if errorlevel 1 exit /b 1
nvcc -ptx -O3 -arch=sm_120 -maxrregcount=128 %UF_FLAGS% -I"%UPSTREAM%\src\cuda\include" -I"%UPSTREAM%\src\cpu\include" -o cuda\build\ultrafast_c1.ptx cuda\photon_c1_schnorr.cu
if errorlevel 1 exit /b 1
nvcc -ptx -O3 -arch=sm_120 -maxrregcount=128 %UF_FLAGS% -DPICKAXE_KEEP_FIXED_D -I"%UPSTREAM%\src\cuda\include" -I"%UPSTREAM%\src\cpu\include" -o cuda\build\ultrafast_c1_fixed_d.ptx cuda\photon_c1_schnorr.cu
if errorlevel 1 exit /b 1
nvcc -ptx -O3 -arch=sm_120 -maxrregcount=128 %UF_FLAGS% -DPICKAXE_INCREMENTAL_K_EXPERIMENT -I"%UPSTREAM%\src\cuda\include" -I"%UPSTREAM%\src\cpu\include" -o cuda\build\ultrafast_c3.ptx cuda\photon_c3_dual.cu
exit /b %errorlevel%
