use std::collections::HashSet;

use scraper::{ElementRef, Html, Selector};
use serde::Serialize;
use tauri::AppHandle;
use url::Url;

use crate::{ytdlp, BROWSER_USER_AGENT};

/// File extensions of a link that is a video file (or a stream list) itself.
const VIDEO_EXTENSIONS: [&str; 8] = ["mp4", "webm", "mov", "m4v", "mkv", "ogv", "m3u8", "mpd"];
/// Sites whose links are always a video's page.
const VIDEO_HOSTS: [&str; 5] = ["youtube.com", "youtu.be", "vimeo.com", "dailymotion.com", "twitch.tv"];
/// Words in a link's path that mark a video's page, when the link also shows a picture.
const VIDEO_PATH_WORDS: [&str; 6] = ["video", "watch", "/clip", "/v/", "/embed/", "/play"];
/// The attributes that hold a picture's address, the lazy-load ones first.
const IMAGE_ATTRIBUTES: [&str; 5] = ["data-src", "data-lazy-src", "data-original", "data-thumb", "src"];

/// One video found on a page.
#[derive(Debug, Serialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VideoItem {
    pub title: String,
    /// The video file, or the page that plays the video.
    pub url: String,
    pub thumbnail: Option<String>,
    /// The page from the user's list where this video was found.
    pub source: String,
    /// True when `url` is a video file; false when it is a page that yt-dlp must read.
    pub direct: bool,
    pub duration_seconds: Option<f64>,
}

fn selector(text: &str) -> Selector {
    Selector::parse(text).expect("a valid selector")
}

fn is_video_file(url: &Url) -> bool {
    let path = url.path().to_ascii_lowercase();
    path.rsplit_once('.')
        .is_some_and(|(_, extension)| VIDEO_EXTENSIONS.contains(&extension))
}

fn is_video_page(url: &Url, has_picture: bool) -> bool {
    let host = url.host_str().unwrap_or("").trim_start_matches("www.").to_ascii_lowercase();
    if VIDEO_HOSTS.iter().any(|known| host == *known || host.ends_with(&format!(".{known}"))) {
        return url.path().len() > 1;
    }
    let path = url.path().to_ascii_lowercase();
    has_picture && VIDEO_PATH_WORDS.iter().any(|word| path.contains(word))
}

fn clean(text: &str) -> Option<String> {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!text.is_empty()).then_some(text)
}

fn file_name(url: &Url) -> String {
    url.path_segments()
        .and_then(|mut segments| segments.next_back().map(str::to_string))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| url.host_str().unwrap_or("Video").to_string())
}

fn resolve(base: &Url, address: &str) -> Option<Url> {
    let address = address.trim();
    if address.is_empty() || address.starts_with("data:") || address.starts_with("javascript:") || address.starts_with('#') {
        return None;
    }
    base.join(address).ok().filter(|url| matches!(url.scheme(), "http" | "https"))
}

fn picture_of(base: &Url, element: ElementRef, images: &Selector) -> Option<String> {
    element.select(images).find_map(|image| {
        IMAGE_ATTRIBUTES
            .iter()
            .find_map(|name| image.value().attr(name).and_then(|address| resolve(base, address)))
            .map(String::from)
    })
}

fn meta(document: &Html, property: &str) -> Option<String> {
    document
        .select(&selector(&format!("meta[property='{property}'], meta[name='{property}']")))
        .find_map(|element| element.value().attr("content").and_then(clean))
}

/** Finds the videos in a page's HTML: video elements, links to video files, embedded players and links to video pages. */
pub fn parse_page(base: &Url, html: &str) -> Vec<VideoItem> {
    let document = Html::parse_document(html);
    let images = selector("img");
    let page_title = document
        .select(&selector("title"))
        .next()
        .and_then(|title| clean(&title.text().collect::<String>()));
    let page_picture = meta(&document, "og:image").and_then(|address| resolve(base, &address)).map(String::from);
    let source = base.to_string();
    let mut seen = HashSet::new();
    let mut items: Vec<VideoItem> = Vec::new();
    let mut add = |item: VideoItem| {
        if seen.insert(item.url.clone()) {
            items.push(item);
        }
    };

    for video in document.select(&selector("video")) {
        let sources = selector("source");
        let address = video
            .value()
            .attr("src")
            .or_else(|| video.select(&sources).find_map(|source| source.value().attr("src")));
        let Some(url) = address.and_then(|address| resolve(base, address)) else { continue };
        let title = ["title", "aria-label"]
            .iter()
            .find_map(|name| video.value().attr(name).and_then(clean))
            .or_else(|| page_title.clone())
            .unwrap_or_else(|| file_name(&url));
        let thumbnail = video
            .value()
            .attr("poster")
            .and_then(|address| resolve(base, address))
            .map(String::from)
            .or_else(|| page_picture.clone());
        add(VideoItem { title, url: url.into(), thumbnail, source: source.clone(), direct: true, duration_seconds: None });
    }

    if let Some(url) = ["og:video:secure_url", "og:video:url", "og:video"]
        .iter()
        .find_map(|name| meta(&document, name))
        .and_then(|address| resolve(base, &address))
    {
        let title = meta(&document, "og:title").or_else(|| page_title.clone()).unwrap_or_else(|| file_name(&url));
        let direct = is_video_file(&url);
        add(VideoItem { title, url: url.into(), thumbnail: page_picture.clone(), source: source.clone(), direct, duration_seconds: None });
    }

    for frame in document.select(&selector("iframe[src]")) {
        let Some(url) = frame.value().attr("src").and_then(|address| resolve(base, address)) else { continue };
        if !is_video_page(&url, false) {
            continue;
        }
        let title = frame.value().attr("title").and_then(clean).or_else(|| page_title.clone()).unwrap_or_else(|| file_name(&url));
        add(VideoItem { title, url: url.into(), thumbnail: None, source: source.clone(), direct: false, duration_seconds: None });
    }

    for link in document.select(&selector("a[href]")) {
        let Some(mut url) = link.value().attr("href").and_then(|address| resolve(base, address)) else { continue };
        url.set_fragment(None);
        let thumbnail = picture_of(base, link, &images);
        let direct = is_video_file(&url);
        if !direct && (!is_video_page(&url, thumbnail.is_some()) || url == *base) {
            continue;
        }
        let title = link
            .value()
            .attr("title")
            .and_then(clean)
            .or_else(|| link.select(&images).find_map(|image| image.value().attr("alt").and_then(clean)))
            .or_else(|| clean(&link.text().collect::<String>()))
            .unwrap_or_else(|| file_name(&url));
        add(VideoItem { title, url: url.into(), thumbnail, source: source.clone(), direct, duration_seconds: None });
    }

    items
}

/// Field names of a JSON item's title, the best first.
const JSON_TITLE_KEYS: [&str; 4] = ["title", "name", "label", "caption"];
/// Field names of a JSON item's address when the item has no video file, the best first.
const JSON_LINK_KEYS: [&str; 9] = ["url", "video_url", "link", "page_url", "webpage_url", "permalink", "embed_url", "embed", "href"];
/// Words in a field name that mark a picture.
const JSON_PICTURE_WORDS: [&str; 6] = ["thumb", "poster", "image", "img", "cover", "picture"];
/// Field names of a JSON item's length.
const JSON_DURATION_KEYS: [&str; 4] = ["duration", "length", "length_seconds", "duration_seconds"];
/// How many levels under an item are searched for its video file and its picture.
const JSON_DEPTH: usize = 4;

fn is_picture_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    JSON_PICTURE_WORDS.iter().any(|word| key.contains(word))
}

/** The first address under a JSON value that passes the test; picture fields are searched only when `pictures` is true. */
fn find_address(base: &Url, value: &serde_json::Value, depth: usize, pictures: bool, test: &dyn Fn(&Url) -> bool) -> Option<Url> {
    match value {
        serde_json::Value::String(text) => resolve(base, text).filter(|url| text.contains('/') && test(url)),
        serde_json::Value::Array(list) if depth > 0 => list.iter().find_map(|entry| find_address(base, entry, depth - 1, pictures, test)),
        serde_json::Value::Object(fields) if depth > 0 => fields
            .iter()
            .filter(|(key, _)| pictures || !is_picture_key(key))
            .find_map(|(_, entry)| find_address(base, entry, depth - 1, pictures, test)),
        _ => None,
    }
}

/** A length in seconds from a number, or from text such as "754", "12:34" or "1:02:03". */
fn json_duration(value: &serde_json::Value) -> Option<f64> {
    if let Some(seconds) = value.as_f64() {
        return Some(seconds);
    }
    let text = value.as_str()?.trim();
    text.split(':').try_fold(0.0, |total, part| part.trim().parse::<f64>().ok().map(|number| total * 60.0 + number))
}

/** A JSON object as a video, when it has a title and an address: a video file anywhere under it, else a link field. */
fn json_item(base: &Url, fields: &serde_json::Map<String, serde_json::Value>, source: &str) -> Option<VideoItem> {
    let title = JSON_TITLE_KEYS
        .iter()
        .find_map(|key| fields.get(*key).and_then(|value| value.as_str()).and_then(clean))?;
    let file = fields
        .iter()
        .filter(|(key, _)| !is_picture_key(key))
        .find_map(|(_, value)| find_address(base, value, JSON_DEPTH, false, &is_video_file));
    let direct = file.is_some();
    let url = file.or_else(|| {
        JSON_LINK_KEYS
            .iter()
            .find_map(|key| fields.get(*key).and_then(|value| value.as_str()).and_then(|address| resolve(base, address)))
    })?;
    let thumbnail = fields
        .iter()
        .filter(|(key, _)| is_picture_key(key))
        .find_map(|(_, value)| find_address(base, value, JSON_DEPTH, true, &|url| !is_video_file(url)))
        .map(String::from);
    let duration_seconds = JSON_DURATION_KEYS.iter().find_map(|key| fields.get(*key).and_then(json_duration));
    Some(VideoItem { title, url: url.into(), thumbnail, source: source.to_string(), direct, duration_seconds })
}

/**
 * Finds the videos in a JSON answer, such as a site's search API gives. The
 * layout is not known, so each object with a title and an address is a video;
 * other objects and lists are searched for such objects.
 */
pub fn parse_json(base: &Url, data: &serde_json::Value) -> Vec<VideoItem> {
    let source = base.to_string();
    let mut seen = HashSet::new();
    let mut items = Vec::new();
    let mut pending = vec![data];
    while let Some(value) = pending.pop() {
        match value {
            serde_json::Value::Array(list) => pending.extend(list.iter().rev()),
            serde_json::Value::Object(fields) => match json_item(base, fields, &source) {
                Some(item) => {
                    if seen.insert(item.url.clone()) {
                        items.push(item);
                    }
                }
                None => pending.extend(fields.values().rev()),
            },
            _ => {}
        }
    }
    items
}

fn text_of(value: &serde_json::Value, key: &str) -> Option<String> {
    value.get(key).and_then(|text| text.as_str()).and_then(clean)
}

fn listed_item(entry: &serde_json::Value, source: &str) -> Option<VideoItem> {
    let url = text_of(entry, "webpage_url").or_else(|| text_of(entry, "url"))?;
    let thumbnail = text_of(entry, "thumbnail").or_else(|| {
        entry
            .get("thumbnails")
            .and_then(|list| list.as_array())
            .and_then(|list| list.iter().rev().find_map(|picture| text_of(picture, "url")))
    });
    Some(VideoItem {
        title: text_of(entry, "title").unwrap_or_else(|| url.clone()),
        url,
        thumbnail,
        source: source.to_string(),
        direct: false,
        duration_seconds: entry.get("duration").and_then(|duration| duration.as_f64()),
    })
}

/// The most videos taken from one channel or playlist.
const LIST_LIMIT: &str = "200";

/** Asks yt-dlp for the videos of a page: one video, or each entry of a channel or playlist. */
async fn list_with_ytdlp(app: &AppHandle, url: &str) -> Result<Vec<VideoItem>, String> {
    let program = ytdlp::ensure(app).await.map_err(|error| format!("Could not get yt-dlp: {error:#}"))?;
    let output = ytdlp::command(&program)
        .args(["--dump-single-json", "--flat-playlist", "--no-warnings", "--playlist-end", LIST_LIMIT, "--", url])
        .output()
        .await
        .map_err(|error| format!("Could not run yt-dlp: {error}"))?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr);
        return Err(message.lines().last().unwrap_or("yt-dlp found no video").trim().to_string());
    }
    let data: serde_json::Value = serde_json::from_slice(&output.stdout).map_err(|error| format!("yt-dlp gave unreadable data: {error}"))?;
    let mut items = Vec::new();
    let mut pending = vec![&data];
    while let Some(value) = pending.pop() {
        match value.get("entries").and_then(|entries| entries.as_array()) {
            /* A channel's tabs are lists inside the list. */
            Some(entries) => pending.extend(entries.iter().rev()),
            None => items.extend(listed_item(value, url)),
        }
    }
    Ok(items)
}

/**
 * Finds the videos on one page of the user's list. The page's HTML is read
 * first; when that shows no video (a page built by script, or a site that
 * refuses the request), yt-dlp reads the page instead.
 */
#[tauri::command]
pub async fn scrape_videos(app: AppHandle, url: String) -> Result<Vec<VideoItem>, String> {
    let base = Url::parse(url.trim()).map_err(|_| format!("{url} is not a web address"))?;
    if !matches!(base.scheme(), "http" | "https") {
        return Err(format!("{url} is not a web address"));
    }
    let page = async {
        let response = reqwest::Client::builder()
            .user_agent(BROWSER_USER_AGENT)
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|error| error.to_string())?
            .get(base.clone())
            .header(reqwest::header::ACCEPT, "text/html,application/json;q=0.9,*/*;q=0.8")
            .send()
            .await
            .map_err(|error| format!("Could not read the page: {error}"))?;
        if !response.status().is_success() {
            return Err(format!("The site answered {}", response.status()));
        }
        let kind = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        /* The address is a video file itself. */
        if kind.starts_with("video/") || is_video_file(&base) {
            return Ok((vec![VideoItem { title: file_name(&base), url: base.to_string(), thumbnail: None, source: base.to_string(), direct: true, duration_seconds: None }], true));
        }
        /* The address after redirects is the base of the page's relative links. */
        let final_url = response.url().clone();
        let body = response.text().await.map_err(|error| format!("Could not read the page: {error}"))?;
        /* A JSON answer (a site's search API) is not sent to yt-dlp: it is complete as it is. */
        if kind.contains("json") || body.trim_start().starts_with(['{', '[']) {
            let data: serde_json::Value = serde_json::from_str(&body).map_err(|error| format!("The site's JSON answer is not readable: {error}"))?;
            let mut items = parse_json(&final_url, &data);
            for item in &mut items {
                item.source = base.to_string();
            }
            return Ok((items, true));
        }
        let mut items = parse_page(&final_url, &body);
        for item in &mut items {
            item.source = base.to_string();
        }
        Ok((items, false))
    }
    .await;

    match page {
        /* The second value is true when the answer is complete and yt-dlp has nothing to add. */
        Ok((items, complete)) if complete || !items.is_empty() => Ok(items),
        page => match list_with_ytdlp(&app, base.as_str()).await {
            Ok(items) => Ok(items),
            /* The page was read and has no video: that is an empty result, not an error. */
            Err(_) if page.is_ok() => Ok(Vec::new()),
            Err(error) => Err(match page {
                Err(page_error) => format!("{page_error}. yt-dlp: {error}"),
                Ok(_) => error,
            }),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r#"<html><head><title>Clips</title><meta property="og:image" content="/cover.jpg"></head><body>
        <video poster="poster.jpg"><source src="/media/one.mp4"></video>
        <a href="files/two.webm">Second clip</a>
        <a href="/watch/3"><img data-src="/thumbs/3.jpg" alt="Third clip"></a>
        <a href="/about"><img src="/logo.png"></a>
        <a href="https://www.youtube.com/watch?v=abc">On YouTube</a>
        <a href="files/two.webm">Again</a>
    </body></html>"#;

    #[test]
    fn finds_video_elements_files_and_pages() {
        let base = Url::parse("https://example.com/list/").unwrap();
        let items = parse_page(&base, PAGE);
        let found: Vec<(&str, &str, bool)> = items.iter().map(|item| (item.title.as_str(), item.url.as_str(), item.direct)).collect();
        assert_eq!(found, vec![
            ("Clips", "https://example.com/media/one.mp4", true),
            ("Second clip", "https://example.com/list/files/two.webm", true),
            ("Third clip", "https://example.com/watch/3", false),
            ("On YouTube", "https://www.youtube.com/watch?v=abc", false),
        ]);
        assert_eq!(items[0].thumbnail.as_deref(), Some("https://example.com/list/poster.jpg"));
        assert_eq!(items[2].thumbnail.as_deref(), Some("https://example.com/thumbs/3.jpg"));
    }

    #[test]
    fn finds_videos_in_a_json_answer() {
        let base = Url::parse("https://example.com/api/v2/video/search/?query=cat").unwrap();
        let data = serde_json::json!({
            "total": 2,
            "videos": [
                { "title": "First cat", "duration": "1:05", "thumbs": [{ "src": "https://cdn.example.com/1.jpg" }], "url": "https://example.com/video/1", "preview": { "mp4": "https://cdn.example.com/1.mp4" } },
                { "name": "Second cat", "length": 90, "default_thumb": "/t/2.jpg", "url": "/video/2" }
            ]
        });
        let items = parse_json(&base, &data);
        assert_eq!(items.len(), 2);
        assert_eq!((items[0].title.as_str(), items[0].url.as_str(), items[0].direct), ("First cat", "https://cdn.example.com/1.mp4", true));
        assert_eq!(items[0].thumbnail.as_deref(), Some("https://cdn.example.com/1.jpg"));
        assert_eq!(items[0].duration_seconds, Some(65.0));
        assert_eq!((items[1].url.as_str(), items[1].direct), ("https://example.com/video/2", false));
        assert_eq!(items[1].thumbnail.as_deref(), Some("https://example.com/t/2.jpg"));
        assert_eq!(items[1].duration_seconds, Some(90.0));
    }
}
