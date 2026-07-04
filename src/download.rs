use std::path::{Path, PathBuf};

use color_eyre::eyre::{Context, Result, bail, eyre};

use crate::slack::client::SlackClient;
use crate::slack::types::SlackFile;

const MAX_NAME_ATTEMPTS: u32 = 100;

/// Where downloads land: the platform download directory, falling
/// back to the home directory, then the current directory.
pub fn download_dir() -> PathBuf {
    dirs::download_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Download `file` into `dir` without overwriting existing files.
/// Returns the path the file was saved to.
///
/// Streams into a `.part` file and renames it into place only after
/// the download completes, so an interrupted download (error or app
/// quit) never leaves a truncated file under its final name.
pub async fn save_file(client: &SlackClient, file: &SlackFile, dir: &Path) -> Result<PathBuf> {
    let url = file
        .download_url()
        .ok_or_else(|| eyre!("File {} has no download URL", file.display_name()))?;

    tokio::fs::create_dir_all(dir)
        .await
        .wrap_err_with(|| format!("Failed to create download dir {}", dir.display()))?;

    let file_name = safe_file_name(file);
    let (part, mut out) = create_unique(dir, &format!("{file_name}.part")).await?;
    let downloaded = client.download_to(url, &mut out).await;
    drop(out);
    if let Err(e) = downloaded {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(e).wrap_err_with(|| format!("Failed to download {}", file.display_name()));
    }

    // Reserve a unique final name, then move the finished download
    // over the placeholder.
    let (dest, placeholder) = match create_unique(dir, &file_name).await {
        Ok(reserved) => reserved,
        Err(e) => {
            let _ = tokio::fs::remove_file(&part).await;
            return Err(e);
        }
    };
    drop(placeholder);
    // Synchronous rename: no await point between reserving the name
    // and filling it, so task cancellation can't leave the empty
    // placeholder behind.
    if let Err(e) = std::fs::rename(&part, &dest) {
        let _ = tokio::fs::remove_file(&part).await;
        let _ = tokio::fs::remove_file(&dest).await;
        return Err(e).wrap_err_with(|| format!("Failed to move download into {}", dest.display()));
    }
    Ok(dest)
}

/// Reduce the Slack-provided name to a single safe path component,
/// falling back to the file id.
fn safe_file_name(file: &SlackFile) -> String {
    sanitize_file_name(file.display_name())
        .or_else(|| sanitize_file_name(&file.id))
        .unwrap_or_else(|| "slack-file".to_string())
}

/// Strip directory components and reject names that would escape
/// the download directory.
fn sanitize_file_name(name: &str) -> Option<String> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let cleaned = base.replace('\0', "");
    let cleaned = cleaned.trim();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        None
    } else {
        Some(cleaned.to_string())
    }
}

/// Open a new file in `dir`, appending ` (n)` before the extension
/// while the name is taken.  `create_new` guarantees no overwrite.
async fn create_unique(dir: &Path, file_name: &str) -> Result<(PathBuf, tokio::fs::File)> {
    for attempt in 0..MAX_NAME_ATTEMPTS {
        let path = dir.join(numbered_name(file_name, attempt));
        match tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await
        {
            Ok(out) => return Ok((path, out)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => {
                return Err(e).wrap_err_with(|| format!("Failed to create {}", path.display()));
            }
        }
    }
    bail!(
        "No free name for {file_name} in {} after {MAX_NAME_ATTEMPTS} tries",
        dir.display()
    );
}

fn numbered_name(file_name: &str, attempt: u32) -> String {
    if attempt == 0 {
        return file_name.to_string();
    }
    let path = Path::new(file_name);
    match (
        path.file_stem().and_then(|stem| stem.to_str()),
        path.extension().and_then(|ext| ext.to_str()),
    ) {
        (Some(stem), Some(ext)) => format!("{stem} ({attempt}).{ext}"),
        _ => format!("{file_name} ({attempt})"),
    }
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;

    fn slack_file(name: &str, id: &str) -> SlackFile {
        SlackFile {
            id: id.into(),
            name: name.into(),
            title: String::new(),
            size: 0,
            mimetype: String::new(),
            url_private: String::new(),
            url_private_download: String::new(),
            thumb_360: String::new(),
            thumb_480: String::new(),
            thumb_720: String::new(),
            thumb_1024: String::new(),
        }
    }

    #[test]
    fn sanitize_file_name_strips_directory_components() {
        assert_eq!(
            sanitize_file_name("../../etc/passwd"),
            Some("passwd".to_string())
        );
        assert_eq!(
            sanitize_file_name(r"C:\evil\report.pdf"),
            Some("report.pdf".to_string())
        );
        assert_eq!(sanitize_file_name("cat.png"), Some("cat.png".to_string()));
    }

    #[test]
    fn sanitize_file_name_rejects_unusable_names() {
        assert_eq!(sanitize_file_name(""), None);
        assert_eq!(sanitize_file_name("."), None);
        assert_eq!(sanitize_file_name(".."), None);
        assert_eq!(sanitize_file_name("dir/"), None);
        assert_eq!(sanitize_file_name("   "), None);
    }

    #[test]
    fn safe_file_name_falls_back_to_id_then_placeholder() {
        assert_eq!(safe_file_name(&slack_file("cat.png", "F1")), "cat.png");
        assert_eq!(safe_file_name(&slack_file("", "F1")), "F1");
        assert_eq!(safe_file_name(&slack_file("", "")), "slack-file");
    }

    #[test]
    fn numbered_name_inserts_counter_before_extension() {
        assert_eq!(numbered_name("cat.png", 0), "cat.png");
        assert_eq!(numbered_name("cat.png", 2), "cat (2).png");
        assert_eq!(numbered_name("Makefile", 1), "Makefile (1)");
        assert_eq!(numbered_name(".env", 1), ".env (1)");
    }

    #[tokio::test]
    async fn create_unique_avoids_existing_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("cat.png"), b"first").expect("write");
        std::fs::write(dir.path().join("cat (1).png"), b"second").expect("write");

        let (path, _out) = create_unique(dir.path(), "cat.png")
            .await
            .expect("create unique");

        assert_eq!(path, dir.path().join("cat (2).png"));
        assert_eq!(
            std::fs::read(dir.path().join("cat.png")).expect("read"),
            b"first"
        );
    }
}
