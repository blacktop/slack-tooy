use std::process::Stdio;
use std::time::Duration;

use reqwest::Url;
use tokio::process::Command;

use crate::message_links::browser_url;

const OPEN_TIMEOUT: Duration = Duration::from_secs(10);

pub async fn open(url: Url) -> Result<(), String> {
    if browser_url(url.as_str()).is_none() {
        return Err("Unsupported browser URL".into());
    }
    #[cfg(target_os = "macos")]
    let mut command = Command::new("/usr/bin/open");
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("rundll32.exe");
        command.arg("url.dll,FileProtocolHandler");
        command
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut command = Command::new("xdg-open");
    command.arg(url.as_str());
    run(command, OPEN_TIMEOUT).await
}

async fn run(mut command: Command, timeout: Duration) -> Result<(), String> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("Could not start browser opener: {error}"))?;
    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) if status.success() => Ok(()),
        Ok(Ok(status)) => Err(format!("Browser opener exited with {status}")),
        Ok(Err(error)) => Err(format!("Browser opener failed: {error}")),
        Err(_) => {
            let cleanup = tokio::time::timeout(OPEN_TIMEOUT, child.kill()).await;
            match cleanup {
                Ok(Ok(())) => Err("Browser opener timed out".into()),
                Ok(Err(error)) => Err(format!("Browser opener cleanup failed: {error}")),
                Err(_) => Err("Browser opener cleanup timed out".into()),
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::time::Duration;

    use tokio::process::Command;

    use crate::browser::run;

    #[tokio::test]
    async fn observes_opener_success_failure_and_timeout() {
        assert!(
            run(Command::new("/usr/bin/true"), Duration::from_secs(2))
                .await
                .is_ok()
        );
        assert!(
            run(Command::new("/usr/bin/false"), Duration::from_secs(2))
                .await
                .is_err()
        );
        let mut sleep = Command::new("/bin/sleep");
        sleep.arg("30");
        assert_eq!(
            run(sleep, Duration::from_millis(20)).await,
            Err("Browser opener timed out".into())
        );
    }
}
