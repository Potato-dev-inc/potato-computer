@echo off
setlocal
set "PUPPET_MASTER_PROFILE=my-computer"
cd /d "%~dp0"
npm run dev
