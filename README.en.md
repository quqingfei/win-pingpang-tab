# FastTabClick

An enhanced window switching tool for the Windows platform, enabling quick switching between different windows via custom hotkeys.

## Features

- **Hotkey Trigger**: Supports custom modifier key (Alt, Ctrl, Shift, Win, etc.) combinations + mouse buttons (Right, Middle, X1, X2)
- **Smart Window Switching**: Automatically builds a stack of all windows under the current cursor, switch to the next window with one click
- **Process Blacklist**: Configurable to ignore specific processes; mouse events pass through unchanged when triggered
- **System Tray**: Runs in the background, tray menu provides quick actions
- **Auto-start on Startup**: Supports automatic startup on boot
- **Logging**: Hierarchical logging for easier troubleshooting
- **Single Instance**: Prevents duplicate launches

## Supported Hotkeys

- **Modifiers**: `lalt` / `ralt` / `lctrl` / `rctrl` / `lshift` / `rshift` / `lwin` / `rwin` (Any combination, at least 1)
- **Trigger Mouse Button**: `right` / `middle` / `x1` / `x2` (Left not included)

## Configuration File

After the first run, a `config.toml` configuration file will be automatically generated in the `%APPDATA%\fast-tab-click\` directory:

```toml
# Modifier key configuration
modifiers = ["lalt"]

# Trigger mouse button
button = "right"

# Process blacklist (if matched, pass mouse events through without switching windows)
blacklist = ["explorer.exe"]

# Start with administrator privileges
elevated = false

# Log level: off | error | info | debug
log_level = "info"

# Max log file size (MB)
max_log_size = 10
```

## Build Requirements

- Rust 1.70+
- Windows SDK
- Visual Studio Build Tools

## Compilation

```bash
cargo build --release
```

The compiled executable is located at `target/release/fast-tab-click.exe`.

## Usage Instructions

1. A configuration file will be created automatically after the first run.
2. Modify the hotkeys and blacklist in the configuration file as needed.
3. Restart the application to apply the configuration.
4. Use the configured hotkeys (hold modifier keys + click mouse button) to switch windows.

### Tray Menu

- **Restart as Administrator**: Restart the application with administrator privileges
- **Open Config Folder**: Open the directory containing the configuration file
- **Auto-start on Startup**: Check to enable/disable auto-start on boot
- **Exit**: Close the application

## Known Limitations

- Does not support left (left-click) as a trigger button
- Some special windows (such as UWP cloaked windows) may be ignored

## Tech Stack

- Rust
- Windows API (Win32)
- Tauri (Optional)

## License

MIT License