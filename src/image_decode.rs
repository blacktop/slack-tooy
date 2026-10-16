use color_eyre::eyre::{Result, WrapErr, ensure, eyre};
use image::DynamicImage;

pub const MAX_ENCODED_BYTES: usize = 16 * 1024 * 1024;
const MAX_RGBA_BYTES: u64 = 16 * 1024 * 1024;
const MAX_DIMENSION: u32 = 4096;

#[cfg(target_os = "macos")]
pub async fn decode(data: Vec<u8>) -> Result<DynamicImage> {
    let executable = std::env::current_exe().wrap_err("Cannot locate the image helper")?;
    let parent = executable
        .parent()
        .ok_or_else(|| eyre!("Executable has no parent directory"))?;
    let helper = parent.join("slack-tooy-image-decoder");
    decode_with_command(
        sandbox_command(&helper)?,
        data,
        std::time::Duration::from_secs(15),
    )
    .await
}

#[cfg(target_os = "macos")]
fn sandbox_command(helper: &std::path::Path) -> Result<tokio::process::Command> {
    ensure!(
        helper.is_file(),
        "Isolated image helper is missing; run scripts/build-macos.sh"
    );
    let helper = helper
        .canonicalize()
        .wrap_err("Cannot resolve image helper path")?;
    let mut command = tokio::process::Command::new(helper);
    command
        .arg("--sandbox")
        .arg(include_str!("../macos/LocalDecoder/decoder.sb"));
    Ok(command)
}

#[cfg(not(target_os = "macos"))]
pub async fn decode(data: Vec<u8>) -> Result<DynamicImage> {
    ensure!(
        !data.is_empty() && data.len() <= MAX_ENCODED_BYTES,
        "Encoded image is empty or exceeds 16 MiB"
    );
    tokio::task::spawn_blocking(move || {
        let mut reader = image::ImageReader::new(std::io::Cursor::new(data))
            .with_guessed_format()
            .wrap_err("Cannot identify image format")?;
        ensure!(
            matches!(
                reader.format(),
                Some(image::ImageFormat::Png | image::ImageFormat::Jpeg)
            ),
            "Only JPEG and PNG images are supported"
        );
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(MAX_DIMENSION);
        limits.max_image_height = Some(MAX_DIMENSION);
        limits.max_alloc = Some(MAX_RGBA_BYTES);
        reader.limits(limits);
        let image = reader.decode().wrap_err("Failed to decode image")?;
        rgba_length(image.width(), image.height())?;
        Ok(image)
    })
    .await
    .wrap_err("Image decode task failed")?
}

fn rgba_length(width: u32, height: u32) -> Result<usize> {
    ensure!(
        width > 0 && height > 0 && width <= MAX_DIMENSION && height <= MAX_DIMENSION,
        "Image dimensions exceed the decoder limit"
    );
    let length = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(4))
        .filter(|length| *length <= MAX_RGBA_BYTES)
        .ok_or_else(|| eyre!("Decoded image exceeds 16 MiB"))?;
    usize::try_from(length).wrap_err("Image size does not fit this platform")
}

#[cfg(any(target_os = "macos", test))]
async fn read_response(mut reader: impl tokio::io::AsyncRead + Unpin) -> Result<DynamicImage> {
    use tokio::io::AsyncReadExt;

    let mut magic = [0; 4];
    reader
        .read_exact(&mut magic)
        .await
        .wrap_err("Image decoder returned no header")?;
    ensure!(&magic == b"STIM", "Invalid image decoder response");
    let width = reader.read_u32_le().await?;
    let height = reader.read_u32_le().await?;
    let declared_length = reader.read_u32_le().await?;
    let length = rgba_length(width, height)?;
    ensure!(
        u64::from(declared_length) == u64::try_from(length)?,
        "Invalid image decoder byte count"
    );
    let mut pixels = vec![0; length];
    reader
        .read_exact(&mut pixels)
        .await
        .wrap_err("Truncated image decoder response")?;
    ensure!(
        reader.read(&mut [0; 1]).await? == 0,
        "Image decoder returned trailing data"
    );
    let image = image::RgbaImage::from_raw(width, height, pixels)
        .ok_or_else(|| eyre!("Invalid decoded image buffer"))?;
    Ok(DynamicImage::ImageRgba8(image))
}

#[cfg(target_os = "macos")]
async fn decode_with_command(
    mut command: tokio::process::Command,
    data: Vec<u8>,
    timeout: std::time::Duration,
) -> Result<DynamicImage> {
    use std::process::Stdio;
    use tokio::io::AsyncWriteExt;

    ensure!(
        !data.is_empty() && data.len() <= MAX_ENCODED_BYTES,
        "Encoded image is empty or exceeds 16 MiB"
    );
    let input_length = u32::try_from(data.len())?;
    let mut child = command
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .wrap_err("Could not start isolated image decoder")?;
    let mut input = child
        .stdin
        .take()
        .ok_or_else(|| eyre!("Missing image decoder input"))?;
    let output = child
        .stdout
        .take()
        .ok_or_else(|| eyre!("Missing image decoder output"))?;
    let transfer = async {
        let write = async {
            input.write_all(&input_length.to_le_bytes()).await?;
            input.write_all(&data).await?;
            input.shutdown().await?;
            Ok::<(), color_eyre::eyre::Report>(())
        };
        let ((), image) = tokio::try_join!(write, read_response(output))?;
        let status = child
            .wait()
            .await
            .wrap_err("Could not observe image decoder exit")?;
        ensure!(status.success(), "Image decoder exited with {status}");
        Ok(image)
    };
    let result = match tokio::time::timeout(timeout, transfer).await {
        Ok(result) => result,
        Err(_) => Err(eyre!("Image decoder timed out")),
    };
    if let Err(error) = result {
        let cleanup = async {
            if child.try_wait()?.is_none() {
                child.kill().await?;
            }
            Ok::<(), std::io::Error>(())
        };
        return match tokio::time::timeout(std::time::Duration::from_secs(2), cleanup).await {
            Ok(Ok(())) => Err(error),
            Ok(Err(cleanup)) => {
                Err(error.wrap_err(format!("Image decoder cleanup failed: {cleanup}")))
            }
            Err(_) => Err(error.wrap_err("Image decoder cleanup timed out")),
        };
    }
    result
}

#[cfg(test)]
mod tests {
    use crate::image_decode::read_response;

    fn response(width: u32, height: u32, length: u32, pixels: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::from(*b"STIM");
        bytes.extend(width.to_le_bytes());
        bytes.extend(height.to_le_bytes());
        bytes.extend(length.to_le_bytes());
        bytes.extend(pixels);
        bytes
    }

    #[tokio::test]
    #[expect(clippy::expect_used, reason = "test fixtures")]
    async fn accepts_exact_rgba_and_rejects_untrusted_lengths() {
        let valid = response(1, 1, 4, &[10, 20, 30, 40]);
        let image = read_response(valid.as_slice()).await.expect("image");
        assert_eq!(image.into_rgba8().as_raw(), &[10, 20, 30, 40]);
        for invalid in [
            response(0, 1, 0, &[]),
            response(u32::MAX, u32::MAX, 0, &[]),
            response(4096, 4096, 0, &[]),
            response(1, 1, u32::MAX, &[]),
            response(1, 1, 4, &[0, 0]),
            response(1, 1, 4, &[0, 0, 0, 0, 0]),
            Vec::from(*b"NOPE"),
        ] {
            assert!(read_response(invalid.as_slice()).await.is_err());
        }
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn stops_a_decoder_that_never_responds() {
        let mut command = tokio::process::Command::new("/bin/sleep");
        command.arg("30");
        let result = crate::image_decode::decode_with_command(
            command,
            vec![1],
            std::time::Duration::from_millis(20),
        )
        .await;
        assert!(result.is_err());
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    #[ignore = "requires the built macOS helper and an unsandboxed terminal; scripts/build-macos.sh runs it"]
    #[expect(clippy::expect_used, reason = "native test fixture")]
    async fn native_decoder_round_trip() {
        let helper = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/release/slack-tooy-image-decoder");
        let mut encoded = std::io::Cursor::new(Vec::new());
        let input = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            1,
            1,
            image::Rgba([255, 0, 0, 128]),
        ));
        input
            .write_to(&mut encoded, image::ImageFormat::Png)
            .expect("encode PNG");
        let image = crate::image_decode::decode_with_command(
            crate::image_decode::sandbox_command(&helper).expect("sandbox command"),
            encoded.into_inner(),
            std::time::Duration::from_secs(15),
        )
        .await
        .expect("native image decoder");
        assert_eq!(image.into_rgba8().as_raw(), &[255, 0, 0, 128]);
    }
}
