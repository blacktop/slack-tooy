use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use crate::message_links;
use crate::slack::client::SlackClient;
use crate::slack::types::SlackFile;
use color_eyre::eyre::{Result, WrapErr, bail, eyre};
use reqwest::Url;
use tempfile::TempDir;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

const MAX_SOURCES: usize = 16;
const MAX_MEDIA_BYTES: u64 = 512 * 1024 * 1024;
const DOWNLOAD_TIMEOUT: Duration = Duration::from_mins(10);
const PREVIEW_TIMEOUT: Duration = Duration::from_hours(2);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Source {
    url: Url,
    from_file_metadata: bool,
    name: String,
    extension_hint: Option<&'static str>,
    size: u64,
}

/// Capture the selected message's full-size files and distinct web links.
/// Content-Type is checked later, so extensionless media URLs work too.
pub fn sources(files: &[SlackFile], text: &str) -> Vec<Source> {
    let mut sources = Vec::new();
    for file in files {
        if sources.len() == MAX_SOURCES {
            break;
        }
        let hint = extension_for_mime(&file.mimetype.to_ascii_lowercase())
            .or_else(|| extension_hint(file.display_name()));
        let Some(url) = file.download_url().and_then(message_links::browser_url) else {
            continue;
        };
        if hint.is_some() && !sources.iter().any(|source: &Source| source.url == url) {
            sources.push(Source {
                url,
                from_file_metadata: true,
                name: file.display_name().chars().take(128).collect(),
                extension_hint: hint,
                size: file.size,
            });
        }
    }
    for url in message_links::links(text) {
        if sources.len() == MAX_SOURCES {
            break;
        }
        if sources.iter().any(|source| source.url == url) {
            continue;
        }
        let name = url.path().rsplit('/').next().unwrap_or("media");
        sources.push(Source {
            from_file_metadata: false,
            name: name.chars().take(128).collect(),
            extension_hint: extension_hint(name),
            size: 0,
            url,
        });
    }
    sources
}

struct PreparedMedia {
    directory: TempDir,
    paths: Vec<PathBuf>,
    unavailable: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct PreviewSummary {
    pub files: usize,
    pub unavailable: usize,
}

/// Keep preview files until the viewers close. Images use Quick Look; movies
/// use `QuickTime` because macOS 27.2's `qlmanage` movie viewer aborts in `AVKit`.
pub async fn open(client: SlackClient, sources: Vec<Source>) -> Result<PreviewSummary, String> {
    if !cfg!(target_os = "macos") {
        return Err("Media preview requires macOS; use d to save or o to open links".into());
    }
    let prepared = tokio::time::timeout(DOWNLOAD_TIMEOUT, prepare(&client, sources))
        .await
        .map_err(|_| "Media downloads timed out".to_string())?
        .map_err(|error| format!("{error:#}"))?;
    let (movies, images): (Vec<_>, Vec<_>) =
        prepared
            .paths
            .iter()
            .map(PathBuf::as_path)
            .partition(|path| {
                path.extension()
                    .and_then(|value| value.to_str())
                    .is_some_and(is_video_extension)
            });
    let mut image_command = Command::new("/usr/bin/qlmanage");
    image_command.arg("-p");
    let mut movie_command = Command::new("/usr/bin/open");
    movie_command.args(["-W", "-n", "-b", "com.apple.QuickTimePlayerX"]);
    let (image_result, movie_result) = tokio::join!(
        run_viewer(image_command, "Quick Look", &images),
        run_viewer(movie_command, "QuickTime Player", &movies),
    );
    prepared
        .directory
        .close()
        .map_err(|error| format!("Could not remove temporary preview files: {error}"))?;
    image_result.and(movie_result).map(|()| PreviewSummary {
        files: prepared.paths.len(),
        unavailable: prepared.unavailable,
    })
}

async fn run_viewer(mut command: Command, viewer: &str, paths: &[&Path]) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    let mut child = command
        // Media decoders do not need the terminal's tokens or other secrets.
        .env_clear()
        .args(paths)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("Could not start {viewer}: {error}"))?;
    match tokio::time::timeout(PREVIEW_TIMEOUT, child.wait()).await {
        Ok(Ok(status)) if status.success() => Ok(()),
        Ok(Ok(status)) => Err(format!("{viewer} exited with {status}")),
        Ok(Err(error)) => Err(format!("{viewer} failed: {error}")),
        Err(_) => match tokio::time::timeout(CLOSE_TIMEOUT, child.kill()).await {
            Ok(Ok(())) => Err(format!("{viewer} preview timed out")),
            Ok(Err(error)) => Err(format!("Could not close {viewer}: {error}")),
            Err(_) => Err(format!("Closing {viewer} timed out")),
        },
    }
}

fn is_video_extension(extension: &str) -> bool {
    matches!(
        extension,
        "mp4" | "mov" | "webm" | "mkv" | "mpg" | "avi" | "3gp" | "ogv"
    )
}

async fn prepare(client: &SlackClient, sources: Vec<Source>) -> Result<PreparedMedia> {
    let directory = tempfile::Builder::new()
        .prefix("slack-tooy-preview-")
        .tempdir()
        .wrap_err("Could not create a temporary preview directory")?;
    let mut prepared = PreparedMedia {
        directory,
        paths: Vec::new(),
        unavailable: 0,
    };
    let mut remaining = MAX_MEDIA_BYTES;
    let mut first_error = None;
    for (index, source) in sources.into_iter().take(MAX_SOURCES).enumerate() {
        match download_source(
            client,
            &source,
            prepared.directory.path(),
            index,
            &mut remaining,
        )
        .await
        {
            Ok(Some(path)) => prepared.paths.push(path),
            Ok(None) => {}
            Err(error) => {
                prepared.unavailable = prepared.unavailable.saturating_add(1);
                first_error.get_or_insert(error);
            }
        }
    }
    if prepared.paths.is_empty() {
        return Err(first_error.unwrap_or_else(|| {
            eyre!("No directly downloadable photo or video; use o to open web pages")
        }));
    }
    Ok(prepared)
}

async fn download_source(
    client: &SlackClient,
    source: &Source,
    directory: &Path,
    index: usize,
    remaining: &mut u64,
) -> Result<Option<PathBuf>> {
    if source.size > *remaining {
        bail!("Media exceeds the 512 MiB preview limit; use d to save it");
    }
    let mut response = client
        .media_request(&source.url, source.from_file_metadata)
        .send()
        .await
        .map_err(reqwest::Error::without_url)
        .wrap_err("Media request failed")?;
    if !response.status().is_success() {
        bail!("Media download returned HTTP {}", response.status());
    }
    let mime = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .filter(|value| value.len() <= 256)
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let extension = if matches!(
        mime.as_str(),
        "" | "application/octet-stream" | "binary/octet-stream"
    ) {
        source.extension_hint
    } else {
        extension_for_mime(&mime)
    };
    let Some(extension) = extension else {
        return Ok(None);
    };
    if response
        .content_length()
        .is_some_and(|size| size > *remaining)
    {
        bail!("Media exceeds the 512 MiB preview limit; use d to save it");
    }
    let path = directory.join(file_name(&source.name, index, extension));
    let mut out = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .await
        .wrap_err("Could not create a temporary preview file")?;
    let result = async {
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(reqwest::Error::without_url)?
        {
            let length =
                u64::try_from(chunk.len()).map_err(|_| eyre!("Media chunk is too large"))?;
            *remaining = remaining.checked_sub(length).ok_or_else(|| {
                eyre!("Media exceeds the 512 MiB preview limit; use d to save it")
            })?;
            out.write_all(&chunk).await?;
        }
        out.flush().await?;
        Ok::<(), color_eyre::Report>(())
    }
    .await;
    drop(out);
    if let Err(error) = result {
        let _ = tokio::fs::remove_file(&path).await;
        return Err(error);
    }
    Ok(Some(path))
}

fn file_name(name: &str, index: usize, extension: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let stem = Path::new(base)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("media");
    let stem: String = stem
        .chars()
        .filter(|ch| !ch.is_control())
        .take(40)
        .collect();
    let stem = stem.trim_matches([' ', '.']);
    let stem = if stem.is_empty() { "media" } else { stem };
    format!("{index:02}-{stem}.{extension}")
}

fn extension_hint(name: &str) -> Option<&'static str> {
    let extension = name.rsplit_once('.')?.1;
    if extension.len() > 8 {
        return None;
    }
    let extension = extension.to_ascii_lowercase();
    match extension.as_str() {
        "jpg" | "jpeg" => Some("jpg"),
        "png" => Some("png"),
        "gif" => Some("gif"),
        "webp" => Some("webp"),
        "avif" => Some("avif"),
        "heic" => Some("heic"),
        "heif" => Some("heif"),
        "tif" | "tiff" => Some("tiff"),
        "bmp" => Some("bmp"),
        "svg" => Some("svg"),
        "ico" => Some("ico"),
        "mp4" | "m4v" => Some("mp4"),
        "mov" => Some("mov"),
        "webm" => Some("webm"),
        "mkv" => Some("mkv"),
        "mpeg" | "mpg" => Some("mpg"),
        "avi" => Some("avi"),
        "3gp" => Some("3gp"),
        "ogv" => Some("ogv"),
        _ => None,
    }
}

fn extension_for_mime(mime: &str) -> Option<&'static str> {
    match mime {
        "image/jpeg" => Some("jpg"),
        "image/png" => Some("png"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        "image/avif" => Some("avif"),
        "image/heic" | "image/heic-sequence" => Some("heic"),
        "image/heif" | "image/heif-sequence" => Some("heif"),
        "image/tiff" => Some("tiff"),
        "image/bmp" | "image/x-ms-bmp" => Some("bmp"),
        "image/svg+xml" => Some("svg"),
        "image/x-icon" | "image/vnd.microsoft.icon" => Some("ico"),
        "video/mp4" | "video/x-m4v" => Some("mp4"),
        "video/quicktime" => Some("mov"),
        "video/webm" => Some("webm"),
        "video/x-matroska" => Some("mkv"),
        "video/mpeg" => Some("mpg"),
        "video/x-msvideo" => Some("avi"),
        "video/3gpp" => Some("3gp"),
        "video/ogg" => Some("ogv"),
        _ => None,
    }
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "test fixtures")]
mod tests {
    use crate::media_preview::{MAX_SOURCES, download_source, file_name, prepare, sources};
    use crate::slack::client::SlackClient;
    use crate::slack::types::SlackFile;
    use reqwest::Url;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    async fn server(responses: Vec<&'static str>) -> (Url, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let url = Url::parse(&format!(
            "http://{}",
            listener.local_addr().expect("address")
        ))
        .expect("URL");
        let server = tokio::spawn(async move {
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                for response in responses {
                    let (stream, _) = listener.accept().await.expect("accept");
                    let mut stream = BufReader::new(stream);
                    let mut bytes = 0;
                    loop {
                        let mut line = String::new();
                        assert!(stream.read_line(&mut line).await.expect("header") > 0);
                        bytes += line.len();
                        assert!(bytes < 8192);
                        let lower = line.to_ascii_lowercase();
                        assert!(!lower.starts_with("authorization:"));
                        assert!(!lower.starts_with("cookie:"));
                        if line == "\r\n" {
                            break;
                        }
                    }
                    stream
                        .get_mut()
                        .write_all(response.as_bytes())
                        .await
                        .expect("response");
                }
            })
            .await
            .expect("server timeout");
        });
        (url, server)
    }

    #[test]
    fn sources_prefer_full_files_deduplicate_links_and_bound_work() {
        let file: SlackFile = serde_json::from_value(serde_json::json!({
            "name":"clip.mov", "mimetype":"video/quicktime", "size":123,
            "url_private_download":"https://files.slack.com/full.mov",
            "thumb_1024":"https://files.slack.com/thumbnail.jpg"
        }))
        .expect("file");
        let text = format!(
            "<https://files.slack.com/full.mov|clip> {}",
            (0..30)
                .map(|i| format!("https://example.com/{i}"))
                .collect::<Vec<_>>()
                .join(" ")
        );
        let sources = sources(&[file], &text);
        assert_eq!(sources.len(), MAX_SOURCES);
        assert_eq!(sources[0].url.as_str(), "https://files.slack.com/full.mov");
        assert_eq!(sources[0].size, 123);
        assert!(sources[0].from_file_metadata);
        assert!(
            sources
                .iter()
                .skip(1)
                .all(|source| !source.from_file_metadata)
        );
        assert!(
            sources
                .iter()
                .all(|source| !source.url.path().contains("thumbnail"))
        );
        assert_eq!(file_name("../../bad\0.png", 1, "mp4"), "01-bad.mp4");
        assert!(file_name(&"🦀".repeat(200), 15, "mp4").len() < 255);
    }

    #[tokio::test]
    async fn downloads_extensionless_media_rejects_html_and_cleans_up() {
        let (url, server) = server(vec![
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 4\r\nConnection: close\r\n\r\npage",
            "HTTP/1.1 200 OK\r\nContent-Type: video/mp4\r\nContent-Length: 5\r\nConnection: close\r\n\r\nvideo",
        ]).await;
        let client = SlackClient::new("test-token", "test-cookie").expect("client");
        let candidates = sources(&[], &format!("{url}fake.png {url}clip"));
        let prepared = prepare(&client, candidates).await.expect("prepare");
        server.await.expect("server");
        assert_eq!(prepared.paths.len(), 1);
        assert_eq!(
            prepared.paths[0].extension().and_then(|ext| ext.to_str()),
            Some("mp4")
        );
        assert_eq!(std::fs::read(&prepared.paths[0]).expect("file"), b"video");
        let directory = prepared.directory.path().to_owned();
        drop(prepared);
        assert!(!directory.exists());
    }

    #[tokio::test]
    async fn rejects_declared_and_streamed_oversize_media_without_partial_files() {
        let (url, server) = server(vec![
            "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: 5\r\nConnection: close\r\n\r\nimage",
            "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\nimage\r\n0\r\n\r\n",
        ]).await;
        let client = SlackClient::new("test-token", "").expect("client");
        let source = sources(&[], url.as_str()).pop().expect("source");
        let directory = tempfile::tempdir().expect("tempdir");
        for index in 0..2 {
            let mut remaining = 3;
            let result =
                download_source(&client, &source, directory.path(), index, &mut remaining).await;
            assert!(result.is_err());
            assert_eq!(
                std::fs::read_dir(directory.path()).expect("files").count(),
                0
            );
        }
        server.await.expect("server");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn viewer_does_not_receive_terminal_secrets() {
        let mut command = tokio::process::Command::new("/bin/sh");
        command
            .args(["-c", r#"test -z "${SLACK_TOOY_PREVIEW_TEST_SECRET+x}""#])
            .env("SLACK_TOOY_PREVIEW_TEST_SECRET", "synthetic-test-secret");
        let result = crate::media_preview::run_viewer(
            command,
            "Test viewer",
            &[std::path::Path::new("preview-fixture")],
        )
        .await;
        assert!(result.is_ok(), "{result:?}");
    }
}
