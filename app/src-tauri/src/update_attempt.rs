//! 更新记录：拉起安装程序之前记下从哪一版更新到哪一版，下次启动时据此看出安装程序是不是根本没起来。
//!
//! 更新插件（tauri-plugin-updater 2.12.0 src/updater.rs 的 Windows `install_inner`）在 `ShellExecuteW` 报成功后
//! 立即 `std::process::exit(0)`，之后不再回调。安全软件在引导程序进程起来之后才拦（隔离映像、结束进程）时，启动器
//! 早已退出，用户只看到落樱没了、什么也没装上。启动器也等不了：自己接管拉起就得复刻插件的解包与参数拼接，「退出时
//! 安装」又是在退出路径上拉起的，没有界面可报。
//!
//! 所以拉起之前写下记录，引导程序一启动就删掉它（app/src-tauri-setup/src/launcher.rs 的 `acknowledge_update_attempt`）。
//! 下次启动时记录还在、正在运行的又还是写记录时那一版，就说明安装程序没起来：本次会话的更新卡上说明可能被安全软件
//! 拦了，给出手动下载的出路。版本变了说明后来装上了（经别的途径也算），记录作废。不论哪种结论，读过就删，只报一次。
//!
//! 记录放在 `%LOCALAPPDATA%\Petalfall-Setup`，与换通道的交接记录同一个目录（[`crate::channel_handoff::setup_dir`]）：
//! 便携安装、数据目录落在别处时引导程序也找得到。
//! 哪一步出错都只进日志：这份记录只为事后多说一句，写不下不该拦住更新，读不出也不该拦住启动。

use std::fs;
use std::io;
use std::path::Path;

use petalfall_core::release::order::compare_versions;
use semver::Version;
use serde::{Deserialize, Serialize};

use crate::channel_handoff::setup_dir;

/// 记录的文件名，放在 `%LOCALAPPDATA%\Petalfall-Setup` 下。必须与引导程序 launcher.rs 的 `UPDATE_ATTEMPT_FILE` 相同。
pub const ATTEMPT_FILE: &str = "update-attempt.json";

#[derive(Debug, Serialize, Deserialize)]
struct Attempt {
    /// 拉起安装程序时正在运行的版本。
    from: String,
    /// 要装的版本。
    to: String,
}

/// 拉起安装程序之前记下这次更新：`from` 是正在运行的版本，`to` 是要装的版本。
pub fn record(from: &Version, to: &str) -> io::Result<()> {
    record_in(&setup_dir()?, from, to)
}

fn record_in(dir: &Path, from: &Version, to: &str) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let attempt = Attempt {
        from: from.to_string(),
        to: to.to_owned(),
    };
    let json = serde_json::to_vec(&attempt).expect("两个字符串字段的结构总能序列化成 JSON");
    fs::write(dir.join(ATTEMPT_FILE), json)
}

/// 安装程序没拉起来、错误已经当场报给用户时撤掉记录，下次启动不再报一遍。已经不在了也算撤掉。
pub fn withdraw() -> io::Result<()> {
    withdraw_in(&setup_dir()?)
}

fn withdraw_in(dir: &Path) -> io::Result<()> {
    match fs::remove_file(dir.join(ATTEMPT_FILE)) {
        Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
        _ => Ok(()),
    }
}

/// 启动时读记录得出的结论。各条路都已记过日志，交回只为让调用方与测试分得清走的是哪一条。
#[derive(Debug, PartialEq, Eq)]
enum Found {
    /// 不支持自更新的构建：没去看，记录原样留着。
    NotSupported,
    /// 没有记录：上次没有拉起过安装程序，或引导程序起来时已经删了。
    Absent,
    /// 记录还在、版本没变：要装的 `version` 没装上，安装程序没起来。
    Missed { version: String },
    /// 版本变了：后来装上了。
    Settled,
    /// 读不懂（不是约定形状的 JSON，或版本号不合法）。
    Malformed,
    /// 读不出来（被占用、没有权限），或定位不到所在的目录：留着，下次启动再看。
    Unreadable,
}

/// 本次会话的更新卡要不要报「安装程序没能启动」，报的是哪一版。启动时读一次记录得出，之后不变。
#[derive(Debug, PartialEq, Eq)]
pub struct MissedInstall(Option<String>);

impl MissedInstall {
    /// 在 setup 里调用一次。`own_version` 是本程序的版本；`self_update_supported` 是这个构建支不支持自更新：
    /// 开发构建不该动这台机器上正式安装留下的记录。
    pub fn consume_on_startup(own_version: &Version, self_update_supported: bool) -> Self {
        if !self_update_supported {
            tracing::debug!("不支持自更新的构建，不看更新记录");
            return Self::from(Found::NotSupported);
        }
        let found = match setup_dir() {
            Ok(dir) => consume_in(&dir, own_version),
            Err(err) => {
                tracing::warn!(error = %err, "定位不到安装引导程序的目录，这次不看更新记录");
                Found::Unreadable
            }
        };
        Self::from(found)
    }

    /// 没装上的那一版；不用报时为 `None`。
    pub fn version(&self) -> Option<&str> {
        self.0.as_deref()
    }
}

impl From<Found> for MissedInstall {
    fn from(found: Found) -> Self {
        match found {
            Found::Missed { version } => Self(Some(version)),
            Found::NotSupported
            | Found::Absent
            | Found::Settled
            | Found::Malformed
            | Found::Unreadable => Self(None),
        }
    }
}

fn consume_in(dir: &Path, own_version: &Version) -> Found {
    let path = dir.join(ATTEMPT_FILE);
    let contents = match fs::read(&path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Found::Absent,
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %err, "更新记录读不出来，留到下次启动再看");
            return Found::Unreadable;
        }
    };
    let found = match parse(&contents) {
        Ok((from, to)) if compare_versions(&from, own_version).is_eq() => {
            tracing::warn!(
                path = %path.display(),
                from = %from,
                to = %to,
                "上次拉起安装程序去更新，安装程序却没有起来（多半被安全软件拦下了），更新卡上提示用户"
            );
            Found::Missed { version: to }
        }
        Ok((from, to)) => {
            tracing::info!(from = %from, to = %to, own_version = %own_version, "上次拉起的更新之后版本已经变了，更新记录作废");
            Found::Settled
        }
        Err(reason) => {
            tracing::warn!(path = %path.display(), reason, "更新记录读不懂，删掉");
            Found::Malformed
        }
    };
    // 读过就删，只报一次
    if let Err(err) = withdraw_in(dir) {
        tracing::warn!(path = %path.display(), error = %err, "更新记录删不掉，下次启动还会按它再判一次");
    }
    found
}

fn parse(contents: &[u8]) -> Result<(Version, String), String> {
    let attempt: Attempt = serde_json::from_slice(contents).map_err(|err| err.to_string())?;
    let from =
        Version::parse(&attempt.from).map_err(|err| format!("from 不是合法的版本号：{err}"))?;
    Version::parse(&attempt.to).map_err(|err| format!("to 不是合法的版本号：{err}"))?;
    Ok((from, attempt.to))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    fn attempt_path(dir: &Path) -> std::path::PathBuf {
        dir.join(ATTEMPT_FILE)
    }

    #[test]
    fn a_record_left_behind_on_the_same_version_reports_the_missed_version_once() {
        let dir = tempfile::tempdir().unwrap();
        let running = version("1.0.0-dev.61");
        record_in(dir.path(), &running, "1.0.0-dev.62").unwrap();

        assert_eq!(
            consume_in(dir.path(), &running),
            Found::Missed {
                version: "1.0.0-dev.62".into()
            }
        );
        assert!(!attempt_path(dir.path()).exists());
        assert_eq!(consume_in(dir.path(), &running), Found::Absent);
    }

    #[test]
    fn a_changed_version_means_it_got_installed_and_the_record_goes() {
        let dir = tempfile::tempdir().unwrap();
        record_in(dir.path(), &version("1.0.0-dev.61"), "1.0.0-dev.62").unwrap();
        assert_eq!(
            consume_in(dir.path(), &version("1.0.0-dev.62")),
            Found::Settled
        );
        assert!(!attempt_path(dir.path()).exists());

        // 后来经别的途径装了更新的版本，同样算装上了
        record_in(dir.path(), &version("1.0.0-beta.3"), "1.0.0-beta.4").unwrap();
        assert_eq!(consume_in(dir.path(), &version("1.0.1")), Found::Settled);
    }

    #[test]
    fn a_record_the_setup_already_acknowledged_reports_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let running = version("1.0.0");
        record_in(dir.path(), &running, "1.0.1").unwrap();
        // 引导程序起来时做的事：删掉记录
        fs::remove_file(attempt_path(dir.path())).unwrap();
        assert_eq!(consume_in(dir.path(), &running), Found::Absent);
        assert_eq!(MissedInstall::from(Found::Absent).version(), None);
    }

    #[test]
    fn a_record_that_cannot_be_read_as_agreed_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        for contents in [
            &b""[..],
            b"{\"from\":\"1.0.0\"}",
            b"{\"from\":\"not-a-version\",\"to\":\"1.0.1\"}",
            b"{\"from\":\"1.0.0\",\"to\":\"1.0\"}",
        ] {
            fs::write(attempt_path(dir.path()), contents).unwrap();
            assert_eq!(
                consume_in(dir.path(), &version("1.0.0")),
                Found::Malformed,
                "{}",
                String::from_utf8_lossy(contents)
            );
            assert!(!attempt_path(dir.path()).exists());
        }
    }

    #[test]
    fn the_record_is_plain_json_naming_both_versions() {
        let root = tempfile::tempdir().unwrap();
        // 目录还不存在时一并建出来：没跑过引导程序的机器上没有这个目录
        let dir = root.path().join("Petalfall-Setup");
        record_in(&dir, &version("1.0.0-dev.61"), "1.0.0-dev.62").unwrap();
        let written: serde_json::Value =
            serde_json::from_slice(&fs::read(attempt_path(&dir)).unwrap()).unwrap();
        assert_eq!(
            written,
            serde_json::json!({ "from": "1.0.0-dev.61", "to": "1.0.0-dev.62" })
        );
    }

    #[test]
    fn withdrawing_removes_the_record_and_a_missing_one_is_fine() {
        let dir = tempfile::tempdir().unwrap();
        record_in(dir.path(), &version("1.0.0"), "1.0.1").unwrap();
        withdraw_in(dir.path()).unwrap();
        assert!(!attempt_path(dir.path()).exists());
        withdraw_in(dir.path()).unwrap();
    }

    #[test]
    fn only_a_missed_install_is_reported() {
        assert_eq!(
            MissedInstall::from(Found::Missed {
                version: "1.0.1".into()
            })
            .version(),
            Some("1.0.1")
        );
        for found in [
            Found::NotSupported,
            Found::Absent,
            Found::Settled,
            Found::Malformed,
            Found::Unreadable,
        ] {
            assert_eq!(MissedInstall::from(found).version(), None);
        }
    }

    /// 写的是启动器、删的是引导程序，两边各写一份文件名，由这条测试钉住它们相同。
    #[test]
    fn the_setup_deletes_the_same_file_this_writes() {
        let setup = include_str!("../../src-tauri-setup/src/launcher.rs");
        let decl = format!("pub const UPDATE_ATTEMPT_FILE: &str = \"{ATTEMPT_FILE}\";");
        assert!(setup.contains(&decl), "launcher.rs 缺少 {decl}");
    }
}
