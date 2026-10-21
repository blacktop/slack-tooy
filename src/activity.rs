use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

pub const MAX_ACTIVITY_ITEMS: usize = 300;
const PREVIEW_CHARS: usize = 360;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ThreadTarget {
    pub channel_id: String,
    pub thread_ts: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityEntry {
    pub target: ThreadTarget,
    pub root_text: String,
    pub root_user: String,
    pub latest_ts: String,
    pub reply_text: String,
    pub reply_user: String,
    pub foreign_ts: Option<String>,
    pub server_read_ts: String,
    pub local_read_ts: String,
    #[serde(default)]
    pub(crate) local_read_revision: u64,
    pub unread_count: usize,
    pub unread: bool,
}

#[derive(Debug, Clone)]
pub struct ActivitySnapshot {
    pub entries: Vec<ActivityEntry>,
    pub complete: bool,
    /// Request-start time, not Slack's future-valued `max_ts` field.
    pub started_ts: String,
    pub read_revision: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ActivityInbox {
    pub entries: Vec<ActivityEntry>,
    observed_after: Option<String>,
    #[serde(default)]
    read_revision: u64,
}

impl ActivityInbox {
    /// Reconcile a successful followed-thread sweep without losing retained items
    /// or treating an incomplete page set as proof that a thread was read.
    pub fn merge(&mut self, snapshot: ActivitySnapshot) -> Vec<ThreadTarget> {
        let mut previous: HashMap<_, _> = self
            .entries
            .drain(..)
            .map(|mut item| {
                if snapshot.complete {
                    item.unread = false;
                }
                (item.target.clone(), item)
            })
            .collect();
        let mut alerts = Vec::new();
        let mut entries = Vec::with_capacity(MAX_ACTIVITY_ITEMS);
        for mut item in snapshot.entries {
            let old = previous.remove(&item.target);
            if let Some(old) = &old
                && (item.server_read_ts >= old.server_read_ts
                    || snapshot.read_revision < old.local_read_revision)
            {
                item.local_read_ts.clone_from(&old.local_read_ts);
                item.local_read_revision = old.local_read_revision;
                if snapshot.read_revision < old.local_read_revision
                    && item.server_read_ts < old.server_read_ts
                {
                    item.server_read_ts.clone_from(&old.server_read_ts);
                }
            }
            item.unread =
                item.latest_ts > item.local_read_ts && item.latest_ts > item.server_read_ts;
            let since = old
                .as_ref()
                .map(|old| old.latest_ts.as_str())
                .or(self.observed_after.as_deref());
            if self.observed_after.is_some()
                && item.unread
                && item.foreign_ts.as_deref().is_some_and(|ts| {
                    since.is_some_and(|since| ts > since) && ts > item.local_read_ts.as_str()
                })
            {
                alerts.push(item.target.clone());
            }
            entries.push(item);
        }
        entries.extend(previous.into_values());
        // Keep unread work ahead of retained read history when storage is full.
        entries.sort_by(|a, b| {
            b.unread
                .cmp(&a.unread)
                .then_with(|| b.latest_ts.cmp(&a.latest_ts))
        });
        entries.truncate(MAX_ACTIVITY_ITEMS);
        entries.sort_by(|a, b| {
            b.latest_ts
                .cmp(&a.latest_ts)
                .then_with(|| a.target.channel_id.cmp(&b.target.channel_id))
                .then_with(|| a.target.thread_ts.cmp(&b.target.thread_ts))
        });
        self.entries = entries;
        if self
            .observed_after
            .as_ref()
            .is_none_or(|old| old < &snapshot.started_ts)
        {
            self.observed_after = Some(snapshot.started_ts);
        }
        alerts
    }

    pub fn read_revision(&self) -> u64 {
        self.read_revision
    }

    pub fn mark_read(&mut self, target: &ThreadTarget, ts: &str) -> bool {
        let Some(item) = self.entries.iter_mut().find(|item| item.target == *target) else {
            return false;
        };
        if ts <= item.local_read_ts.as_str() || ts <= item.server_read_ts.as_str() {
            return false;
        }
        item.local_read_ts = ts.to_string();
        self.read_revision = self.read_revision.saturating_add(1);
        item.local_read_revision = self.read_revision;
        item.unread = item.latest_ts.as_str() > ts && item.latest_ts > item.server_read_ts;
        true
    }

    pub fn confirm_read(&mut self, target: &ThreadTarget, ts: &str) {
        if let Some(item) = self.entries.iter_mut().find(|item| item.target == *target)
            && ts > item.server_read_ts.as_str()
        {
            item.server_read_ts = ts.to_string();
            self.read_revision = self.read_revision.saturating_add(1);
            item.local_read_revision = self.read_revision;
        }
    }

    pub fn unread_count(&self) -> usize {
        self.entries.iter().filter(|entry| entry.unread).count()
    }
}

pub fn timestamp_now() -> String {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}.{:06}", elapsed.as_secs(), elapsed.subsec_micros())
}

pub fn valid_timestamp(ts: &str) -> bool {
    let Some((seconds, micros)) = ts.split_once('.') else {
        return false;
    };
    seconds.len() == 10
        && micros.len() == 6
        && seconds
            .bytes()
            .chain(micros.bytes())
            .all(|b| b.is_ascii_digit())
}

pub fn valid_channel_id(id: &str) -> bool {
    (2..=32).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

pub fn preview(text: &str) -> String {
    text.chars()
        .take(PREVIEW_CHARS)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

#[cfg(test)]
pub(crate) fn test_entry(channel: &str, latest: &str) -> ActivityEntry {
    ActivityEntry {
        target: ThreadTarget {
            channel_id: channel.into(),
            thread_ts: "1700000000.000001".into(),
        },
        root_text: "Can someone review this?".into(),
        root_user: "UROOT".into(),
        latest_ts: latest.into(),
        reply_text: "Review is ready".into(),
        reply_user: "UOTHER".into(),
        foreign_ts: Some(latest.into()),
        server_read_ts: "1700000000.000001".into(),
        local_read_ts: String::new(),
        local_read_revision: 0,
        unread_count: 1,
        unread: true,
    }
}

#[cfg(test)]
#[expect(clippy::panic_in_result_fn, reason = "test assertions")]
mod tests {
    use crate::activity::{ActivityInbox, ActivitySnapshot, MAX_ACTIVITY_ITEMS, test_entry};

    fn snapshot(
        entries: Vec<crate::activity::ActivityEntry>,
        now: &str,
        complete: bool,
        revision: u64,
    ) -> ActivitySnapshot {
        ActivitySnapshot {
            entries,
            started_ts: now.into(),
            complete,
            read_revision: revision,
        }
    }

    #[test]
    fn baseline_restart_and_reordered_older_items_do_not_ring() -> color_eyre::Result<()> {
        let mut inbox = ActivityInbox::default();
        assert_eq!(
            inbox.merge(snapshot(vec![], "1700000100.000000", true, 0)),
            Vec::new()
        );
        let new = test_entry("C1", "1700000101.000000");
        assert_eq!(
            inbox.merge(snapshot(vec![new.clone()], "1700000102.000000", false, 0)),
            vec![new.target.clone()]
        );
        let mut restored: ActivityInbox = serde_json::from_str(&serde_json::to_string(&inbox)?)?;
        assert_eq!(
            restored.merge(snapshot(
                vec![new, test_entry("C2", "1700000099.000000")],
                "1700000103.000000",
                false,
                0
            )),
            Vec::new()
        );
        assert_eq!(restored.unread_count(), 2);
        Ok(())
    }

    #[test]
    fn foreign_reply_followed_by_own_reply_still_rings() {
        let mut inbox = ActivityInbox::default();
        inbox.merge(snapshot(
            vec![test_entry("C1", "1700000100.000000")],
            "1700000101.000000",
            true,
            0,
        ));
        let mut entry = test_entry("C1", "1700000103.000000");
        entry.reply_user = "USELF".into();
        entry.foreign_ts = Some("1700000102.000000".into());
        assert_eq!(
            inbox.merge(snapshot(vec![entry.clone()], "1700000104.000000", true, 0)),
            vec![entry.target.clone()]
        );
        entry.latest_ts = "1700000105.000000".into();
        assert_eq!(
            inbox.merge(snapshot(vec![entry], "1700000106.000000", true, 0)),
            Vec::new()
        );
    }

    #[test]
    fn stale_sweep_cannot_undo_read_but_later_mark_unread_can() {
        let mut inbox = ActivityInbox::default();
        let entry = test_entry("C1", "1700000100.000000");
        inbox.merge(snapshot(vec![entry.clone()], "1700000101.000000", true, 0));
        assert!(inbox.mark_read(&entry.target, &entry.latest_ts));
        let in_flight_revision = inbox.read_revision();
        inbox.confirm_read(&entry.target, &entry.latest_ts);
        inbox.merge(snapshot(
            vec![entry.clone()],
            "1700000102.000000",
            true,
            in_flight_revision,
        ));
        assert_eq!(inbox.unread_count(), 0);
        assert!(!inbox.mark_read(&entry.target, "1700000099.000000"));
        inbox.merge(snapshot(
            vec![entry],
            "1700000104.000000",
            true,
            inbox.read_revision(),
        ));
        assert_eq!(inbox.unread_count(), 1);
    }

    #[test]
    fn incomplete_refresh_preserves_unreads_and_complete_refresh_retains_history() {
        let mut inbox = ActivityInbox::default();
        inbox.merge(snapshot(
            vec![test_entry("C1", "1700000100.000000")],
            "1700000101.000000",
            true,
            0,
        ));
        inbox.merge(snapshot(vec![], "1700000102.000000", false, 0));
        assert_eq!(inbox.unread_count(), 1);
        inbox.merge(snapshot(vec![], "1700000103.000000", true, 0));
        assert_eq!(inbox.unread_count(), 0);
        assert_eq!(inbox.entries.len(), 1);
        let entries = (0..MAX_ACTIVITY_ITEMS + 20)
            .map(|i| test_entry(&format!("C{i}"), "1700000100.000000"))
            .collect();
        inbox.merge(snapshot(entries, "1700000104.000000", false, 0));
        assert_eq!(inbox.entries.len(), MAX_ACTIVITY_ITEMS);
    }
}
