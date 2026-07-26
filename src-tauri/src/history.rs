//! 用量趋势历史采样存储：`history.json`。
//!
//! 设计原则（架构方钉死）：**纯事实、不预测** —— 只记录每次成功刷新时的本地
//! 采样，前端据点画折线，不做任何外推。
//!
//! 目录规则与 storage.rs 完全一致（`KIMICODEBAR_CONFIG_DIR` 覆盖，否则
//! `%APPDATA%\KimiCodeBar`，直接复用 `storage::config_dir` 保证两份实现不漂移）。
//! 写入为原子写（临时文件 + rename）；损坏文件容忍为空历史（历史是派生数据，
//! 丢了重新攒即可）。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::kimi::web::MonthlyInfo;
use crate::quota::{KimiQuota, QuotaDetail};

/// 历史保留窗口：最近 7 天（now - 7*86400 之前的采样在 append 时丢弃）
const RETENTION_SECS: i64 = 7 * 86400;
/// 总条数上限：超出时从最旧端删（约 5 分钟一采可存近 14 天，冗余于 7 天窗口）
const MAX_POINTS: usize = 4000;

/// 单个历史采样点（与 src/types.ts 的 HistoryPoint 一一对应，snake_case；
/// 百分比为**已用**语义，字段缺失为 None 且序列化时跳过该键）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HistoryPoint {
    /// epoch 秒
    pub t: i64,
    /// 7 天窗口已用百分比（缺失为 None）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub weekly: Option<f64>,
    /// 5 小时窗口已用百分比（缺失为 None）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub five_hour: Option<f64>,
    /// 月度总量已用百分比（缺失为 None）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub monthly: Option<f64>,
}

/// 由一次成功刷新的最终配额与月度数据构造采样点。
/// 各窗口取 used / limit * 100（已用%）；窗口缺失、或 limit ≤ 0
/// （已用比例无定义，避免产出 NaN）时对应字段为 None。
pub fn sample_point(quota: &KimiQuota, monthly: Option<&MonthlyInfo>, t: i64) -> HistoryPoint {
    HistoryPoint {
        t,
        weekly: quota.weekly.as_ref().and_then(used_pct),
        five_hour: quota.five_hour.as_ref().and_then(used_pct),
        monthly: monthly.map(|m| m.total_pct),
    }
}

/// 已用百分比：used / limit * 100；limit ≤ 0 时为 None
fn used_pct(d: &QuotaDetail) -> Option<f64> {
    if d.limit > 0.0 {
        Some(d.used / d.limit * 100.0)
    } else {
        None
    }
}

/// 历史采样存储（内存态 + 落盘）。磁盘格式 `{"points":[...]}`（对象包裹，
/// 为将来扩展元数据留位）
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct HistoryStore {
    points: Vec<HistoryPoint>,
}

impl HistoryStore {
    /// 空存储
    pub fn new() -> Self {
        Self::default()
    }

    /// 当前所有采样点（按 t 升序）
    pub fn points(&self) -> &[HistoryPoint] {
        &self.points
    }

    /// 取出所有采样点（按 t 升序）
    pub fn into_points(self) -> Vec<HistoryPoint> {
        self.points
    }

    /// 追加一条采样（"现在"取真实时钟）：同一秒已存在则覆盖旧值；
    /// 随后 prune（保留最近 7 天且 ≤ 4000 条）并按 t 升序归位
    pub fn append(&mut self, point: HistoryPoint) {
        self.append_at(point, now_unix());
    }

    /// 与 append 相同，但"现在"由调用方注入（测试可复现 7 天边界）
    fn append_at(&mut self, point: HistoryPoint, now: i64) {
        // 同一秒重复 append 覆盖旧值
        if let Some(existing) = self.points.iter_mut().find(|p| p.t == point.t) {
            *existing = point;
        } else {
            self.points.push(point);
        }
        self.prune(now);
        // append 可能带乱序时间戳（补采/时钟回拨），统一保持升序
        self.points.sort_by_key(|p| p.t);
    }

    /// 丢弃 now - 7*86400 之前的采样；总数超 4000 时从最旧端删
    fn prune(&mut self, now: i64) {
        let cutoff = now - RETENTION_SECS;
        self.points.retain(|p| p.t >= cutoff);
        let overflow = self.points.len().saturating_sub(MAX_POINTS);
        if overflow > 0 {
            self.points.drain(..overflow);
        }
    }

    /// 读取历史：文件不存在、损坏或其他 IO 错误均按空容忍；
    /// 返回前按 t 升序排序（磁盘数据可能被手工改过）
    pub fn load() -> Self {
        let text = match std::fs::read_to_string(history_file_path()) {
            Ok(text) => text,
            Err(_) => return Self::default(),
        };
        let mut store: Self = serde_json::from_str(&text).unwrap_or_default();
        store.points.sort_by_key(|p| p.t);
        store
    }

    /// 原子写入 history.json（临时文件 + rename；先删目标再 rename，
    /// Windows rename 不允许覆盖已存在文件，与 storage::save_json 同款）
    pub fn save(&self) -> Result<(), String> {
        let path = history_file_path();
        let dir = path.parent().unwrap_or_else(|| std::path::Path::new("."));
        std::fs::create_dir_all(dir).map_err(|e| format!("创建配置目录失败: {e}"))?;

        let json = serde_json::to_string_pretty(self).map_err(|e| format!("序列化失败: {e}"))?;
        let tmp_path = dir.join("history.json.tmp");
        std::fs::write(&tmp_path, json).map_err(|e| format!("写入临时文件失败: {e}"))?;
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| format!("删除旧文件失败: {e}"))?;
        }
        std::fs::rename(&tmp_path, &path).map_err(|e| format!("重命名临时文件失败: {e}"))
    }
}

/// 历史文件路径：{config_dir}/history.json（config_dir 规则与 storage.rs 一致）
fn history_file_path() -> PathBuf {
    crate::storage::config_dir().join("history.json")
}

fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}

#[cfg(test)]
mod tests {
    use super::*;

    // 环境变量是进程级全局状态，凡改动 KIMICODEBAR_CONFIG_DIR 的测试都须持锁串行；
    // 锁为全库共享（lib.rs::TEST_ENV_LOCK），与 storage 等模块的同类测试互斥
    use crate::TEST_ENV_LOCK as ENV_LOCK;

    /// 指向独立临时目录，避免碰真实 %APPDATA%
    fn use_temp_config_dir() -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("kimicodebar-history-test-{}", uuid::Uuid::new_v4()));
        std::env::set_var("KIMICODEBAR_CONFIG_DIR", &dir);
        dir
    }

    fn cleanup(dir: &PathBuf) {
        std::env::remove_var("KIMICODEBAR_CONFIG_DIR");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 只带 t 的采样点（百分比字段全 None）
    fn point(t: i64) -> HistoryPoint {
        HistoryPoint {
            t,
            weekly: None,
            five_hour: None,
            monthly: None,
        }
    }

    fn detail(used: f64, limit: f64) -> QuotaDetail {
        QuotaDetail {
            used,
            limit,
            remaining: (limit - used).max(0.0),
            reset_time: None,
            percent_remaining: if limit > 0.0 {
                (limit - used) / limit * 100.0
            } else {
                0.0
            },
        }
    }

    // ---- append / prune ----

    #[test]
    fn append_same_second_overwrites_old_value() {
        let mut store = HistoryStore::new();
        store.append_at(
            HistoryPoint {
                weekly: Some(10.0),
                ..point(1000)
            },
            2000,
        );
        store.append_at(
            HistoryPoint {
                weekly: Some(42.5),
                five_hour: Some(3.0),
                ..point(1000)
            },
            2000,
        );

        assert_eq!(store.points().len(), 1);
        assert_eq!(store.points()[0].weekly, Some(42.5));
        assert_eq!(store.points()[0].five_hour, Some(3.0));
    }

    #[test]
    fn append_out_of_order_stays_sorted() {
        let mut store = HistoryStore::new();
        store.append_at(point(300), 1000);
        store.append_at(point(100), 1000);
        store.append_at(point(200), 1000);

        let ts: Vec<i64> = store.points().iter().map(|p| p.t).collect();
        assert_eq!(ts, vec![100, 200, 300]);
    }

    #[test]
    fn prune_drops_points_older_than_7_days() {
        let now = 1_900_000_000;
        let cutoff = now - 7 * 86400;
        let mut store = HistoryStore::new();
        store.append_at(point(cutoff - 1), now); // 边界外 1 秒：丢弃
        store.append_at(point(cutoff), now); // 恰在边界：保留
        store.append_at(point(cutoff + 1), now);
        store.append_at(point(now), now);

        let ts: Vec<i64> = store.points().iter().map(|p| p.t).collect();
        assert_eq!(ts, vec![cutoff, cutoff + 1, now]);
    }

    #[test]
    fn prune_caps_at_4000_points_dropping_oldest() {
        let now = 1_900_000_000;
        let mut store = HistoryStore::new();
        // 4100 条递增时间戳（4100 秒远小于 7 天窗口，只触发条数上限）
        for i in 0..4100 {
            store.append_at(point(now - 4100 + i), now);
        }

        assert_eq!(store.points().len(), 4000);
        // 最旧的 100 条被删：最早保留 t = now - 4000，最新仍是 now - 1
        assert_eq!(store.points().first().unwrap().t, now - 4000);
        assert_eq!(store.points().last().unwrap().t, now - 1);
    }

    // ---- load / save ----

    #[test]
    fn load_missing_file_returns_empty() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = use_temp_config_dir();

        assert!(HistoryStore::load().points().is_empty());

        cleanup(&dir);
    }

    #[test]
    fn load_corrupt_file_returns_empty() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = use_temp_config_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("history.json"), "not json").unwrap();

        assert!(HistoryStore::load().points().is_empty());

        cleanup(&dir);
    }

    #[test]
    fn save_load_roundtrip() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = use_temp_config_dir();

        let now = now_unix();
        let mut store = HistoryStore::new();
        store.append(HistoryPoint {
            t: now - 60,
            weekly: Some(12.5),
            five_hour: Some(3.25),
            monthly: None,
        });
        store.append(HistoryPoint {
            t: now,
            weekly: Some(13.0),
            five_hour: None,
            monthly: Some(16.12),
        });
        store.save().unwrap();
        assert!(dir.join("history.json").exists());
        // 临时文件不应残留
        assert!(!dir.join("history.json.tmp").exists());

        let loaded = HistoryStore::load();
        assert_eq!(loaded.points(), store.points());

        // 覆盖写入（rename 目标已存在的路径）
        store.save().unwrap();
        assert_eq!(HistoryStore::load().points(), store.points());

        // 磁盘格式为 snake_case JSON（与 types.ts 契约一致）
        let raw = std::fs::read_to_string(dir.join("history.json")).unwrap();
        assert!(raw.contains("\"points\""));
        assert!(raw.contains("\"five_hour\""));
        assert!(raw.contains("\"weekly\""));

        cleanup(&dir);
    }

    #[test]
    fn load_sorts_unsorted_disk_data() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = use_temp_config_dir();
        std::fs::create_dir_all(&dir).unwrap();
        // 磁盘数据乱序且点缺 monthly 字段（serde default 读回 None）
        std::fs::write(
            dir.join("history.json"),
            r#"{"points":[{"t":300,"weekly":5.0},{"t":100,"five_hour":1.5,"monthly":16.0},{"t":200}]}"#,
        )
        .unwrap();

        let loaded = HistoryStore::load();
        let ts: Vec<i64> = loaded.points().iter().map(|p| p.t).collect();
        assert_eq!(ts, vec![100, 200, 300]);
        assert_eq!(loaded.points()[0].five_hour, Some(1.5));
        assert_eq!(loaded.points()[0].monthly, Some(16.0));
        assert!(loaded.points()[1].weekly.is_none());

        cleanup(&dir);
    }

    #[test]
    fn point_skips_none_fields_in_json() {
        let json = serde_json::to_string(&point(1)).unwrap();
        assert_eq!(json, r#"{"t":1}"#);
    }

    // ---- 采样换算 ----

    #[test]
    fn sample_point_computes_used_percent() {
        let quota = KimiQuota {
            weekly: Some(detail(30.0, 100.0)),
            five_hour: Some(detail(1.0, 4.0)),
            ..Default::default()
        };
        let monthly = MonthlyInfo {
            total_pct: 16.12,
            kimi_pct: 11.12,
            code_pct: 5.0,
            reset_time: None,
        };

        let p = sample_point(&quota, Some(&monthly), 1234);
        assert_eq!(p.t, 1234);
        assert!((p.weekly.unwrap() - 30.0).abs() < 1e-9);
        assert!((p.five_hour.unwrap() - 25.0).abs() < 1e-9);
        assert!((p.monthly.unwrap() - 16.12).abs() < 1e-9);
    }

    #[test]
    fn sample_point_without_monthly() {
        let quota = KimiQuota {
            weekly: Some(detail(50.0, 100.0)),
            ..Default::default()
        };

        let p = sample_point(&quota, None, 1);
        assert!((p.weekly.unwrap() - 50.0).abs() < 1e-9);
        assert!(p.five_hour.is_none());
        assert!(p.monthly.is_none());
    }

    #[test]
    fn sample_point_missing_windows_are_none() {
        // 完全空配额（无任何窗口）：全部 None
        let p = sample_point(&KimiQuota::default(), None, 1);
        assert!(p.weekly.is_none());
        assert!(p.five_hour.is_none());
        assert!(p.monthly.is_none());
    }

    #[test]
    fn sample_point_zero_limit_is_none() {
        // limit 为 0（如未开通额度）时已用比例无定义：None 而不是 NaN
        let quota = KimiQuota {
            weekly: Some(QuotaDetail::default()),
            ..Default::default()
        };

        let p = sample_point(&quota, None, 1);
        assert!(p.weekly.is_none());
    }
}
