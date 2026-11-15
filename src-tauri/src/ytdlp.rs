use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Manager};

const DOWNLOAD_BASE: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download";
const PROGRAM_NAME: &str = "yt-dlp.exe";

/// Only one task installs yt-dlp at a time; the others wait and then find the file.
#[derive(Default)]
pub struct Install(tokio::sync::Mutex<()>);

/**
 * The path of the yt-dlp program, which reads video sites that a plain page
 * read cannot (YouTube, Vimeo and many more). It is not in the installer: the
 * first use downloads the official release into the app's data folder and
 * checks it against the release's published SHA-256.
 */
pub async fn ensure(app: &AppHandle) -> Result<PathBuf> {
    let folder = app.path().app_local_data_dir().context("no app data folder")?;
    let program = folder.join(PROGRAM_NAME);
    let _guard = app.state::<Install>().inner().0.lock().await;
    if program.is_file() {
        return Ok(program);
    }
    tokio::fs::create_dir_all(&folder).await?;
    let client = reqwest::Client::builder().user_agent("ScrapeSage").build()?;
    let sums = client
        .get(format!("{DOWNLOAD_BASE}/SHA2-256SUMS"))
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    let expected = sums
        .lines()
        .find_map(|line| {
            let mut parts = line.split_whitespace();
            let sum = parts.next()?;
            (parts.next()?.trim_start_matches('*') == PROGRAM_NAME).then(|| sum.to_ascii_lowercase())
        })
        .ok_or_else(|| anyhow!("the yt-dlp checksum list has no entry for {PROGRAM_NAME}"))?;
    let bytes = client
        .get(format!("{DOWNLOAD_BASE}/{PROGRAM_NAME}"))
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    let actual = Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if actual != expected {
        return Err(anyhow!("the yt-dlp download does not match its SHA-256 checksum"));
    }
    let partial = folder.join("yt-dlp.download");
    tokio::fs::write(&partial, &bytes).await?;
    tokio::fs::rename(&partial, &program).await?;
    Ok(program)
}

/** A yt-dlp command that opens no console window. */
pub fn command(program: &PathBuf) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(program);
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command.stdin(std::process::Stdio::null());
    command
}
