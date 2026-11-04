# My Computer

Share one mouse and keyboard between your computers.

The app is a Tauri desktop app. It opens directly on the **My Computer** page:
screen hops on this PC, pairing with another computer, mouse/keyboard sharing,
clipboard, and pointer/key settings.

## Run

```bat
build.cmd
run.cmd
```

`dev.cmd` starts the Vite + Tauri development loop.

The app runs under the `my-computer` profile, so its settings and keys live in
their own app-data folder.

## Requirements

- Node.js 22+
- Rust (stable) with Cargo
- Windows: WebView2 (installed with Windows 10/11)
