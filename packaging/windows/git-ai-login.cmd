@echo off
setlocal

set "GIT_AI_USER_BINARY=%USERPROFILE%\.git-ai\bin\git-ai.exe"
if exist "%GIT_AI_USER_BINARY%" (
  "%GIT_AI_USER_BINARY%" bg start
  exit /b %ERRORLEVEL%
)

"%~dp0git-ai.exe" bg start
