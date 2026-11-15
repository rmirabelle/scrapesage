use std::path::Path;

/** Opens the folder of an existing file in File Explorer, with the file selected. */
#[tauri::command]
pub fn reveal_file(path: String) -> Result<(), String> {
    if !Path::new(&path).is_file() {
        return Err(format!("{path} no longer exists"));
    }
    let mut command = std::process::Command::new("explorer");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.raw_arg(format!("/select,\"{path}\""));
    }
    #[cfg(not(windows))]
    command.arg(&path);
    command
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("Could not open File Explorer: {error}"))
}

/** Opens a folder in File Explorer. */
#[tauri::command]
pub fn open_folder(path: String) -> Result<(), String> {
    if !Path::new(&path).is_dir() {
        return Err(format!("{path} is not a folder"));
    }
    std::process::Command::new("explorer")
        .arg(&path)
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("Could not open File Explorer: {error}"))
}

/** Writes an error from the webview to the app's error output, so crashes show in the dev log. */
#[tauri::command]
pub fn log_frontend_error(message: String) {
    eprintln!("[scrapesage] frontend error: {}", message.chars().take(4000).collect::<String>());
}

/** Returns the OS cursor position in physical pixels, for the window-control hover fix. */
#[tauri::command]
pub fn cursor_position() -> (i32, i32) {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::POINT;
        use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
        let mut point = POINT { x: 0, y: 0 };
        unsafe {
            let _ = GetCursorPos(&mut point);
        }
        (point.x, point.y)
    }
    #[cfg(not(windows))]
    {
        (0, 0)
    }
}
