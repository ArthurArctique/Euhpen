@echo off
rem Euhpen : double-cliquez pour ouvrir l application.
rem La premiere fois, installe ce qui manque et compile Euhpen (quelques minutes).
cd /d "%~dp0"
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0packaging\windows\lancer.ps1"
if errorlevel 1 pause
