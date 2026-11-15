use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context, Result};
use futures_util::StreamExt;
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::sync::Notify;
use url::Url;

use crate::{ytdlp, BROWSER_USER_AGENT};

/// The marker that starts each yt-dlp progress line (see `--progress-template`).
const PROGRESS_MARK: &str = "SCRAPESAGE-PROGRESS ";
/// The longest file name made from a video's title, in characters.
const NAME_LIMIT: usize = 120;
/// The error text of a download that the user cancelled; the screen shows no error for it.
const CANCELLED: &str = "Cancelled";

/// The downloads that run now, by id, each with the signal that cancels it.
#[derive(Default)]
pub struct Downloads(Mutex<HashMap<String, Arc<Notify>>>);

/** The default folder for downloads: Videos\Scrape Sage. */
fn default_folder(app: &AppHandle) -> Result<PathBuf> {
    Ok(app.path().video_dir().context("could not find the Videos folder")?.join("Scrape Sage"))
}

/** The folder that a download goes to: the user's choice, else the default. It is made if it is missing. */
async fn target_folder(app: &AppHandle, chosen: Option<&str>) -> Result<PathBuf> {
    let folder = match chosen.map(str::trim).filter(|path| !path.is_empty()) {
        Some(path) => PathBuf::from(path),
        None => default_folder(app)?,
    };
    tokio::fs::create_dir_all(&folder)
        .await
        .with_context(|| format!("could not create {}", folder.display()))?;
    Ok(folder)
}

/** The default download folder's path, made if it is missing. */
#[tauri::command]
pub async fn download_folder(app: AppHandle) -> Result<String, String> {
    target_folder(&app, None)
        .await
        .map(|folder| folder.to_string_lossy().into_owned())
        .map_err(|error| format!("{error:#}"))
}

/** A title as a Windows file name: no reserved characters, no end dot or space, not too long. */
fn safe_name(title: &str) -> String {
    let name: String = title
        .chars()
        .map(|character| if character.is_control() || "<>:\"/\\|?*".contains(character) { ' ' } else { character })
        .collect();
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    let name: String = name.chars().take(NAME_LIMIT).collect();
    let name = name.trim_end_matches(['.', ' ']).to_string();
    if name.is_empty() { "Video".into() } else { name }
}

/** A path in the folder that no file has yet: "Name.mp4", then "Name (2).mp4" and so on. */
fn free_path(folder: &Path, name: &str, extension: &str) -> PathBuf {
    let mut path = folder.join(format!("{name}.{extension}"));
    let mut number = 2;
    while path.exists() {
        path = folder.join(format!("{name} ({number}).{extension}"));
        number += 1;
    }
    path
}

fn emit_progress(app: &AppHandle, id: &str, percent: Option<f64>) {
    let _ = app.emit("download-progress", serde_json::json!({ "id": id, "percent": percent }));
}

/** Deletes an incomplete file when a download fails or is cancelled (the task is dropped). */
struct PartialFile(Option<PathBuf>);

impl Drop for PartialFile {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

/**
 * Stops yt-dlp when a download fails or is cancelled (the task is dropped).
 * yt-dlp.exe starts a second process that does the work, so the whole process
 * tree is stopped, not only the first process.
 */
struct ProcessTree(Option<u32>);

impl Drop for ProcessTree {
    fn drop(&mut self) {
        if let Some(id) = self.0.take() {
            let mut command = std::process::Command::new("taskkill");
            command.args(["/PID", &id.to_string(), "/T", "/F"]);
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                const CREATE_NO_WINDOW: u32 = 0x0800_0000;
                command.creation_flags(CREATE_NO_WINDOW);
            }
            let _ = command
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
        }
    }
}

/** Downloads a video file with a plain request. */
async fn download_file(app: &AppHandle, id: &str, url: &Url, title: &str, source: &str, folder: &Path) -> Result<PathBuf> {
    let response = reqwest::Client::builder()
        .user_agent(BROWSER_USER_AGENT)
        .build()?
        .get(url.clone())
        .header(reqwest::header::REFERER, source)
        .send()
        .await?;
    if !response.status().is_success() {
        return Err(anyhow!("the site answered {}", response.status()));
    }
    let extension = url
        .path()
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .filter(|extension| extension.len() <= 4 && extension.chars().all(|character| character.is_ascii_alphanumeric()))
        .unwrap_or_else(|| "mp4".into());
    let target = free_path(folder, &safe_name(title), &extension);
    /**
     * The file has a ".part" name until it is complete, so a stopped download
     * is not taken for a video. The guard is made before the file, so the file
     * is closed before the guard deletes it.
     */
    let partial = target.with_extension(format!("{extension}.part"));
    let mut guard = PartialFile(Some(partial.clone()));
    let total = response.content_length().unwrap_or(0);
    let mut file = tokio::fs::File::create(&partial)
        .await
        .with_context(|| format!("create {}", partial.display()))?;
    let mut stream = response.bytes_stream();
    let mut downloaded = 0_u64;
    let mut last_emitted = 0_u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        file.write_all(&chunk).await?;
        downloaded += chunk.len() as u64;
        if total > 0 && downloaded.saturating_sub(last_emitted) >= 262_144 {
            last_emitted = downloaded;
            emit_progress(app, id, Some(downloaded as f64 * 100.0 / total as f64));
        }
    }
    file.flush().await?;
    drop(file);
    tokio::fs::rename(&partial, &target).await?;
    guard.0 = None;
    Ok(target)
}

/**
 * Downloads a video with yt-dlp: a video's page, or a stream list. The format
 * is the best single file that has picture and sound, because the app has no
 * FFmpeg to join separate picture and sound files. Incomplete files stay in
 * the app's data folder, not in the download folder; a later download of the
 * same video continues from them.
 */
async fn download_with_ytdlp(app: &AppHandle, id: &str, url: &str, source: &str, folder: &Path) -> Result<PathBuf> {
    let program = ytdlp::ensure(app).await.context("could not get yt-dlp")?;
    let partial_folder = app.path().app_local_data_dir().context("no app data folder")?.join("partial");
    let mut home = std::ffi::OsString::from("home:");
    home.push(folder);
    let mut temp = std::ffi::OsString::from("temp:");
    temp.push(&partial_folder);
    let mut child = ytdlp::command(&program)
        .args(["--no-playlist", "--no-warnings", "--format", "b", "--windows-filenames", "--no-overwrites"])
        .args(["--progress", "--newline", "--progress-template"])
        .arg(format!("download:{PROGRESS_MARK}%(progress._percent_str)s"))
        .args(["--print", "after_move:filepath", "--encoding", "utf-8", "--referer", source])
        .arg("--paths")
        .arg(home)
        .arg("--paths")
        .arg(temp)
        .args(["--output", "%(title).120B [%(id)s].%(ext)s", "--", url])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("could not run yt-dlp")?;
    let mut tree = ProcessTree(child.id());
    let stdout = child.stdout.take().context("yt-dlp has no output")?;
    let mut stderr = child.stderr.take().context("yt-dlp has no error output")?;
    let errors = tokio::spawn(async move {
        let mut text = String::new();
        let _ = tokio::io::AsyncReadExt::read_to_string(&mut stderr, &mut text).await;
        text
    });
    let mut lines = tokio::io::BufReader::new(stdout).lines();
    let mut path = None;
    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if let Some(percent) = line.strip_prefix(PROGRESS_MARK) {
            emit_progress(app, id, percent.trim().trim_end_matches('%').parse().ok());
        } else if !line.is_empty() {
            path = Some(PathBuf::from(line));
        }
    }
    let status = child.wait().await?;
    tree.0 = None;
    let errors = errors.await.unwrap_or_default();
    if !status.success() {
        return Err(anyhow!("{}", errors.lines().last().unwrap_or("yt-dlp failed").trim().to_string()));
    }
    path.ok_or_else(|| anyhow!("yt-dlp did not say where it saved the video"))
}

/**
 * Downloads one video and returns the file's path. `folder` is the user's
 * download folder; without it the video goes to Videos\Scrape Sage. While the
 * download runs, "download-progress" events carry the id and the percent done
 * (null when the size is not known), and `cancel_download` with the same id
 * stops it: the answer is then the error "Cancelled".
 */
#[tauri::command]
pub async fn download_video(
    app: AppHandle,
    id: String,
    url: String,
    title: String,
    source: String,
    direct: bool,
    folder: Option<String>,
) -> Result<String, String> {
    let address = Url::parse(&url).map_err(|_| format!("{url} is not a web address"))?;
    if !matches!(address.scheme(), "http" | "https") {
        return Err(format!("{url} is not a web address"));
    }
    let cancel = Arc::new(Notify::new());
    app.state::<Downloads>().0.lock().unwrap().insert(id.clone(), cancel.clone());
    emit_progress(&app, &id, None);
    let path = address.path().to_ascii_lowercase();
    let stream_list = path.ends_with(".m3u8") || path.ends_with(".mpd");
    let work = async {
        let folder = target_folder(&app, folder.as_deref()).await?;
        if direct && !stream_list {
            download_file(&app, &id, &address, &title, &source, &folder).await
        } else {
            download_with_ytdlp(&app, &id, &url, &source, &folder).await
        }
    };
    /* On a cancel, the work is dropped: its guards delete the incomplete file and stop yt-dlp. */
    let result = tokio::select! {
        result = work => result,
        _ = cancel.notified() => Err(anyhow!(CANCELLED)),
    };
    app.state::<Downloads>().0.lock().unwrap().remove(&id);
    result
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(|error| format!("{error:#}"))
}

/** Cancels the download with this id, if it runs. */
#[tauri::command]
pub fn cancel_download(app: AppHandle, id: String) {
    if let Some(cancel) = app.state::<Downloads>().0.lock().unwrap().get(&id) {
        cancel.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::safe_name;

    #[test]
    fn makes_safe_file_names() {
        assert_eq!(safe_name("  What? A <clip>: part 1/2.  "), "What A clip part 1 2");
        assert_eq!(safe_name("???"), "Video");
    }
}
