use std::collections::VecDeque;

use color_eyre::eyre::Result;
use futures::future::BoxFuture;
use futures::stream::FuturesUnordered;
use futures::{FutureExt, StreamExt};

use crate::action::Action;
use crate::image_decode;
use crate::message_links::browser_url;
use crate::slack::client::SlackClient;

const MAX_REQUESTS: usize = 256;
const MAX_TARGET_BYTES: usize = 8192;
const CONCURRENT_DECODES: usize = 2;

pub enum Target {
    Avatar(String),
    File(String),
}

/// The TUI owns the waiting queue and polls at most two download/decode jobs.
/// Waiting jobs hold no image data or child processes.
#[derive(Default)]
pub struct ImagePipeline {
    waiting: VecDeque<BoxFuture<'static, Action>>,
    active: FuturesUnordered<BoxFuture<'static, Action>>,
}

impl ImagePipeline {
    pub fn remaining_capacity(&self) -> usize {
        MAX_REQUESTS
            .saturating_sub(self.waiting.len())
            .saturating_sub(self.active.len())
    }

    pub fn enqueue(&mut self, client: &SlackClient, target: Target, url: &str) -> bool {
        let key = match &target {
            Target::Avatar(key) | Target::File(key) => key,
        };
        if key.len() > MAX_TARGET_BYTES || self.remaining_capacity() == 0 {
            return false;
        }
        let Some(url) = browser_url(url) else {
            return false;
        };
        let client = client.clone();
        self.waiting.push_back(
            async move {
                let result: Result<_> = async {
                    let data = client
                        .download_image_bytes(url.as_str(), image_decode::MAX_ENCODED_BYTES)
                        .await?;
                    image_decode::decode(data).await
                }
                .await;
                match (target, result) {
                    (Target::Avatar(user_id), Ok(image)) => Action::AvatarDownloaded {
                        user_id,
                        image: Box::new(image),
                    },
                    (Target::File(image_key), Ok(image)) => Action::FileImageDownloaded {
                        image_key,
                        image: Box::new(image),
                    },
                    (Target::Avatar(_), Err(error)) => {
                        tracing::warn!(%error, "Avatar image unavailable");
                        Action::Render
                    }
                    (Target::File(image_key), Err(error)) => {
                        tracing::warn!(%error, "Inline image unavailable");
                        Action::FileImageFailed { image_key }
                    }
                }
            }
            .boxed(),
        );
        true
    }

    pub async fn next(&mut self) -> Option<Action> {
        while self.active.len() < CONCURRENT_DECODES {
            let Some(request) = self.waiting.pop_front() else {
                break;
            };
            self.active.push(request);
        }
        self.active.next().await
    }
}

#[cfg(test)]
mod tests {
    use crate::image_pipeline::{ImagePipeline, MAX_REQUESTS, MAX_TARGET_BYTES, Target};
    use crate::slack::client::SlackClient;

    #[test]
    #[expect(clippy::expect_used, reason = "test fixtures")]
    fn image_queue_rejects_invalid_urls_and_excess_work_before_fetching() {
        let client = SlackClient::new("test-token", "").expect("client");
        let mut pipeline = ImagePipeline::default();
        assert!(!pipeline.enqueue(
            &client,
            Target::Avatar("U1".into()),
            "file:///tmp/image.png"
        ));
        assert!(!pipeline.enqueue(
            &client,
            Target::File("x".repeat(MAX_TARGET_BYTES + 1)),
            "https://example.com/image.png"
        ));
        for index in 0..MAX_REQUESTS {
            assert!(pipeline.enqueue(
                &client,
                Target::File(index.to_string()),
                "https://example.com/image.png"
            ));
        }
        assert_eq!(pipeline.remaining_capacity(), 0);
        assert!(!pipeline.enqueue(
            &client,
            Target::Avatar("U2".into()),
            "https://example.com/image.png"
        ));
    }
}
