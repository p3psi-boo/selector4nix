use std::sync::Arc;

use serde::Serialize;

use crate::domain::common::url::Url;
use crate::domain::nar_info::model::NarFileName;
use crate::infrastructure::metric::{NarTransferMetric, NarTransferMetricEntry};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct TransferringData {
    pub files: Vec<TransferringFileItemData>,
    pub recent: Vec<TransferringFileItemData>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct TransferringFileItemData {
    pub id: u64,
    pub bytes_per_second: u64,
    pub remaining_secs: Option<u64>,
    pub outcome: Option<&'static str>,
    pub package_name: Option<String>,
    pub nar_file_name: NarFileName,
    pub substituter_url: Url,
    pub bytes_total: Option<u64>,
    pub bytes_transferred: u64,
    pub elapsed_secs: u64,
    pub started_at_unix_ms: u64,
}

pub struct GetDashboardTransferringUseCase {
    nar_transfer_metric: Arc<NarTransferMetric>,
}

impl GetDashboardTransferringUseCase {
    pub fn new(nar_transfer_metric: Arc<NarTransferMetric>) -> Self {
        Self {
            nar_transfer_metric,
        }
    }

    pub async fn run(&self) -> TransferringData {
        TransferringData {
            files: self
                .nar_transfer_metric
                .transferring()
                .into_iter()
                .map(Self::item)
                .collect(),
            recent: self
                .nar_transfer_metric
                .recent()
                .into_iter()
                .map(Self::item)
                .collect(),
        }
    }

    fn item(e: NarTransferMetricEntry) -> TransferringFileItemData {
        let bytes_per_second = e.bytes_per_second();
        let elapsed_secs = e.elapsed_secs();
        TransferringFileItemData {
            id: e.id,
            bytes_per_second,
            remaining_secs: e
                .meta
                .content_length
                .filter(|_| bytes_per_second > 0)
                .map(|total| {
                    total
                        .saturating_sub(e.bytes_transferred)
                        .div_ceil(bytes_per_second)
                }),
            outcome: e.outcome,
            package_name: e.meta.store_path.and_then(|s| Self::parse_package_name(&s)),
            nar_file_name: e.meta.nar_file_name,
            substituter_url: e.meta.substituter_url,
            bytes_total: e.meta.content_length,
            bytes_transferred: e.bytes_transferred,
            elapsed_secs,
            started_at_unix_ms: e.started_at_unix_ms,
        }
    }

    fn parse_package_name(store_path: &str) -> Option<String> {
        let (_, without_store_dir) = store_path.rsplit_once('/')?;
        let hash_and_name = without_store_dir.split('/').next().unwrap();
        let (_, package_name) = hash_and_name.split_once('-')?;
        Some(package_name.to_string())
    }
}
