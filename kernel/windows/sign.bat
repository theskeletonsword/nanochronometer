@echo off
rem SPDX-License-Identifier: MIT
rem
rem Windows-side signing of build\nanochrono_{x64,arm64}.sys
rem
rem Uses osslsigncode.exe if available, otherwise signtool (WDK).
rem Requires certs-private\nanochrono-test.pfx (created by certs\make-test-cert.sh
rem or sign.sh on the Linux side) and an admin "Developer Mode" prompt;
rem test drivers need `bcdedit /set testsigning on` before loading.
rem
rem Output goes to build\signed\.

setlocal
cd /d "%~dp0"

set OUT=build\signed
set PFX=certs-private\nanochrono-test.pfx
if not exist "%PFX%" (
    echo ERROR: %PFX% not found. Run certs\make-test-cert.sh or sign.sh first.
    exit /b 1
)
if not exist "%OUT%" mkdir "%OUT%"

where osslsigncode >nul 2>nul
if %errorlevel%==0 (
    for %%f in (build\nanochrono_x64.sys build\nanochrono_arm64.sys) do (
        if not exist "%%f" (
            echo ERROR: missing %%f - build first.
            exit /b 1
        )
        echo == signing %%f
        osslsigncode sign -certs certs-private\nanochrono-test.pem ^
            -key certs-private\nanochrono-test.key ^
            -h sha256 -n "NanoChronometer test driver" ^
            -in "%%f" -out "%OUT%\%%~nxf"
    )
    echo == verifying
    for %%f in (%OUT%\*.sys) do osslsigncode verify "%%f"
    goto :done
)

where signtool >nul 2>nul
if %errorlevel%==0 (
    for %%f in (build\nanochrono_x64.sys build\nanochrono_arm64.sys) do (
        signtool sign /f "%PFX%" /p "" /fd SHA256 /td SHA256 /tr "http://timestamp.digicert.com" "%%f"
    )
    goto :done
)

echo ERROR: neither osslsigncode nor signtool found. Install osslsigncode or the WDK.
exit /b 1

:done
echo == done: signed drivers in %OUT%