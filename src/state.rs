use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pending,
    Running,
    Done,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    pub label: String,
    pub status: Status,
    pub pct: f64,
    pub attempts: u32,
    pub updated: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Store {
    pub url: String,
    pub items: BTreeMap<String, Item>,
}

impl Store {
    pub fn path(dir: &Path, key: &str) -> PathBuf {
        dir.join(format!("{key}.json"))
    }

    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn touch(&mut self, label: &str, status: Status, pct: f64, bump_attempt: bool) {
        let entry = self.items.entry(label.to_string()).or_insert_with(|| Item {
            label: label.to_string(),
            status: Status::Pending,
            pct: 0.0,
            attempts: 0,
            updated: String::new(),
        });
        entry.status = status;
        entry.pct = pct;
        if bump_attempt {
            entry.attempts += 1;
        }
        entry.updated = chrono::Local::now().to_rfc3339();
    }

    /// 把"试过但没学完"的项重新放回可重试状态。
    ///
    /// advance 会跳过 attempts 到上限的项，避免在同一门上死磕。可当整页入口都被
    /// 跳过时，程序会误判"没有可进入的学习项"直接收摊——实际上只是每门都试过几次。
    /// 这里清一次计数，让它再来一轮。
    pub fn reset_stalled(&mut self, threshold: f64) -> usize {
        let mut n = 0;
        for item in self.items.values_mut() {
            if item.status != Status::Done && item.pct < threshold && item.attempts > 0 {
                item.attempts = 0;
                n += 1;
            }
        }
        n
    }
}
