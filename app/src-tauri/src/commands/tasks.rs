//! 长任务的取消与任务日志的读写。登记表与日志的实现在 [`crate::tasks`]。

use std::path::Path;

use petalfall_core::folders::same_path;
use petalfall_core::Petalfall;
use serde_json::{json, Map, Value};
use tauri::State;

use crate::commands::error::code;
use crate::commands::CommandError;
use crate::facade::Facade;
use crate::tasks::{JournalSnapshot, TaskJournal, TaskRegistry};

/// 按实例名动手的长任务开工前的一道核对：`expected` 是前端入队那一刻的游戏目录（命令参数 `gameDir`），
/// `snapshot` 是这次要用来干活的那份快照。两者对不上报 `game_dir_changed`。
///
/// 任务记录只存实例名，切换游戏目录之后重试的话，同名实例在新目录里是另一个实例，照名字解析就作用到了它身上。
/// 前端重试前也比对，但切换是异步的，前端的检查有窗口期，所以命令自己再核对一次。调用方必须在占锁、登记任务之前、
/// 用同一份快照调用它：再取一份快照来比，两次之间切换了目录就漏过去了。
pub(crate) fn ensure_game_dir(snapshot: &Petalfall, expected: &str) -> Result<(), CommandError> {
    let current = snapshot.game_dir();
    if same_path(Path::new(expected), current) {
        return Ok(());
    }
    Err(CommandError::new(
        code::GAME_DIR_CHANGED,
        format!(
            "当前游戏目录已经换了，不是发起任务时的那个（发起于 {expected}，当前是 {}）",
            current.display()
        ),
    )
    .with_detail(json!({
        "expected": expected,
        "current": current.display().to_string(),
    })))
}

/// 测试用：门面此刻的游戏目录，当作入队那一刻的 `gameDir` 传给按实例名动手的长任务命令。
#[cfg(test)]
pub(crate) async fn current_game_dir(facade: &Facade) -> String {
    facade.read().await.game_dir().display().to_string()
}

/// 测试用：切换目录后重试被拦下的那个错误，detail 原样带回发起时的目录与此刻的当前目录。
#[cfg(test)]
pub(crate) fn assert_game_dir_changed(err: &CommandError, expected: &str, current: &Path) {
    assert_eq!(err.code, code::GAME_DIR_CHANGED, "{err:?}");
    assert_eq!(
        err.detail,
        Some(json!({ "expected": expected, "current": current.display().to_string() })),
        "{err:?}"
    );
}

/// 测试用：`root` 下每一项的相对路径，文件带内容、目录为 `None`；`root` 不在时为空。前后各取一次相等，
/// 就是盘上什么都没动：没建、没删、没改名，也没改内容。
#[cfg(test)]
pub(crate) fn disk_state(root: &Path) -> std::collections::BTreeMap<String, Option<Vec<u8>>> {
    let mut state = std::collections::BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => panic!("读不了 {}：{err}", dir.display()),
        };
        for entry in entries {
            let path = entry.unwrap().path();
            let relative = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if path.is_dir() {
                state.insert(relative, None);
                stack.push(path);
            } else {
                state.insert(relative, Some(std::fs::read(&path).unwrap()));
            }
        }
    }
    state
}

/// 取消一个运行中的长任务。返回该任务此刻是否在运行；不在运行（已结束或尚未登记）时返回
/// `false`，这不是错误——取消与任务自然结束本来就会赛跑。被取消的命令在 1000 ms 内以
/// `{ code: "cancelled" }` 返回。
#[tauri::command]
pub async fn cancel_task(
    task_id: String,
    tasks: State<'_, TaskRegistry>,
) -> Result<bool, CommandError> {
    Ok(tasks.cancel(&task_id))
}

/// 整份写入前端任务表到 `<数据目录>/tasks.json`。每项必须带非空、互不重复的字符串 `id`。
///
/// `seq` 是前端给这份的序号，从 1 起单调递增。不比已落盘那份的序号大时，这份是晚到的旧版本，
/// 丢弃而不报错：更新的那份已经在盘上了。
#[tauri::command]
pub async fn save_task_journal(
    seq: u64,
    tasks: Vec<Map<String, Value>>,
    facade: State<'_, Facade>,
    journal: State<'_, TaskJournal>,
) -> Result<(), CommandError> {
    let data_dir = facade.read().await.data_dir().to_path_buf();
    journal.save(&data_dir, seq, tasks).await?;
    Ok(())
}

/// 删掉 `dest` 旁边上次没写完的导出临时文件。只收导出的命名（`<dest>.part`、`<文件名>.<8 位十六进制>.part.mrpack`），
/// 别的文件不碰；目录已经不在、没有残留都算成功。`dest` 不是绝对路径时报 `invalid_argument`。
#[tauri::command]
pub async fn discard_export_leftovers(dest: String) -> Result<(), CommandError> {
    let path = Path::new(&dest);
    if !path.is_absolute() {
        return Err(CommandError::invalid_argument(format!(
            "导出位置须是绝对路径：{dest}"
        )));
    }
    petalfall_core::discard_export_leftovers(path).await;
    Ok(())
}

/// 读回任务表与它的序号 `{ seq, tasks }`。从未写过时为 `{ seq: 0, tasks: [] }`；
/// 文件损坏或版本不符报 `task_journal_invalid`。
#[tauri::command]
pub async fn load_task_journal(
    facade: State<'_, Facade>,
    journal: State<'_, TaskJournal>,
) -> Result<JournalSnapshot, CommandError> {
    let data_dir = facade.read().await.data_dir().to_path_buf();
    journal.load(&data_dir).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use petalfall_core::PetalfallConfig;

    fn petalfall_at(dir: &Path, game_dir: &Path) -> Petalfall {
        let config = PetalfallConfig {
            game_directory: Some(game_dir.to_path_buf()),
            ..PetalfallConfig::default()
        };
        Petalfall::open(config, dir.to_path_buf(), dir.join("config.json")).unwrap()
    }

    /// 与后端别处的路径等价同一口径：大小写、分隔符混用与末尾分隔符都不算换了目录。
    #[test]
    fn same_directory_written_differently_passes() {
        let dir = tempfile::tempdir().unwrap();
        let game = dir.path().join("Games").join(".minecraft");
        let petalfall = petalfall_at(dir.path(), &game);
        let spelled = format!(
            "{}/",
            game.display().to_string().to_uppercase().replace('\\', "/")
        );
        ensure_game_dir(&petalfall, &spelled).unwrap();
        ensure_game_dir(&petalfall, &game.display().to_string()).unwrap();
    }

    /// 换了目录：报 game_dir_changed，detail 原样带回发起时的目录与此刻的目录，正文两个都写出来。
    #[test]
    fn other_directory_is_refused_with_both_paths() {
        let dir = tempfile::tempdir().unwrap();
        let game = dir.path().join("B").join(".minecraft");
        let petalfall = petalfall_at(dir.path(), &game);
        let expected = dir
            .path()
            .join("A")
            .join(".minecraft")
            .display()
            .to_string();
        let err = ensure_game_dir(&petalfall, &expected).unwrap_err();
        assert_eq!(err.code, code::GAME_DIR_CHANGED);
        assert_eq!(
            err.detail,
            Some(json!({ "expected": expected, "current": game.display().to_string() }))
        );
        assert!(err.message.contains(&expected), "{}", err.message);
        assert!(
            err.message.contains(&game.display().to_string()),
            "{}",
            err.message
        );
    }
}
