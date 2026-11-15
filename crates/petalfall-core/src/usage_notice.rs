//! 用户确认过的使用须知：哪一版、什么时候确认的。
//!
//! 须知的正文与版本号只在前端产物里（构建时写进产物根目录的 usage-notice.md，文首「版本：N」）。后端不知道当前是
//! 第几版，只记下用户点了同意的那一版与时刻；要不要在进入首次启动向导或主界面之前再展示，由前端拿文件里的版本号来比。
//!
//! 记在 config.json 顶层的 `usage_notice` 下，形如 `{"version": 1, "accepted_at": 1763200000000}`，没确认过时不写这个键。
//! 由此定下几条：
//! - 旧配置没有这个键，按没确认过读入：首次启动与从没有这一项的版本升上来，都会先看到须知。
//! - 这一项读不懂（类型对不上、版本号是 0、缺了时刻，或更新的版本换了写法）时只当没确认过、记一条警告，
//!   不连累整份配置被当成坏文件改名重置（见 [`read_lenient`]）：多看一次须知无害，丢了游戏目录登记与首次启动状态却找不回来。
//! - 记录里不认识的键不保留，落盘时丢掉。
//! - 不涉及数据格式号：没有这个键的旧版本把它当不认识的顶层键原样写回；有这个键的版本读到什么都不会拒读。

use std::num::NonZeroU32;

use petalfall_auth::unix_millis;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::facade::Petalfall;

/// 一次确认。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageNoticeAcceptance {
    /// 确认的是第几版：须知文首「版本：N」的 N，从 1 起。
    pub version: NonZeroU32,
    /// 确认的时刻，Unix 毫秒。
    pub accepted_at: u64,
}

impl Petalfall {
    /// 本机确认过的使用须知；没确认过（含旧配置没有这一项、这一项读不懂）为 `None`。
    pub fn usage_notice_acceptance(&self) -> Option<UsageNoticeAcceptance> {
        self.config().usage_notice
    }

    /// 记下用户此刻确认了第 `version` 版，替换之前的记录；落盘仍需调用 [`Petalfall::save_config`]。
    pub fn accept_usage_notice(&mut self, version: NonZeroU32) {
        self.config_mut().usage_notice = Some(UsageNoticeAcceptance {
            version,
            accepted_at: unix_millis(),
        });
    }
}

/// 配置里 `usage_notice` 的读法：读不懂时当没确认过并记警告，不让整份配置因这一项读失败。写成 `null` 与没有这个键同义。
pub(crate) fn read_lenient<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<UsageNoticeAcceptance>, D::Error> {
    let raw = Value::deserialize(deserializer)?;
    if raw.is_null() {
        return Ok(None);
    }
    match UsageNoticeAcceptance::deserialize(&raw) {
        Ok(acceptance) => Ok(Some(acceptance)),
        Err(err) => {
            tracing::warn!(error = %err, value = %raw, "配置里确认过的使用须知读不懂，按没确认过处理，启动时会再展示须知");
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ConfigStore, PetalfallConfig};

    fn version(n: u32) -> NonZeroU32 {
        NonZeroU32::new(n).expect("测试里的版本号从 1 起")
    }

    fn petalfall_in(dir: &std::path::Path) -> Petalfall {
        Petalfall::open(
            PetalfallConfig::default(),
            dir.to_path_buf(),
            dir.join("config.json"),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn old_config_without_the_key_reads_as_not_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, br#"{"first_run_completed": true}"#).unwrap();
        let config = ConfigStore::at(&path).load().await.unwrap();
        assert_eq!(config.usage_notice, None);
        assert!(config.first_run_completed);
        assert!(PetalfallConfig::default().usage_notice.is_none());
    }

    #[tokio::test]
    async fn accepting_records_version_and_time_and_survives_a_reload() {
        let dir = tempfile::tempdir().unwrap();
        let mut petalfall = petalfall_in(dir.path());
        assert_eq!(petalfall.usage_notice_acceptance(), None);
        let picked = 1 + fastrand::u32(..999);

        let before = unix_millis();
        petalfall.accept_usage_notice(version(picked));
        let after = unix_millis();
        petalfall.save_config().await.unwrap();

        let recorded = petalfall
            .usage_notice_acceptance()
            .expect("确认之后应有记录");
        assert_eq!(recorded.version.get(), picked);
        assert!(
            (before..=after).contains(&recorded.accepted_at),
            "确认时刻 {} 不在调用前后 {before}..={after} 之间",
            recorded.accepted_at
        );
        let raw: Value =
            serde_json::from_slice(&std::fs::read(dir.path().join("config.json")).unwrap())
                .unwrap();
        assert_eq!(
            raw["usage_notice"],
            serde_json::json!({ "version": picked, "accepted_at": recorded.accepted_at })
        );
        let reloaded = ConfigStore::at(dir.path().join("config.json"))
            .load()
            .await
            .unwrap();
        assert_eq!(reloaded.usage_notice, Some(recorded));
    }

    /// 须知改版后再确认：记录换成新的一版与新的时刻，不留旧版本。
    #[tokio::test]
    async fn accepting_a_newer_version_replaces_the_record() {
        let dir = tempfile::tempdir().unwrap();
        let mut petalfall = petalfall_in(dir.path());
        petalfall.accept_usage_notice(version(1));
        let first = petalfall.usage_notice_acceptance().unwrap();
        petalfall.accept_usage_notice(version(2));
        let second = petalfall.usage_notice_acceptance().unwrap();
        assert_eq!(second.version.get(), 2);
        assert!(second.accepted_at >= first.accepted_at);
    }

    #[tokio::test]
    async fn not_accepted_writes_no_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        ConfigStore::at(&path)
            .save(&PetalfallConfig::default())
            .await
            .unwrap();
        let raw: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(
            raw.get("usage_notice").is_none(),
            "没确认过时不该写出 usage_notice：{raw}"
        );
    }

    /// 这一项读不懂只当没确认过：别的设置照常读入，不改名留存、不报启动处置，也不落进不认识的顶层设置里原样写回。
    #[tokio::test]
    async fn unreadable_record_counts_as_not_accepted_and_spares_the_rest() {
        for broken in [
            r#""1""#,
            "1",
            "true",
            r#"{"version": 0, "accepted_at": 1763200000000}"#,
            r#"{"version": 1.5, "accepted_at": 1763200000000}"#,
            r#"{"version": -1, "accepted_at": 1763200000000}"#,
            r#"{"version": "1", "accepted_at": 1763200000000}"#,
            r#"{"version": 1}"#,
            r#"{"accepted_at": 1763200000000}"#,
            "null",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("config.json");
            let text = format!(
                r#"{{"first_run_completed": true, "download_concurrency": 16, "usage_notice": {broken}}}"#
            );
            std::fs::write(&path, &text).unwrap();
            let (config, recovery) = ConfigStore::at(&path).load_or_preserve().await.unwrap();
            assert_eq!(config.usage_notice, None, "{broken}");
            assert!(recovery.is_none(), "{broken} 不该算配置损坏：{recovery:?}");
            assert!(config.first_run_completed, "{broken}");
            assert_eq!(config.download_concurrency, 16, "{broken}");
            assert!(
                !config.unrecognized.contains_key("usage_notice"),
                "{broken} 落进了保留区"
            );
            assert_eq!(std::fs::read_to_string(&path).unwrap(), text, "{broken}");
        }
    }

    /// 更新的版本往记录里加了键：认得的两项照常读入。
    #[tokio::test]
    async fn extra_keys_in_the_record_do_not_hide_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(
            &path,
            br#"{"usage_notice": {"version": 3, "accepted_at": 1763200000000, "privacy_version": 2}}"#,
        )
        .unwrap();
        let config = ConfigStore::at(&path).load().await.unwrap();
        assert_eq!(
            config.usage_notice,
            Some(UsageNoticeAcceptance {
                version: version(3),
                accepted_at: 1_763_200_000_000,
            })
        );
    }
}
