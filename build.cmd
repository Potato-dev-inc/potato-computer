@echo off
setlocal
set "PUPPET_MASTER_PROFILE=my-computer"
cd /d "%~dp0"
npm run build --workspace=@puppet-master/app || exit /b 1
cargo build --manifest-path packages\app\src-tauri\Cargo.toml || exit /b 1
echo Built target\debug\puppet_master_app.exe - start it with run.cmd
