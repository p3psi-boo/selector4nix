use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use dashmap::DashMap;

use crate::domain::common::url::Url;
use crate::domain::nar_info::model::NarFileName;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct NarTransferId(u64);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NarTransferMeta {
    pub nar_file_name: NarFileName,
    pub store_path: Option<String>,
    pub substituter_url: Url,
    pub source_url: Url,
    pub content_length: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NarTransferMetricEntry {
    pub id: u64,
    pub meta: NarTransferMeta,
    pub bytes_transferred: u64,
    pub started_at_unix_ms: u64,
    pub outcome: Option<&'static str>,
    started: Instant,
    ended: Option<Instant>,
    samples: VecDeque<(Instant, u64)>,
}

pub struct NarTransferMetric {
    next_id: AtomicU64,
    transferring: DashMap<NarTransferId, NarTransferMetricEntry>,
    recent: Mutex<VecDeque<NarTransferMetricEntry>>,
}

impl NarTransferMetric {
    pub fn new() -> Self {
        Self {
            next_id: AtomicU64::new(0),
            transferring: DashMap::new(),
            recent: Mutex::new(VecDeque::new()),
        }
    }

    pub fn begin(self: &Arc<Self>, meta: NarTransferMeta) -> NarTransferHandle {
        let id = NarTransferId(self.next_id.fetch_add(1, Ordering::Relaxed));

        let started_at_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        let started = Instant::now();
        self.transferring.insert(
            id,
            NarTransferMetricEntry {
                id: id.0,
                meta,
                outcome: None,
                started,
                ended: None,
                samples: VecDeque::from([(started, 0)]),
                bytes_transferred: 0,
                started_at_unix_ms,
            },
        );

        NarTransferHandle {
            metric: Arc::clone(self),
            id,
        }
    }

    pub fn transferring(&self) -> Vec<NarTransferMetricEntry> {
        let mut items: Vec<NarTransferMetricEntry> = self
            .transferring
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        items.sort_by_key(|item| item.started_at_unix_ms);
        items
    }

    pub fn transferring_count(&self) -> usize {
        self.transferring.len()
    }

    pub fn recent(&self) -> Vec<NarTransferMetricEntry> {
        self.recent.lock().unwrap().iter().rev().cloned().collect()
    }

    fn finish(&self, id: NarTransferId, outcome: &'static str) {
        if let Some((_, mut entry)) = self.transferring.remove(&id) {
            entry.ended = Some(Instant::now());
            // HTTP bodies with a known length may be dropped without polling EOF.
            let outcome = if outcome == "Cancelled: client disconnected"
                && entry.meta.content_length == Some(entry.bytes_transferred)
            {
                "Completed"
            } else {
                outcome
            };
            entry.outcome = Some(
                if outcome == "Completed"
                    && entry
                        .meta
                        .content_length
                        .is_some_and(|total| total != entry.bytes_transferred)
                {
                    "Failed: incomplete response"
                } else {
                    outcome
                },
            );
            let mut recent = self.recent.lock().unwrap();
            recent.push_back(entry);
            if recent.len() > 50 {
                recent.pop_front();
            }
        }
    }
}

pub struct NarTransferHandle {
    metric: Arc<NarTransferMetric>,
    id: NarTransferId,
}

impl NarTransferMetricEntry {
    pub fn elapsed_secs(&self) -> u64 {
        self.ended
            .unwrap_or_else(Instant::now)
            .duration_since(self.started)
            .as_secs()
    }

    pub fn bytes_per_second(&self) -> u64 {
        let now = self.ended.unwrap_or_else(Instant::now);
        let Some((at, bytes)) = self
            .samples
            .iter()
            .find(|(at, _)| now.duration_since(*at) <= Duration::from_secs(5))
        else {
            return 0;
        };
        let elapsed = now.duration_since(*at).as_secs_f64();
        if elapsed < 0.001 {
            return 0;
        }
        ((self.bytes_transferred - bytes) as f64 / elapsed) as u64
    }
}

impl NarTransferHandle {
    pub fn complete(&self) {
        self.metric.finish(self.id, "Completed");
    }
    pub fn fail(&self) {
        self.metric.finish(self.id, "Failed: stream error");
    }

    pub fn record_bytes(&self, bytes: u64) {
        if let Some(mut entry) = self.metric.transferring.get_mut(&self.id) {
            entry.bytes_transferred += bytes;
            let now = Instant::now();
            let transferred = entry.bytes_transferred;
            if entry
                .samples
                .back()
                .is_none_or(|(at, _)| now.duration_since(*at) >= Duration::from_millis(250))
            {
                entry.samples.push_back((now, transferred));
            }
            while entry.samples.len() > 1
                && now.duration_since(entry.samples[1].0) > Duration::from_secs(5)
            {
                entry.samples.pop_front();
            }
        };
    }
}

impl Drop for NarTransferHandle {
    fn drop(&mut self) {
        self.metric
            .finish(self.id, "Cancelled: client disconnected");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(total: Option<u64>) -> NarTransferMeta {
        NarTransferMeta {
            nar_file_name: NarFileName::new("test.nar.xz".into()).unwrap(),
            store_path: None,
            substituter_url: Url::new("https://cache.example.org/").unwrap(),
            source_url: Url::new("https://cache.example.org/nar/test.nar.xz").unwrap(),
            content_length: total,
        }
    }

    #[test]
    fn history_distinguishes_completion_failure_and_cancellation() {
        let metric = Arc::new(NarTransferMetric::new());
        let complete = metric.begin(meta(Some(10)));
        complete.record_bytes(10);
        complete.complete();
        drop(complete);
        let failed = metric.begin(meta(None));
        failed.fail();
        drop(failed);
        drop(metric.begin(meta(None)));
        let short = metric.begin(meta(Some(10)));
        short.record_bytes(3);
        short.complete();
        drop(short);
        assert_eq!(metric.transferring_count(), 0);
        let outcomes: Vec<_> = metric.recent().iter().map(|e| e.outcome.unwrap()).collect();
        assert_eq!(
            outcomes,
            [
                "Failed: incomplete response",
                "Cancelled: client disconnected",
                "Failed: stream error",
                "Completed"
            ]
        );
    }

    #[test]
    fn fully_consumed_known_length_body_can_finish_without_polling_eof() {
        let metric = Arc::new(NarTransferMetric::new());
        let handle = metric.begin(meta(Some(10)));
        handle.record_bytes(10);
        drop(handle);
        assert_eq!(metric.recent()[0].outcome, Some("Completed"));
    }
    #[test]
    fn history_keeps_only_the_latest_fifty_results() {
        let metric = Arc::new(NarTransferMetric::new());
        for _ in 0..60 {
            metric.begin(meta(None)).complete();
        }
        let recent = metric.recent();
        assert_eq!(recent.len(), 50);
        assert_eq!(recent.first().unwrap().id, 59);
        assert_eq!(recent.last().unwrap().id, 10);
    }

    #[test]
    fn throughput_uses_recent_bytes_and_stalls_decay_to_zero() {
        let metric = Arc::new(NarTransferMetric::new());
        let handle = metric.begin(meta(None));
        handle.record_bytes(2048);
        let mut entry = metric.transferring().pop().unwrap();
        let now = Instant::now();
        entry.ended = Some(now);
        entry.samples = VecDeque::from([(now - Duration::from_secs(2), 0)]);
        assert_eq!(entry.bytes_per_second(), 1024);
        entry.ended = Some(now + Duration::from_secs(6));
        assert_eq!(entry.bytes_per_second(), 0);
    }
}
