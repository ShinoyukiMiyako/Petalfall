//! 按实例名安装：原版与加载器的版本 JSON 合并成一份，落在 `versions/<实例名>/`。
//!
//! 传统布局里加载器版本靠 `inheritsFrom` 指向 `versions/<原版>/`，新建一个 Fabric 实例会在实例列表里
//! 多出一个原版实例，改名、删除也会牵连别的目录。按实例安装时每个实例自成一体：版本 JSON 合并掉
//! 继承链、`id` 改成实例名，客户端 jar 与 natives 也放进实例目录；库与资源仍在共享的 `libraries/`、
//! `assets/` 下。合并后的 JSON 里记下 `clientVersion`，实例改了名也认得出原版版本号。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use petalfall_version::{VersionJson, resolve};

use crate::error::{Error, Result, io_err};
use crate::layout::GameLayout;

/// 把原版 JSON 与可选的加载器 JSON（`inheritsFrom` 指向该原版）合并成实例 `instance_id` 的版本 JSON。
pub fn merge_for_instance(
    instance_id: &str,
    vanilla: &VersionJson,
    loader: Option<&VersionJson>,
) -> Result<VersionJson> {
    let mut merged = match loader {
        None => vanilla.clone(),
        Some(loader) => {
            if loader.inherits_from.as_deref() != Some(vanilla.id.as_str()) {
                return Err(Error::UnsupportedInstallProfile {
                    reason: format!(
                        "加载器版本 {} 继承自 {:?}，不是要安装的原版 {}",
                        loader.id, loader.inherits_from, vanilla.id
                    ),
                });
            }
            let provider = HashMap::from([(vanilla.id.clone(), vanilla.clone())]);
            resolve(loader, &provider)?
        }
    };
    merged.id = instance_id.to_owned();
    merged.inherits_from = None;
    merged.client_version = Some(vanilla.id.clone());
    Ok(merged)
}

/// 把实例的版本 JSON 写到 `versions/<id>/<id>.json`（原子写）。
pub async fn write_instance_json(layout: &GameLayout, version: &VersionJson) -> Result<()> {
    let bytes = instance_json_bytes(version)?;
    petalfall_base::fs::atomic_write(layout.version_json(&version.id), &bytes).await?;
    Ok(())
}

fn instance_json_bytes(version: &VersionJson) -> Result<Vec<u8>> {
    serde_json::to_vec_pretty(version).map_err(|source| Error::Json {
        context: "实例版本 JSON".to_owned(),
        source,
    })
}

/// 加载器安装器的处理器开跑之前，把实例的版本 JSON 换成 `version`（装加载器之前、只合并了原版的那份），交回守卫：
/// 装成、合并结果写好之后调 [`PlacedJson::keep`]，没走完时守卫在 drop 里把它还原成换之前的样子。
///
/// 官方启动器的布局里客户端 jar 旁边一直是纯原版的版本 JSON，有的 Forge 安装器处理器（1.14.3 的 DEOBF_REALMS）
/// 就按客户端 jar 的路径找到它、从里面读原版的库清单。按实例安装时实例的版本 JSON 要到加载器装完才合并落盘：新建时
/// 处理器那会儿它还不在；重装时在的那份也给不了处理器：写坏的解析不出来，传统布局的带 `inheritsFrom`、库清单里只有
/// 加载器自己的库，装好的合并结果里 Forge 自己的库下载地址是空串，处理器把空串当网址解析，直接抛错。所以不论已有的
/// 那份是什么样，一律先换成原版的。
///
/// 读已有的那份时出了「不存在」以外的错（被占用、没有权限、那个位置是个目录）照实报错，什么都不换。
///
/// 从读原字节、写临时文件到改名换上，整段在调用的线程上同步做完，不是 async：中间有让出点的话，安装的 future 可能在
/// 异步写把改名交给阻塞线程池之后被丢弃，守卫在 drop 里先还原，那次改名随后才落地，又把原版那份放了回去。文件只有
/// 几十 KB，与守卫在 drop 里写回原字节一样只阻塞几毫秒。
pub fn place_json_for_processors(layout: &GameLayout, version: &VersionJson) -> Result<PlacedJson> {
    let path = layout.version_json(&version.id);
    let restore = match std::fs::read(&path) {
        Ok(bytes) => Restore::WriteBack(bytes),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Restore::Remove,
        Err(source) => return Err(io_err(&path, source)),
    };
    let bytes = instance_json_bytes(version)?;
    let dir = layout.version_dir(&version.id);
    std::fs::create_dir_all(&dir).map_err(|source| petalfall_base::Error::Io {
        path: dir.clone(),
        source,
    })?;
    // 原子写失败时目标没被换掉，盘上还是原来那份，不用守卫还原。
    write_atomically(&path, &bytes).map_err(|source| petalfall_base::Error::Io {
        path: path.clone(),
        source,
    })?;
    Ok(PlacedJson {
        path,
        restore: Some(restore),
    })
}

/// [`place_json_for_processors`] 换上原版版本 JSON 之后的守卫。没经 [`keep`](Self::keep) 放下就被 drop 时（安装器、
/// 合并或最后写版本 JSON 失败，安装的 future 被丢弃，panic）同步地把实例的版本 JSON 还原成换之前的样子：本来没有的
/// 删掉，本来有的写回原来的字节。还原失败只记日志：drop 里交不回错误，要报的是让安装没走完的那个错。
///
/// 进程被硬杀时什么都来不及还原，盘上留下原版那份。这一处兜不住，靠调用方：换掉版本 JSON 之前要已经记下这次要装的
/// 内容（落樱在新建与重装动手之前写下带安装内容的未完成标记），下一次补全照记下的内容把加载器装回来，不从版本 JSON
/// 里读加载器。原来的字节只留在内存里、不另存成备份文件，也是为了这种时候：硬杀之后不多出一份要另行清理的备份。
#[must_use = "drop 即还原：合并结果写好之后要调 keep"]
#[derive(Debug)]
pub struct PlacedJson {
    path: PathBuf,
    /// `None`：已经放下，不再还原。
    restore: Option<Restore>,
}

#[derive(Debug)]
enum Restore {
    /// 实例本来没有版本 JSON。
    Remove,
    /// 实例本来那份版本 JSON 的原字节。
    WriteBack(Vec<u8>),
}

impl PlacedJson {
    /// 实例的版本 JSON 已经由合并结果盖掉，不再还原。
    pub fn keep(mut self) {
        self.restore = None;
    }
}

impl Drop for PlacedJson {
    fn drop(&mut self) {
        let Some(restore) = self.restore.take() else {
            return;
        };
        let restored = match &restore {
            Restore::Remove => match std::fs::remove_file(&self.path) {
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
                other => other,
            },
            Restore::WriteBack(bytes) => write_atomically(&self.path, bytes),
        };
        if let Err(err) = restored {
            tracing::warn!(path = %self.path.display(), %err, "没装成，还原实例的版本 JSON 失败");
        }
    }
}

/// 同步的原子写，换上原版那份与 drop 里写回原字节共用（drop 里不能 await，换上的那一段不能有让出点，见
/// [`place_json_for_processors`]）：写的途中被打断时盘上还是写之前那份，不会变成半截文件。临时名的取法与
/// `petalfall_base::fs::atomic_write` 相同。
fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or_else(|| std::io::Error::other("版本 JSON 的路径没有文件名"))?;
    let tmp = path.with_file_name(format!(".{name}.{:016x}.tmp", fastrand::u64(..)));
    let written = std::fs::File::create(&tmp).and_then(|mut file| {
        file.write_all(bytes)?;
        file.sync_all()
    });
    let replaced = written.and_then(|()| std::fs::rename(&tmp, path));
    if replaced.is_err() {
        // 要报的是写本身的错；临时文件删不掉也只是多一个点开头的文件。
        let _ = std::fs::remove_file(&tmp);
    }
    replaced
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vanilla() -> VersionJson {
        VersionJson::from_json_str(
            r#"{"id":"1.21.1","type":"release","mainClass":"net.minecraft.client.main.Main",
                "assetIndex":{"id":"17","sha1":"a","size":1,"url":"u"},"assets":"17",
                "downloads":{"client":{"sha1":"c","size":2,"url":"https://piston-data.mojang.com/client.jar"}},
                "javaVersion":{"component":"java-runtime-delta","majorVersion":21},
                "arguments":{"game":["--username","${auth_player_name}"],"jvm":["-Xss1M"]},
                "libraries":[{"name":"org.lwjgl:lwjgl:3.3.3"}]}"#,
        )
        .unwrap()
    }

    fn fabric() -> VersionJson {
        VersionJson::from_json_str(
            r#"{"id":"fabric-loader-0.16.10-1.21.1","inheritsFrom":"1.21.1","type":"release",
                "mainClass":"net.fabricmc.loader.impl.launch.knot.KnotClient",
                "arguments":{"game":[],"jvm":["-DFabricMcEmu= net.minecraft.client.main.Main "]},
                "libraries":[{"name":"net.fabricmc:fabric-loader:0.16.10","url":"https://maven.fabricmc.net/"}]}"#,
        )
        .unwrap()
    }

    #[test]
    fn vanilla_instance_takes_the_instance_name_and_records_client_version() {
        let merged = merge_for_instance("周末生存", &vanilla(), None).unwrap();
        assert_eq!(merged.id, "周末生存");
        assert_eq!(merged.client_version.as_deref(), Some("1.21.1"));
        assert_eq!(merged.inherits_from, None);
        assert_eq!(
            merged.main_class.as_deref(),
            Some("net.minecraft.client.main.Main")
        );
        assert_eq!(
            petalfall_version::identify_mc_version(&merged)
                .value
                .as_deref(),
            Some("1.21.1")
        );
    }

    #[test]
    fn loader_instance_merges_libraries_main_class_and_arguments() {
        let merged = merge_for_instance("模组尝鲜", &vanilla(), Some(&fabric())).unwrap();
        assert_eq!(merged.id, "模组尝鲜");
        assert_eq!(merged.inherits_from, None);
        assert_eq!(merged.client_version.as_deref(), Some("1.21.1"));
        assert_eq!(
            merged.main_class.as_deref(),
            Some("net.fabricmc.loader.impl.launch.knot.KnotClient")
        );
        let libs: Vec<&str> = merged.libraries.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(
            libs,
            vec![
                "net.fabricmc:fabric-loader:0.16.10",
                "org.lwjgl:lwjgl:3.3.3"
            ]
        );
        // 原版的客户端下载、资源索引、Java 要求都留在合并结果里。
        assert!(merged.downloads.as_ref().unwrap().client.is_some());
        assert_eq!(merged.asset_index.as_ref().unwrap().id, "17");
        assert_eq!(merged.java_version.as_ref().unwrap().major_version, 21);
        let jvm = &merged.arguments.as_ref().unwrap().jvm;
        assert_eq!(jvm.len(), 2, "原版在前、加载器在后拼接");
        let loaders = petalfall_version::detect_loaders(&merged);
        assert_eq!(loaders[0].kind, petalfall_version::LoaderKind::Fabric);
    }

    #[test]
    fn loader_for_another_game_version_is_rejected() {
        let mut other = fabric();
        other.inherits_from = Some("1.20.1".into());
        let err = merge_for_instance("x", &vanilla(), Some(&other)).unwrap_err();
        assert!(matches!(err, Error::UnsupportedInstallProfile { .. }));
    }

    #[tokio::test]
    async fn instance_json_is_written_under_its_own_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = GameLayout::new(tmp.path());
        let merged = merge_for_instance("周末生存", &vanilla(), None).unwrap();
        write_instance_json(&layout, &merged).await.unwrap();
        let path = tmp
            .path()
            .join("versions")
            .join("周末生存")
            .join("周末生存.json");
        let back = VersionJson::from_json_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(back, merged);
    }

    /// 实例目录里的文件名，排好序。还原用的临时文件留下了也会列出来。
    fn files_in(dir: &std::path::Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn json_at(path: &std::path::Path) -> VersionJson {
        VersionJson::from_json_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    /// 处理器开跑前不论实例本来有没有版本 JSON、本来那份能不能给处理器用，盘上都换成只合并了原版的那份；没走完
    /// （drop）时还原成换之前的样子，本来没有的删掉、本来有的逐字节写回；装成（keep）后留着原版那份，等合并结果盖掉。
    #[test]
    fn the_json_for_processors_replaces_whatever_is_there_until_kept() {
        let id = "老整合包";
        let base = merge_for_instance(id, &vanilla(), None).unwrap();
        let merged = serde_json::to_vec_pretty(
            &merge_for_instance(id, &vanilla(), Some(&fabric())).unwrap(),
        )
        .unwrap();
        let traditional = serde_json::to_vec_pretty(&fabric()).unwrap();
        let garbage: Vec<u8> = (0..fastrand::usize(1..4096))
            .map(|_| fastrand::u8(..))
            .collect();
        let cases: [(&str, Option<Vec<u8>>); 6] = [
            ("本来没有", None),
            ("空文件", Some(Vec::new())),
            ("装好的合并结果", Some(merged)),
            ("传统布局、带 inheritsFrom", Some(traditional)),
            ("不是 UTF-8", Some(b"\xff\xfe{\"id\":".to_vec())),
            ("写坏的", Some(garbage)),
        ];
        for (case, before) in cases {
            for keep in [false, true] {
                let tmp = tempfile::tempdir().unwrap();
                let layout = GameLayout::new(tmp.path());
                let path = layout.version_json(id);
                if let Some(bytes) = &before {
                    std::fs::create_dir_all(layout.version_dir(id)).unwrap();
                    std::fs::write(&path, bytes).unwrap();
                }

                let placed = place_json_for_processors(&layout, &base).unwrap();
                assert_eq!(json_at(&path), base, "{case}：处理器跑的时候盘上是原版那份");

                let json_file = vec![format!("{id}.json")];
                if keep {
                    placed.keep();
                    assert_eq!(json_at(&path), base, "{case}：keep 之后留着原版那份");
                    assert_eq!(files_in(&layout.version_dir(id)), json_file, "{case}");
                } else {
                    drop(placed);
                    match &before {
                        None => {
                            assert!(!path.exists(), "{case}：本来没有的删掉");
                            assert!(files_in(&layout.version_dir(id)).is_empty(), "{case}");
                        }
                        Some(bytes) => {
                            assert_eq!(&std::fs::read(&path).unwrap(), bytes, "{case}：原字节写回");
                            assert_eq!(files_in(&layout.version_dir(id)), json_file, "{case}");
                        }
                    }
                }
            }
        }
    }

    /// 安装的 future 在处理器跑着时被丢弃（取消后宽限期满）：守卫跟着被 drop，本来的那份照样写回。
    #[tokio::test]
    async fn a_dropped_install_restores_the_instance_json() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = GameLayout::new(tmp.path());
        let id = "模组尝鲜";
        let with_loader = merge_for_instance(id, &vanilla(), Some(&fabric())).unwrap();
        write_instance_json(&layout, &with_loader).await.unwrap();
        let before = std::fs::read(layout.version_json(id)).unwrap();
        let base = merge_for_instance(id, &vanilla(), None).unwrap();

        let (placed_tx, placed_rx) = tokio::sync::oneshot::channel();
        let install = async {
            let _placed = place_json_for_processors(&layout, &base).unwrap();
            placed_tx.send(()).unwrap();
            std::future::pending::<()>().await;
        };
        tokio::select! {
            () = install => unreachable!("安装不会自己结束"),
            placed = placed_rx => placed.unwrap(),
        }

        assert_eq!(std::fs::read(layout.version_json(id)).unwrap(), before);
    }

    /// 换上原版那份的整段没有让出点：安装的 future 只被 poll 一次就已经换好，这时被丢弃，守卫的还原就是盘上的终局，
    /// 不会再有晚到的改名把原版那份放回去。不在 tokio 运行时里跑：中间用了异步文件读写的话第一次 poll 就 panic。
    #[test]
    fn placing_finishes_within_one_poll_so_a_drop_right_after_restores_the_original() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = GameLayout::new(tmp.path());
        let id = "模组尝鲜";
        let before = serde_json::to_vec_pretty(
            &merge_for_instance(id, &vanilla(), Some(&fabric())).unwrap(),
        )
        .unwrap();
        std::fs::create_dir_all(layout.version_dir(id)).unwrap();
        std::fs::write(layout.version_json(id), &before).unwrap();
        let base = merge_for_instance(id, &vanilla(), None).unwrap();

        let placed = std::cell::Cell::new(false);
        let mut install = Box::pin(async {
            let _placed = place_json_for_processors(&layout, &base).unwrap();
            placed.set(true);
            std::future::pending::<()>().await;
        });
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(install.as_mut().poll(&mut cx).is_pending());
        assert!(placed.get(), "第一次 poll 就已经换好");
        assert_eq!(json_at(&layout.version_json(id)), base);

        drop(install);
        assert_eq!(std::fs::read(layout.version_json(id)).unwrap(), before);
        assert_eq!(
            files_in(&layout.version_dir(id)),
            vec![format!("{id}.json")]
        );
    }

    /// 读已有那份时出的不是「不存在」（这里那个位置是个目录）：照实报错，什么都不换。
    #[test]
    fn an_unreadable_instance_json_is_reported_and_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = GameLayout::new(tmp.path());
        let id = "网盘同步的";
        let path = layout.version_json(id);
        std::fs::create_dir_all(&path).unwrap();
        let base = merge_for_instance(id, &vanilla(), None).unwrap();

        let err = place_json_for_processors(&layout, &base).unwrap_err();

        assert!(
            matches!(&err, Error::Io { path: at, .. } if *at == path),
            "{err:?}"
        );
        assert!(path.is_dir());
        assert_eq!(
            files_in(&layout.version_dir(id)),
            vec![format!("{id}.json")]
        );
    }
}
