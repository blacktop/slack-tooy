use std::collections::HashSet;
use std::time::Duration;

use color_eyre::eyre::{Result, WrapErr, bail};
use serde::Deserialize;

use crate::activity::{
    ActivityEntry, ActivitySnapshot, MAX_ACTIVITY_ITEMS, ThreadTarget, preview, timestamp_now,
    valid_channel_id, valid_timestamp,
};
use crate::slack::client::SlackClient;

const METHOD: &str = "subscriptions.thread.getView";

#[derive(Deserialize)]
struct ThreadPage {
    threads: Vec<ThreadView>,
    has_more: bool,
}

#[derive(Deserialize)]
struct ThreadView {
    root_msg: RootMessage,
    #[serde(default)]
    latest_replies: Vec<Reply>,
    #[serde(default)]
    unread_replies: Vec<Reply>,
}

#[derive(Deserialize)]
struct RootMessage {
    channel: String,
    latest_reply: String,
    #[serde(default)]
    last_read: String,
    #[serde(default)]
    subscribed: Option<bool>,
    #[serde(flatten)]
    message: Reply,
}

#[derive(Deserialize)]
struct Reply {
    ts: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    user: String,
}

impl ThreadView {
    fn into_entry(self, self_id: &str) -> Result<Option<ActivityEntry>> {
        let root = self.root_msg;
        if !valid_channel_id(&root.channel)
            || !valid_timestamp(&root.message.ts)
            || !valid_timestamp(&root.latest_reply)
            || (!root.last_read.is_empty()
                && root.last_read != "0"
                && !valid_timestamp(&root.last_read))
        {
            bail!("Slack returned an invalid thread identity");
        }
        if root.subscribed == Some(false) {
            return Ok(None);
        }
        let foreign_ts = self
            .unread_replies
            .iter()
            .chain(&self.latest_replies)
            .filter(|reply| reply.user != self_id && reply.ts > root.last_read)
            .map(|reply| &reply.ts)
            .filter(|ts| valid_timestamp(ts))
            .max()
            .cloned();
        let newest = self
            .latest_replies
            .iter()
            .chain(&self.unread_replies)
            .filter(|reply| valid_timestamp(&reply.ts))
            .max_by(|a, b| a.ts.cmp(&b.ts));
        Ok(Some(ActivityEntry {
            target: ThreadTarget {
                channel_id: root.channel,
                thread_ts: root.message.ts,
            },
            root_text: preview(&root.message.text),
            root_user: preview(&root.message.user),
            latest_ts: root.latest_reply,
            reply_text: newest.map_or_else(String::new, |reply| preview(&reply.text)),
            reply_user: newest.map_or_else(String::new, |reply| preview(&reply.user)),
            foreign_ts,
            server_read_ts: root.last_read,
            local_read_ts: String::new(),
            local_read_revision: 0,
            unread_count: self.unread_replies.len(),
            unread: true,
        }))
    }
}

impl SlackClient {
    pub async fn fetch_thread_activity(&self, self_id: &str) -> Result<ActivitySnapshot> {
        tokio::time::timeout(
            Duration::from_secs(90),
            self.fetch_thread_activity_pages(self_id),
        )
        .await
        .wrap_err("Thread Activity refresh timed out")?
    }

    async fn fetch_thread_activity_pages(&self, self_id: &str) -> Result<ActivitySnapshot> {
        let mut snapshot = ActivitySnapshot {
            entries: Vec::new(),
            complete: false,
            started_ts: timestamp_now(),
            read_revision: 0,
        };
        let mut cursor = String::new();
        let mut seen = HashSet::new();
        for _ in 0..30 {
            let mut params = vec![
                ("limit", "100"),
                ("priority_mode", "all"),
                ("fetch_threads_state", "true"),
            ];
            if !cursor.is_empty() {
                params.push(("current_ts", &cursor));
            }
            let page: ThreadPage = self.post_urlencoded(METHOD, &params).await?;
            if page.threads.len() > 100 {
                bail!("Thread Activity page exceeds the 100-item limit");
            }
            let next = page
                .threads
                .last()
                .map(|item| item.root_msg.latest_reply.clone());
            let mut omitted = false;
            for thread in page.threads {
                if let Some(entry) = thread.into_entry(self_id)?
                    && !seen.contains(&entry.target)
                {
                    if snapshot.entries.len() < MAX_ACTIVITY_ITEMS {
                        seen.insert(entry.target.clone());
                        snapshot.entries.push(entry);
                    } else {
                        omitted = true;
                    }
                }
            }
            if !page.has_more {
                snapshot.complete = !omitted;
                return Ok(snapshot);
            }
            let Some(next) =
                next.filter(|ts| valid_timestamp(ts) && (cursor.is_empty() || ts < &cursor))
            else {
                bail!("Thread Activity pagination cursor did not advance");
            };
            cursor = next;
            if snapshot.entries.len() >= MAX_ACTIVITY_ITEMS {
                return Ok(snapshot);
            }
        }
        Ok(snapshot)
    }

    pub async fn mark_thread_read(&self, target: &ThreadTarget, ts: &str) -> Result<()> {
        if !valid_channel_id(&target.channel_id)
            || !valid_timestamp(&target.thread_ts)
            || !valid_timestamp(ts)
        {
            bail!("Invalid thread read target");
        }
        let _: serde_json::Value = self
            .post_urlencoded(
                "subscriptions.thread.mark",
                &[
                    ("channel", &target.channel_id),
                    ("thread_ts", &target.thread_ts),
                    ("ts", ts),
                    ("read", "true"),
                ],
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
#[expect(clippy::panic_in_result_fn, reason = "test assertions")]
mod tests {
    use crate::activity::ThreadTarget;
    use crate::slack::client::SlackClient;
    use serde_json::{Value, json};
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

    async fn server(
        responses: Vec<Value>,
    ) -> color_eyre::Result<(
        SlackClient,
        tokio::task::JoinHandle<color_eyre::Result<Vec<String>>>,
    )> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let client = SlackClient::with_api_base(format!("http://{}", listener.local_addr()?))?;
        let task = tokio::spawn(async move {
            tokio::time::timeout(std::time::Duration::from_secs(5), async move {
                let mut requests = Vec::new();
                for response in responses {
                    let (stream, _) = listener.accept().await?;
                    let mut stream = BufReader::new(stream);
                    let mut headers = String::new();
                    let mut length = 0;
                    loop {
                        let mut line = String::new();
                        let count = stream.read_line(&mut line).await?;
                        color_eyre::eyre::ensure!(count > 0 && headers.len() < 8192);
                        if line == "\r\n" { break; }
                        if let Some((name, value)) = line.split_once(':')
                            && name.eq_ignore_ascii_case("content-length") { length = value.trim().parse::<usize>()?; }
                        headers.push_str(&line);
                    }
                    color_eyre::eyre::ensure!(length <= 4096);
                    let mut body = vec![0; length];
                    stream.read_exact(&mut body).await?;
                    requests.push(format!("{headers}\n{}", String::from_utf8(body)?));
                    let response = response.to_string();
                    stream.get_mut().write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).as_bytes()).await?;
                }
                Ok(requests)
            }).await?
        });
        Ok((client, task))
    }

    fn item(index: usize, latest: &str) -> Value {
        json!({"root_msg":{"channel":format!("C{index}"),"ts":"1700000000.000001","latest_reply":latest,"last_read":"1700000000.000001","subscribed":true,"user":"UROOT","text":"Root"},"latest_replies":[{"ts":latest,"user":"USELF","text":"Own follow-up"}],"unread_replies":[{"ts":"1700000102.000000","user":"UOTHER","text":"Reply"},{"ts":latest,"user":"USELF","text":"Own follow-up"}]})
    }

    #[tokio::test]
    async fn pages_using_latest_reply_and_marks_exact_thread() -> color_eyre::Result<()> {
        let (client, requests) = server(vec![
            json!({"ok":true,"max_ts":"9999999999.000000","has_more":true,"threads":[item(1,"1700000103.000000")]}),
            json!({"ok":true,"has_more":false,"threads":[item(2,"1700000101.000000")]}),
            json!({"ok":true}),
        ]).await?;
        let snapshot = client.fetch_thread_activity("USELF").await?;
        assert!(snapshot.complete);
        assert_eq!(snapshot.entries.len(), 2);
        assert_eq!(
            snapshot.entries[0].foreign_ts.as_deref(),
            Some("1700000102.000000")
        );
        client
            .mark_thread_read(
                &ThreadTarget {
                    channel_id: "C1".into(),
                    thread_ts: "1700000000.000001".into(),
                },
                "1700000103.000000",
            )
            .await?;
        let requests = requests.await??;
        assert!(requests[0].contains("application/x-www-form-urlencoded"));
        assert!(requests[0].contains("priority_mode=all"));
        assert!(!requests[0].contains("current_ts="));
        assert!(requests[1].contains("current_ts=1700000103.000000"));
        assert!(!requests[1].contains("9999999999"));
        assert!(requests[2].contains("POST /subscriptions.thread.mark "));
        assert!(
            requests[2]
                .contains("channel=C1&thread_ts=1700000000.000001&ts=1700000103.000000&read=true")
        );
        Ok(())
    }

    #[tokio::test]
    async fn repeated_cursor_is_an_error_and_overflow_is_incomplete() -> color_eyre::Result<()> {
        let page = json!({"ok":true,"has_more":true,"threads":[item(1,"1700000103.000000")]});
        let (client, requests) = server(vec![page.clone(), page]).await?;
        assert!(client.fetch_thread_activity("USELF").await.is_err());
        requests.await??;
        let pages = [100,100,99,2].into_iter().enumerate().map(|(page,count)| {
            let latest = format!("1700000{}.000000", 900-page);
            json!({"ok":true,"has_more":page < 3,"threads":(0..count).map(|i|item(page*100+i,&latest)).collect::<Vec<_>>()})
        }).collect();
        let (client, requests) = server(pages).await?;
        let snapshot = client.fetch_thread_activity("USELF").await?;
        assert_eq!(snapshot.entries.len(), 300);
        assert!(!snapshot.complete);
        requests.await??;
        Ok(())
    }
}
