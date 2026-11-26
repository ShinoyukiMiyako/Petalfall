//! 把实例导出为 Modrinth `.mrpack`。
//!
//! 预览与导出共用同一次规划，导出总是重新规划、不信任前端带回的任何清单：两者之间磁盘随时可能变。
//! 预览要列出每个额外项的体积，遍历全部；导出只遍历勾选的，存档可能很大，没勾的不该白走一遍盘。
//!
//! 1. 判定写不写得成：实例要装完整、认得出游戏版本，加载器只能是 Fabric、Quilt、Forge、NeoForge 之一
//!    且认得出版本。
//! 2. mods 目录直接子项里启用的 jar 逐个算 sha1。卷宗记为 Modrinth 的先按卷宗里的版本 id 一次取回
//!    版本（`/versions`），在版本的文件里认 sha1；认不出的（文件被换过）、卷宗没有的、卷宗是 CurseForge
//!    的再一起按哈希反查（`/version_files`）。查到、且平台给的 sha512、大小与下载地址都能写进清单的，
//!    只写索引，导入的人从 Modrinth 下载；其余的原样打包进文件。
//! 3. 额外项（配置、资源包、光影、存档……）只从白名单目录收集，跳过启动器元数据目录、`.old` 备份、
//!    存档的 `session.lock`、符号链接与目录联接；日志、崩溃报告、natives、版本 jar 与 JSON 不在白名单里，结构上就进不了包。
//!    配置里模组的默认设置去掉启动器补进去的行再写（见 [`crate::game_lang::FilledLines`]），盘上的文件不动。
//!
//! 写包在阻塞线程里进行，先写到目标旁边的临时文件，回读校验通过后才换到目标位置。
//!
//! 导出按固定的四步报步骤（`CoreEvent::Phase`）：解析实例并算哈希、向 Modrinth 认 Mod、收集要打包的文件并
//! 写包、回读校验，每次都报全。预览不属于任何任务，同一次规划走下来不报步骤。

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use petalfall_instance::{DiscoveredVersion, MarkerStore, PETALFALL_META_DIR};
use petalfall_modpack::{
    ExportPack, IndexFile, OverrideFile, PackLoader, PackLoaderKind, SafeRelativePath,
    WriteProgress, is_export_download_url,
};
use petalfall_modplatform::{DISABLED_SUFFIX, ModrinthFile, ModrinthVersion, Platform};
use petalfall_version::{LoaderKind, identify_mc_version};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::error::{CoreError, Result};
use crate::event::{CoreEvent, DownloadProgress, EventSink, InstallPhase, emit};
use crate::facade::{PROGRESS_EVENT_INTERVAL, Petalfall};
use crate::game_lang::{FilledLines, filled_lines_path, read_filled_lines, without_lines};
use crate::instance_install::PhaseAnnouncer;
use crate::modfs::{BACKUP_SUFFIX, MODS_DIR, bounded_map, local_concurrency, try_exists};
use crate::modpack::read_instance_modpack;

/// 实例没记下整合包版本时的默认版本号。
const DEFAULT_PACK_VERSION: &str = "1.0.0";
/// 一次 `/versions?ids=` 带的版本数。接口文档没写上限，id 都拼在查询串里，分批让地址保持在常规长度。
const VERSIONS_BATCH: usize = 100;
/// 一次 `/version_files` 带的哈希数。接口文档没写上限，分批让请求体与响应都保持在常规大小。
const HASHES_BATCH: usize = 500;
/// 游戏选项文件，额外项里唯一的单个文件。
const OPTIONS_FILE: &str = "options.txt";
/// 存档的占用锁文件。
const SESSION_LOCK: &str = "session.lock";
/// 导出的步骤表。表是固定的，每一步都报：哪一步瞬间过去（没有启用的 Mod 时认 Mod 那一步）也照报，
/// 跳过一步前端写的步号会断档。
const EXPORT_PHASES: [InstallPhase; 4] = [
    InstallPhase::ExportHash,
    InstallPhase::ExportIdentify,
    InstallPhase::ExportPack,
    InstallPhase::ExportVerify,
];

/// 可以一起打包的额外内容。顺序即界面顺序。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ExportExtra {
    #[serde(rename = "config")]
    Config,
    #[serde(rename = "defaultconfigs")]
    DefaultConfigs,
    #[serde(rename = "kubejs")]
    KubeJs,
    #[serde(rename = "resourcepacks")]
    ResourcePacks,
    #[serde(rename = "shaderpacks")]
    ShaderPacks,
    #[serde(rename = "options")]
    Options,
    #[serde(rename = "saves")]
    Saves,
}

impl ExportExtra {
    /// 全部额外项，按界面顺序。
    pub const ALL: [ExportExtra; 7] = [
        ExportExtra::Config,
        ExportExtra::DefaultConfigs,
        ExportExtra::KubeJs,
        ExportExtra::ResourcePacks,
        ExportExtra::ShaderPacks,
        ExportExtra::Options,
        ExportExtra::Saves,
    ];

    /// 在工作目录下的相对路径。
    pub fn relative(self) -> &'static str {
        match self {
            ExportExtra::Config => "config",
            ExportExtra::DefaultConfigs => "defaultconfigs",
            ExportExtra::KubeJs => "kubejs",
            ExportExtra::ResourcePacks => "resourcepacks",
            ExportExtra::ShaderPacks => "shaderpacks",
            ExportExtra::Options => OPTIONS_FILE,
            ExportExtra::Saves => "saves",
        }
    }

    /// 按 serde 名解析；认不出返回 `None`。
    pub fn parse(raw: &str) -> Option<Self> {
        serde_json::from_value(serde_json::Value::String(raw.to_owned())).ok()
    }
}

/// 一次导出的选择。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModpackExportOptions {
    /// 整合包名。
    pub name: String,
    /// 整合包版本号。
    pub version: String,
    /// 简介；Modrinth 格式没有作者字段，这里是唯一的自由文字。
    pub summary: Option<String>,
    /// 一起打包的额外项。
    pub extras: Vec<ExportExtra>,
    /// 停用的 Mod 是否以停用状态带上。
    pub include_disabled: bool,
}

/// 一个 Mod 是怎么在 Modrinth 上认出来的。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportMatch {
    /// 按卷宗记下的版本认出。
    Ledger,
    /// 按文件哈希反查认出。
    Hash,
}

/// 一个 Mod 为什么要把文件打包进去。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BundleReason {
    /// Modrinth 上找不到，或找到了但平台给的文件写不进清单。
    Unlisted,
    /// 卷宗记的来源是 CurseForge，Modrinth 上又找不到。
    #[serde(rename = "curseforge")]
    CurseForge,
    /// 停用的 Mod。
    Disabled,
}

/// 只写进索引的 Mod。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportIndexedMod {
    pub file_name: String,
    /// 实例相对路径（`mods/<文件名>`）。
    pub path: String,
    pub size: u64,
    pub project_id: String,
    pub version_id: String,
    /// 卷宗里记的工程标题；按哈希认出的为 `None`。
    pub title: Option<String>,
    pub matched_by: ExportMatch,
}

/// 文件要打包进去的 Mod。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportBundledMod {
    pub file_name: String,
    /// 实例相对路径。
    pub path: String,
    pub size: u64,
    pub reason: BundleReason,
}

/// 一个额外项此刻在盘上的文件数与字节数（已去掉不会打包的文件）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportExtraStat {
    pub extra: ExportExtra,
    pub files: u64,
    pub bytes: u64,
    /// 界面上是否默认勾选：有文件就勾，存档体积可能很大、只有地图类整合包需要，默认不勾。
    pub default_selected: bool,
    /// 这一项里名字不能安全写进整合包的文件（实例相对的正斜杠路径），勾上才算这次导出会跳过的。
    pub skipped: Vec<String>,
}

/// 导出前的预览。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModpackExportPreview {
    /// 默认名称：实例记下的整合包名，没有则为实例 id。
    pub name: String,
    /// 默认版本号：实例记下的整合包版本，没有则为 1.0.0。
    pub version: String,
    pub minecraft: String,
    pub loader: Option<PackLoader>,
    pub indexed: Vec<ExportIndexedMod>,
    pub bundled: Vec<ExportBundledMod>,
    pub disabled: Vec<ExportBundledMod>,
    pub extras: Vec<ExportExtraStat>,
    /// mods 目录里启用的 jar 中名字不能安全写进整合包的文件（实例相对的正斜杠路径）。不看勾选，导出时总会跳过。
    pub mod_skipped: Vec<String>,
    /// 停用的 jar 中名字不能安全写进整合包的文件，带上停用的 Mod 时才算这次导出会跳过的。
    pub disabled_skipped: Vec<String>,
}

/// 一次导出的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModpackExportOutcome {
    pub path: PathBuf,
    pub name: String,
    pub version: String,
    /// 产物的字节数。
    pub bytes: u64,
    pub indexed_files: usize,
    pub override_files: usize,
    /// 打包进文件的 Mod 文件名。
    pub bundled_mods: Vec<String>,
    /// 实际带上的停用 Mod 数。
    pub disabled_mods: usize,
    pub skipped: Vec<String>,
}

/// 导出的落点与写包用的临时文件。
///
/// 先写到目标旁边的临时文件，校验通过后才换到目标位置：写到一半失败或取消时，用户原来的同名文件
/// 原样不动。临时文件与目标在同一目录，最后一步是同卷改名。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModpackExportTarget {
    destination: PathBuf,
    part: PathBuf,
}

impl ModpackExportTarget {
    /// 不碰磁盘。临时文件为 `<父目录>/<去掉扩展名的文件名>.<8 位小写十六进制>.part.mrpack`：
    /// 扩展名必须是 .mrpack，回读校验用的读包只认 .mrpack 与 .zip。
    pub fn new(destination: PathBuf) -> Self {
        let random = format!("{:08x}.part.mrpack", fastrand::u32(..));
        let part_name = match destination.file_stem() {
            Some(stem) => format!("{}.{random}", stem.to_string_lossy()),
            // 没有文件名的目标过不了命令层的扩展名校验，走不到这里；仍给出确定的落点。
            None => random,
        };
        let part = match destination.parent() {
            Some(parent) => parent.join(part_name),
            None => PathBuf::from(part_name),
        };
        Self { destination, part }
    }

    /// 导出的目标文件。
    pub fn destination(&self) -> &Path {
        &self.destination
    }

    /// 写包用的临时文件。
    pub fn part_path(&self) -> &Path {
        &self.part
    }

    /// 删掉临时文件（不存在不算错），删不掉只记日志；从不碰目标文件。
    pub async fn discard(&self) {
        if let Err(err) = tokio::fs::remove_file(&self.part).await
            && err.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(path = %self.part.display(), error = %err, "导出的临时文件未能删除");
        }
    }
}

/// 删掉 `destination` 旁边上次没写完的导出临时文件，返回删掉的个数。
///
/// 写包时进程没了，收尾代码跑不到，只能下次启动按任务日志里的落点来收：存档导出是 `<落点>.part`，整合包
/// 导出是 `<文件名>.<8 位十六进制>.part.mrpack`。别的文件一概不碰；目录已经不在等于没有残留，删不掉只记日志。
pub async fn discard_export_leftovers(destination: &Path) -> usize {
    let (Some(parent), Some(name), Some(stem)) = (
        destination.parent(),
        destination.file_name(),
        destination.file_stem(),
    ) else {
        return 0;
    };
    let (name, stem) = (name.to_string_lossy(), stem.to_string_lossy());
    let mut entries = match tokio::fs::read_dir(parent).await {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return 0,
        Err(err) => {
            tracing::warn!(path = %parent.display(), error = %err, "读不了导出目录，没写完的临时文件留着");
            return 0;
        }
    };
    let mut removed = 0;
    loop {
        let entry = match entries.next_entry().await {
            Ok(Some(entry)) => entry,
            Ok(None) => break,
            Err(err) => {
                tracing::warn!(path = %parent.display(), error = %err, "遍历导出目录中断");
                break;
            }
        };
        if !is_export_leftover(&entry.file_name().to_string_lossy(), &name, &stem) {
            continue;
        }
        match tokio::fs::remove_file(entry.path()).await {
            Ok(()) => removed += 1,
            Err(err) => {
                tracing::warn!(path = %entry.path().display(), error = %err, "导出的临时文件未能删除");
            }
        }
    }
    removed
}

fn is_export_leftover(file: &str, destination_name: &str, destination_stem: &str) -> bool {
    if file.strip_suffix(".part") == Some(destination_name) {
        return true;
    }
    file.strip_prefix(destination_stem)
        .and_then(|rest| rest.strip_prefix('.'))
        .and_then(|rest| rest.strip_suffix(".part.mrpack"))
        .is_some_and(|random| random.len() == 8 && random.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// 实例写不成整合包的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportBlock {
    VersionUnknown,
    LoaderUnsupported,
    LoaderVersionUnknown,
    MultipleLoaders,
    Incomplete,
}

impl ExportBlock {
    /// 面向用户的原因。`loader` 是不受支持的加载器名。
    pub(crate) fn describe(self, loader: Option<&str>) -> String {
        match (self, loader) {
            (ExportBlock::VersionUnknown, _) => {
                "认不出这个实例的游戏版本，没法写进整合包".to_owned()
            }
            (ExportBlock::LoaderUnsupported, Some(loader)) => {
                format!("整合包格式不支持 {loader}，只支持 Fabric、Quilt、Forge、NeoForge")
            }
            (ExportBlock::LoaderUnsupported, None) => {
                "整合包格式只支持 Fabric、Quilt、Forge、NeoForge".to_owned()
            }
            (ExportBlock::LoaderVersionUnknown, _) => "认不出加载器版本，没法写进整合包".to_owned(),
            (ExportBlock::MultipleLoaders, _) => {
                "这个实例装了不止一种加载器，整合包只能写一种".to_owned()
            }
            (ExportBlock::Incomplete, _) => "实例还没装完整，修复后再导出".to_owned(),
        }
    }
}

/// 盘上的一个文件与它在包里的落点。
#[derive(Debug, Clone)]
struct PlannedFile {
    path: SafeRelativePath,
    source: PathBuf,
    /// 预览报的字节数；`content` 有值时是它的长度。
    size: u64,
    /// 包里写的内容与盘上的不同时为规划时定下的内容（见 [`leave_out_filled_lines`]），写包时不再读盘。
    content: Option<Vec<u8>>,
}

impl PlannedFile {
    fn into_override(self) -> OverrideFile {
        OverrideFile {
            path: self.path,
            source: self.source,
            content: self.content,
        }
    }
}

/// 一个额外项收集到的文件。
#[derive(Debug, Default)]
struct CollectedExtra {
    files: Vec<PlannedFile>,
    skipped: Vec<String>,
}

/// 预览与导出共用的规划结果。
struct ExportPlan {
    name: String,
    version: String,
    minecraft: String,
    loader: Option<PackLoader>,
    indexed: Vec<(ExportIndexedMod, IndexFile)>,
    bundled: Vec<(ExportBundledMod, PlannedFile)>,
    disabled: Vec<(ExportBundledMod, PlannedFile)>,
    /// 这次规划要的额外项；没要的不在里面，也没有遍历过。
    extras: BTreeMap<ExportExtra, CollectedExtra>,
    /// mods 目录里启用的 jar 中名字写不进整合包的文件。
    mod_skipped: Vec<String>,
    /// 停用的 jar 中名字写不进整合包的文件。
    disabled_skipped: Vec<String>,
}

impl ExportPlan {
    fn preview(&self) -> ModpackExportPreview {
        let extras = self
            .extras
            .iter()
            .map(|(&extra, collected)| {
                let files = collected.files.len() as u64;
                ExportExtraStat {
                    extra,
                    files,
                    bytes: collected.files.iter().map(|f| f.size).sum(),
                    default_selected: extra != ExportExtra::Saves && files > 0,
                    skipped: collected.skipped.clone(),
                }
            })
            .collect();
        ModpackExportPreview {
            name: self.name.clone(),
            version: self.version.clone(),
            minecraft: self.minecraft.clone(),
            loader: self.loader.clone(),
            indexed: self.indexed.iter().map(|(m, _)| m.clone()).collect(),
            bundled: self.bundled.iter().map(|(m, _)| m.clone()).collect(),
            disabled: self.disabled.iter().map(|(m, _)| m.clone()).collect(),
            extras,
            mod_skipped: self.mod_skipped.clone(),
            disabled_skipped: self.disabled_skipped.clone(),
        }
    }
}

/// mods 目录里的一个 jar。
#[derive(Debug, Clone)]
struct LocalJar {
    file_name: String,
    file: PlannedFile,
}

/// 算过哈希的启用 jar。
#[derive(Debug, Clone)]
struct HashedJar {
    jar: LocalJar,
    sha1: String,
}

impl Petalfall {
    /// 导出前的预览：能不能导出、默认名称与版本、哪些 Mod 走索引、哪些要打包、各额外项的大小。
    /// 只读，要联网问 Modrinth。
    pub async fn preview_modpack_export(&self, id: &str) -> Result<ModpackExportPreview> {
        Ok(self
            .plan_export(id, &ExportExtra::ALL, None, None, None, true)
            .await?
            .preview())
    }

    /// 把实例导出为 `.mrpack`，写到 `target` 的目标位置（已有同名文件时替换）。
    ///
    /// 先按此刻的盘面重新规划，再在阻塞线程里写到临时文件并回读校验，通过后改名到目标位置。取消在
    /// 算哈希时按文件、联网时立即、写包时按 64 KiB 观察。失败时临时文件已删，目标文件不动。
    ///
    /// 步骤按 `EXPORT_PHASES` 四步报全；开工前就已取消的不报任何一步。
    pub async fn export_modpack(
        &self,
        id: &str,
        options: &ModpackExportOptions,
        target: &ModpackExportTarget,
        events: Option<&EventSink>,
    ) -> Result<ModpackExportOutcome> {
        self.ensure_not_cancelled()?;
        // 校验那一步是在写包的阻塞线程里进入的，播报器要能带过去。
        let phases = Arc::new(PhaseAnnouncer::new(events, EXPORT_PHASES.to_vec()));
        phases.enter(InstallPhase::ExportHash);
        let plan = self
            .plan_export(
                id,
                &options.extras,
                Some(target),
                events,
                Some(&*phases),
                false,
            )
            .await?;

        let mut skipped = plan.mod_skipped.clone();
        let mut overrides: Vec<PlannedFile> = Vec::new();
        overrides.extend(plan.bundled.iter().map(|(_, f)| f.clone()));
        let disabled_mods = if options.include_disabled {
            overrides.extend(plan.disabled.iter().map(|(_, f)| f.clone()));
            skipped.extend(plan.disabled_skipped.iter().cloned());
            plan.disabled.len()
        } else {
            0
        };
        for collected in plan.extras.values() {
            overrides.extend(collected.files.iter().cloned());
            skipped.extend(collected.skipped.iter().cloned());
        }
        overrides.sort_by(|a, b| a.path.cmp(&b.path));
        skipped.sort();
        let bundled_mods: Vec<String> = plan
            .bundled
            .iter()
            .map(|(m, _)| m.file_name.clone())
            .collect();

        let mut files: Vec<IndexFile> = plan.indexed.iter().map(|(_, f)| f.clone()).collect();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let override_count = overrides.len();
        let bytes_total: u64 = overrides.iter().map(|f| f.size).sum();
        let pack = ExportPack {
            name: options.name.clone(),
            version_id: options.version.clone(),
            summary: options.summary.clone(),
            minecraft: plan.minecraft.clone(),
            loader: plan.loader.clone(),
            files,
            overrides: overrides
                .into_iter()
                .map(PlannedFile::into_override)
                .collect(),
        };

        if !bundled_mods.is_empty() {
            emit(
                events,
                CoreEvent::warning(format!(
                    "{} 个 Mod 不在 Modrinth 上，已直接打包进文件",
                    bundled_mods.len()
                )),
            );
        }
        if !skipped.is_empty() {
            emit(
                events,
                CoreEvent::warning(format!(
                    "{} 个文件名不能安全写进整合包，已跳过",
                    skipped.len()
                )),
            );
        }
        emit(
            events,
            CoreEvent::stage(format!("打包 {override_count} 个文件")),
        );
        let written = self
            .write_pack_reporting(pack, target.part_path(), bytes_total, events, phases)
            .await?;

        if let Err(source) = tokio::fs::rename(target.part_path(), target.destination()).await {
            target.discard().await;
            return Err(petalfall_base::Error::Io {
                path: target.destination().to_path_buf(),
                source,
            }
            .into());
        }
        emit(
            events,
            CoreEvent::stage(format!("已导出到 {}", target.destination().display())),
        );
        Ok(ModpackExportOutcome {
            path: target.destination().to_path_buf(),
            name: options.name.clone(),
            version: options.version.clone(),
            bytes: written.bytes,
            indexed_files: written.index_files,
            override_files: written.override_files,
            bundled_mods,
            disabled_mods,
            skipped,
        })
    }

    /// 在阻塞线程里写包，把写包进度节流成下载进度事件（结束时补发终值）；写完最后一个文件时进入校验这一步。
    async fn write_pack_reporting(
        &self,
        pack: ExportPack,
        part: &Path,
        bytes_total: u64,
        events: Option<&EventSink>,
        phases: Arc<PhaseAnnouncer>,
    ) -> Result<petalfall_modpack::WrittenPack> {
        let (tx, rx) = watch::channel(DownloadProgress {
            total: pack.overrides.len() as u64,
            finished: 0,
            bytes: 0,
            total_bytes: Some(bytes_total),
            transferred: 0,
            speed: 0,
        });
        let forwarder = events
            .cloned()
            .map(|sink| tokio::spawn(forward_progress(rx, sink)));
        let cancel = self.cancel_token().cloned();
        let verify_sink = events.cloned();
        let part = part.to_path_buf();
        let written = tokio::task::spawn_blocking(move || {
            let started = Instant::now();
            let mut last_file = LastFileWritten::default();
            let is_cancelled = || cancel.as_ref().is_some_and(|t| t.is_cancelled());
            let mut on_progress = |p: WriteProgress| {
                let secs = started.elapsed().as_secs_f64();
                let speed = if secs > 0.0 {
                    (p.bytes_done as f64 / secs) as u64
                } else {
                    0
                };
                tx.send_replace(DownloadProgress {
                    total: p.files_total,
                    finished: p.files_done,
                    bytes: p.bytes_done,
                    total_bytes: Some(p.bytes_total),
                    transferred: 0,
                    speed,
                });
                // 最后一个文件写完之后紧接着收尾与回读校验。
                if last_file.reached(p) {
                    phases.enter(InstallPhase::ExportVerify);
                    emit(verify_sink.as_ref(), CoreEvent::stage("校验导出的整合包"));
                }
            };
            petalfall_modpack::write_pack(&pack, &part, &is_cancelled, &mut on_progress)
        })
        .await?;
        if let Some(forwarder) = forwarder {
            forwarder.await?;
        }
        Ok(written?)
    }

    fn ensure_not_cancelled(&self) -> Result<()> {
        if self.is_cancelled() {
            return Err(petalfall_modpack::Error::Cancelled.into());
        }
        Ok(())
    }

    /// 网络请求不经下载池，自己响应取消：取消后立即放弃等待，不等平台慢慢回。
    async fn cancellable<T>(&self, work: impl Future<Output = T>) -> Result<T> {
        match self.cancel_token() {
            Some(token) => token
                .run_until_cancelled(work)
                .await
                .ok_or_else(|| petalfall_modpack::Error::Cancelled.into()),
            None => Ok(work.await),
        }
    }

    /// `extras` 是要遍历的额外项，别的额外项不碰盘。`phases` 是导出任务的步骤播报器，第一步（算哈希）由导出在
    /// 开工时报，这里报认 Mod 与凑包两步；预览不属于任何任务，传 `None`。`previewing` 为真时是给预览规划，
    /// 记账读不出来不让它失败（见下）。
    async fn plan_export(
        &self,
        id: &str,
        extras: &[ExportExtra],
        target: Option<&ModpackExportTarget>,
        events: Option<&EventSink>,
        phases: Option<&PhaseAnnouncer>,
        previewing: bool,
    ) -> Result<ExportPlan> {
        let version = self.resolve_instance(id).await?;
        let (minecraft, loader) = exportable(&version).await?;
        let (name, pack_version) = match read_instance_modpack(&version.dir).await? {
            Some(info) => (
                info.name,
                info.version
                    .unwrap_or_else(|| DEFAULT_PACK_VERSION.to_owned()),
            ),
            None => (id.to_owned(), DEFAULT_PACK_VERSION.to_owned()),
        };
        let dirs = self.instance_dirs(&version).await?;
        let listing = list_mods(&dirs.mods_dir).await?;

        emit(
            events,
            CoreEvent::stage(format!("读取 {} 个 Mod 文件", listing.enabled.len())),
        );
        let cancel = self.cancel_token().cloned();
        let hashed = bounded_map(
            listing.enabled,
            local_concurrency(),
            move |jar: LocalJar| {
                let cancel = cancel.clone();
                async move {
                    if cancel.as_ref().is_some_and(|t| t.is_cancelled()) {
                        return Err(petalfall_modpack::Error::Cancelled.into());
                    }
                    let sha1 = petalfall_base::fs::sha1_hex(&jar.file.source).await?;
                    Ok(HashedJar { jar, sha1 })
                }
            },
        )
        .await?;

        if let Some(phases) = phases {
            phases.enter(InstallPhase::ExportIdentify);
        }
        let (indexed, bundled) = self.identify_on_modrinth(id, hashed, events).await?;

        // 认完 Mod 起就是在凑要打包的文件（停用的 jar、额外项）并写包，额外项里的存档可能要走很久。
        if let Some(phases) = phases {
            phases.enter(InstallPhase::ExportPack);
        }
        let disabled = listing
            .disabled
            .into_iter()
            .map(|jar| {
                (
                    ExportBundledMod {
                        file_name: jar.file_name.clone(),
                        path: jar.file.path.as_str().to_owned(),
                        size: jar.file.size,
                        reason: BundleReason::Disabled,
                    },
                    jar.file,
                )
            })
            .collect();

        let excluded: HashSet<String> = target
            .map(|t| {
                [t.destination(), t.part_path()]
                    .into_iter()
                    .map(path_key)
                    .collect()
            })
            .unwrap_or_default();
        let working_dir = dirs.working_dir.clone();
        let wanted: BTreeSet<ExportExtra> = extras.iter().copied().collect();
        // 记账与启动前补全看的是同一个工作目录。模组的默认设置都在配置里，没要配置就不读。记账读不出来时只有
        // 带配置的导出照实报错，不能悄悄把补的行带出去；预览只报统计，按没有记账算并记一条警告：导出弹窗要等
        // 预览成功才出表单，预览一失败，连不带配置的导出也发不出去。
        let filled = if !wanted.contains(&ExportExtra::Config) {
            FilledLines::default()
        } else if previewing {
            match read_filled_lines(&working_dir).await {
                Ok(filled) => filled,
                Err(err) => {
                    tracing::warn!(
                        path = %filled_lines_path(&working_dir).display(),
                        error = %err,
                        cause = ?std::error::Error::source(&err),
                        "补行记账读不出来，预览按没有记账统计配置"
                    );
                    FilledLines::default()
                }
            }
        } else {
            read_filled_lines(&working_dir).await?
        };
        let extras = tokio::task::spawn_blocking(move || {
            wanted
                .into_iter()
                .map(|extra| {
                    Ok((
                        extra,
                        collect_extra(&working_dir, extra, &excluded, &filled)?,
                    ))
                })
                .collect::<std::result::Result<BTreeMap<_, _>, petalfall_base::Error>>()
        })
        .await??;

        Ok(ExportPlan {
            name,
            version: pack_version,
            minecraft,
            loader,
            indexed,
            bundled,
            disabled,
            extras,
            mod_skipped: listing.skipped,
            disabled_skipped: listing.disabled_skipped,
        })
    }

    /// 在 Modrinth 上认启用的 jar，分成只写索引的与要打包的两组，各按路径排好。
    #[allow(clippy::type_complexity)]
    async fn identify_on_modrinth(
        &self,
        id: &str,
        hashed: Vec<HashedJar>,
        events: Option<&EventSink>,
    ) -> Result<(
        Vec<(ExportIndexedMod, IndexFile)>,
        Vec<(ExportBundledMod, PlannedFile)>,
    )> {
        let ledger = self.ledger_store(id).load().await?;
        let mut indexed = Vec::new();
        let mut bundled = Vec::new();
        if !hashed.is_empty() {
            emit(
                events,
                CoreEvent::stage(format!("向 Modrinth 查询 {} 个 Mod", hashed.len())),
            );
        }
        let modrinth = self.modrinth_client();

        let mut by_ledger: Vec<(HashedJar, String, Option<String>)> = Vec::new();
        let mut by_hash: Vec<(HashedJar, BundleReason)> = Vec::new();
        for jar in hashed {
            match ledger.find(&jar.jar.file_name) {
                Some(entry) if entry.platform == Platform::Modrinth => {
                    by_ledger.push((jar, entry.version_id.clone(), entry.title.clone()))
                }
                Some(_) => by_hash.push((jar, BundleReason::CurseForge)),
                None => by_hash.push((jar, BundleReason::Unlisted)),
            }
        }

        let mut wanted: Vec<String> = by_ledger.iter().map(|(_, v, _)| v.clone()).collect();
        wanted.sort();
        wanted.dedup();
        let mut versions: HashMap<String, ModrinthVersion> = HashMap::new();
        for batch in wanted.chunks(VERSIONS_BATCH) {
            for version in self.cancellable(modrinth.versions_by_ids(batch)).await?? {
                versions.insert(version.id.clone(), version);
            }
        }
        for (jar, version_id, title) in by_ledger {
            let Some((version, file)) = versions
                .get(&version_id)
                .and_then(|v| find_file(v, &jar.sha1).map(|f| (v, f)))
            else {
                // 文件被换过（更新或手动替换），卷宗记的版本里没有它：交给哈希反查。
                by_hash.push((jar, BundleReason::Unlisted));
                continue;
            };
            match index_entry(&jar, file) {
                Some(entry) => indexed.push((
                    indexed_mod(&jar, version, title, ExportMatch::Ledger),
                    entry,
                )),
                None => bundled.push(bundled_mod(jar, BundleReason::Unlisted)),
            }
        }

        let mut hashes: Vec<String> = by_hash.iter().map(|(j, _)| j.sha1.clone()).collect();
        hashes.sort();
        hashes.dedup();
        let mut found: HashMap<String, ModrinthVersion> = HashMap::new();
        for batch in hashes.chunks(HASHES_BATCH) {
            let found_batch = self
                .cancellable(modrinth.versions_by_hashes(batch))
                .await??;
            found.extend(
                found_batch
                    .into_iter()
                    .map(|(hash, version)| (hash.to_ascii_lowercase(), version)),
            );
        }
        for (jar, reason) in by_hash {
            let hit = found
                .get(&jar.sha1)
                .and_then(|v| find_file(v, &jar.sha1).map(|f| (v, f)))
                .and_then(|(v, f)| index_entry(&jar, f).map(|entry| (v, entry)));
            match hit {
                Some((version, entry)) => {
                    indexed.push((indexed_mod(&jar, version, None, ExportMatch::Hash), entry))
                }
                None => bundled.push(bundled_mod(jar, reason)),
            }
        }

        indexed.sort_by(|a, b| a.1.path.cmp(&b.1.path));
        bundled.sort_by(|a, b| a.1.path.cmp(&b.1.path));
        Ok((indexed, bundled))
    }
}

/// 判定实例能不能写成整合包，给出 MC 版本与加载器。
async fn exportable(version: &DiscoveredVersion) -> Result<(String, Option<PackLoader>)> {
    let block = |reason: ExportBlock, loader: Option<String>| CoreError::ModpackExportUnsupported {
        id: version.id.clone(),
        reason,
        loader,
    };
    if try_exists(MarkerStore::for_version_dir(&version.dir).path()).await? {
        return Err(block(ExportBlock::Incomplete, None));
    }
    let Some(minecraft) = identify_mc_version(&version.json).value else {
        return Err(block(ExportBlock::VersionUnknown, None));
    };
    if let Some(unsupported) = version
        .loaders
        .iter()
        .find(|l| matches!(l.kind, LoaderKind::OptiFine | LoaderKind::LiteLoader))
    {
        return Err(block(
            ExportBlock::LoaderUnsupported,
            Some(unsupported.kind.as_str().to_owned()),
        ));
    }
    // 1.20.1 的 NeoForge 沿用 Forge 的坐标与启动参数，探测同时命中两个。探测照实列着（这时 Forge 的 Mod 也能装），
    // 写进整合包的只有 NeoForge。
    let neoforge = version
        .loaders
        .iter()
        .any(|l| l.kind == LoaderKind::NeoForge);
    let mut writable = version.loaders.iter().filter_map(|l| {
        let kind = match l.kind {
            LoaderKind::Fabric => PackLoaderKind::Fabric,
            LoaderKind::Quilt => PackLoaderKind::Quilt,
            LoaderKind::Forge if neoforge => return None,
            LoaderKind::Forge => PackLoaderKind::Forge,
            LoaderKind::NeoForge => PackLoaderKind::NeoForge,
            LoaderKind::OptiFine | LoaderKind::LiteLoader => return None,
        };
        Some((kind, l.version.clone()))
    });
    let loader = match (writable.next(), writable.next()) {
        (None, _) => None,
        (Some(_), Some(_)) => return Err(block(ExportBlock::MultipleLoaders, None)),
        (Some((_, None)), None) => return Err(block(ExportBlock::LoaderVersionUnknown, None)),
        (Some((kind, Some(version))), None) => Some(PackLoader { kind, version }),
    };
    Ok((minecraft, loader))
}

/// mods 目录直接子项的分类结果。
#[derive(Debug, Default)]
struct ModListing {
    enabled: Vec<LocalJar>,
    disabled: Vec<LocalJar>,
    /// 启用的 jar 里名字写不进整合包的。
    skipped: Vec<String>,
    /// 停用的 jar 里名字写不进整合包的：没带上停用的 Mod 时它们本来就不进包，不算跳过。
    disabled_skipped: Vec<String>,
}

/// 只看 mods 目录直接子项里的普通文件：`*.jar` 启用、`*.jar.disabled` 停用，其余（备份、子目录、
/// 说明文件）加载器不读，也不导出。与 Mod 列表同一条判定。
async fn list_mods(mods_dir: &Path) -> Result<ModListing> {
    let io = |path: &Path, source: std::io::Error| -> CoreError {
        petalfall_base::Error::Io {
            path: path.to_path_buf(),
            source,
        }
        .into()
    };
    let mut listing = ModListing::default();
    let mut entries = match tokio::fs::read_dir(mods_dir).await {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(listing),
        Err(source) => return Err(io(mods_dir, source)),
    };
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|source| io(mods_dir, source))?
    {
        let path = entry.path();
        let file_type = entry
            .file_type()
            .await
            .map_err(|source| io(&path, source))?;
        if !file_type.is_file() {
            continue;
        }
        let raw = entry.file_name();
        let lossy = raw.to_string_lossy();
        let enabled = lossy.ends_with(".jar");
        let disabled = lossy
            .strip_suffix(DISABLED_SUFFIX)
            .is_some_and(|base| base.ends_with(".jar"));
        if !enabled && !disabled {
            continue;
        }
        let relative = format!("{MODS_DIR}/{lossy}");
        let (Some(file_name), Ok(safe)) = (raw.to_str(), SafeRelativePath::new(relative.clone()))
        else {
            let skipped = if enabled {
                &mut listing.skipped
            } else {
                &mut listing.disabled_skipped
            };
            skipped.push(relative);
            continue;
        };
        let size = entry
            .metadata()
            .await
            .map_err(|source| io(&path, source))?
            .len();
        let jar = LocalJar {
            file_name: file_name.to_owned(),
            file: PlannedFile {
                path: safe,
                source: path,
                size,
                content: None,
            },
        };
        if enabled {
            listing.enabled.push(jar);
        } else {
            listing.disabled.push(jar);
        }
    }
    listing
        .enabled
        .sort_by(|a, b| a.file.path.cmp(&b.file.path));
    listing
        .disabled
        .sort_by(|a, b| a.file.path.cmp(&b.file.path));
    listing.skipped.sort();
    listing.disabled_skipped.sort();
    Ok(listing)
}

/// 版本里 sha1 与本地文件相同的那个文件。
fn find_file<'a>(version: &'a ModrinthVersion, sha1: &str) -> Option<&'a ModrinthFile> {
    version.files.iter().find(|f| {
        f.hashes
            .sha1
            .as_deref()
            .is_some_and(|h| h.eq_ignore_ascii_case(sha1))
    })
}

/// 平台给的文件能不能写进清单：要有 128 位 sha512、大小与本地一致、下载地址在导出白名单里。
fn index_entry(jar: &HashedJar, file: &ModrinthFile) -> Option<IndexFile> {
    let sha512 = file.hashes.sha512.as_deref()?;
    let well_formed = sha512.len() == 128 && sha512.bytes().all(|b| b.is_ascii_hexdigit());
    if !well_formed || file.size != jar.jar.file.size || !is_export_download_url(&file.url) {
        return None;
    }
    Some(IndexFile {
        path: jar.jar.file.path.clone(),
        sha1: jar.sha1.clone(),
        sha512: sha512.to_ascii_lowercase(),
        size: jar.jar.file.size,
        downloads: vec![file.url.clone()],
    })
}

fn indexed_mod(
    jar: &HashedJar,
    version: &ModrinthVersion,
    title: Option<String>,
    matched_by: ExportMatch,
) -> ExportIndexedMod {
    ExportIndexedMod {
        file_name: jar.jar.file_name.clone(),
        path: jar.jar.file.path.as_str().to_owned(),
        size: jar.jar.file.size,
        project_id: version.project_id.clone(),
        version_id: version.id.clone(),
        title,
        matched_by,
    }
}

fn bundled_mod(jar: HashedJar, reason: BundleReason) -> (ExportBundledMod, PlannedFile) {
    (
        ExportBundledMod {
            file_name: jar.jar.file_name,
            path: jar.jar.file.path.as_str().to_owned(),
            size: jar.jar.file.size,
            reason,
        },
        jar.jar.file,
    )
}

/// 路径比较键：按组件、不分大小写（Windows 文件系统）。
fn path_key(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
        .collect::<Vec<_>>()
        .join("/")
}

/// 收集一个额外项的文件。`filled` 是工作目录里的记账，导出配置时用。阻塞 IO。
fn collect_extra(
    working_dir: &Path,
    extra: ExportExtra,
    excluded: &HashSet<String>,
    filled: &FilledLines,
) -> std::result::Result<CollectedExtra, petalfall_base::Error> {
    let mut collected = CollectedExtra::default();
    let root = working_dir.join(extra.relative());
    if extra == ExportExtra::Options {
        let meta = match std::fs::symlink_metadata(&root) {
            Ok(meta) => meta,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(collected),
            Err(source) => return Err(petalfall_base::Error::Io { path: root, source }),
        };
        if meta.file_type().is_file() && !excluded.contains(&path_key(&root)) {
            collected.files.push(PlannedFile {
                path: SafeRelativePath::new(OPTIONS_FILE)
                    .expect("options.txt 是合法的实例相对路径"),
                source: root,
                size: meta.len(),
                content: None,
            });
        }
        return Ok(collected);
    }
    match std::fs::symlink_metadata(&root) {
        // 额外项目录本身是链接（指到别的实例或别处）时不跟随，与目录里的链接同一处理。
        Ok(meta) if meta.file_type().is_dir() => {}
        Ok(_) => return Ok(collected),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(collected),
        Err(source) => return Err(petalfall_base::Error::Io { path: root, source }),
    }
    walk(&root, extra.relative(), excluded, &mut collected)?;
    for file in &mut collected.files {
        leave_out_filled_lines(file, filled)?;
    }
    collected.files.sort_by(|a, b| a.path.cmp(&b.path));
    collected.skipped.sort();
    Ok(collected)
}

/// 启动器补进模组默认设置的行不随包带走（见 [`FilledLines`]）：包里写去掉它们之后的内容，盘上的文件不动。大小
/// 跟着换成去掉之后的，预览报的才与写进包里的一致。记账里没有这份文件的不读它。阻塞 IO。
fn leave_out_filled_lines(
    file: &mut PlannedFile,
    filled: &FilledLines,
) -> std::result::Result<(), petalfall_base::Error> {
    let Some(lines) = filled.lines_for(file.path.as_str()) else {
        return Ok(());
    };
    let bytes = std::fs::read(&file.source).map_err(|source| petalfall_base::Error::Io {
        path: file.source.clone(),
        source,
    })?;
    if let Some(kept) = without_lines(&bytes, lines) {
        file.size = kept.len() as u64;
        file.content = Some(kept);
    }
    Ok(())
}

/// 递归收集目录里的普通文件。符号链接与目录联接（Windows 上 `is_symlink` 对两者都为真）一律不跟随：
/// 跟进去可能走出实例目录，把别的实例乃至整个盘打进包里。
fn walk(
    dir: &Path,
    relative: &str,
    excluded: &HashSet<String>,
    out: &mut CollectedExtra,
) -> std::result::Result<(), petalfall_base::Error> {
    let io = |path: &Path, source: std::io::Error| petalfall_base::Error::Io {
        path: path.to_path_buf(),
        source,
    };
    let entries = std::fs::read_dir(dir).map_err(|source| io(dir, source))?;
    for entry in entries {
        let entry = entry.map_err(|source| io(dir, source))?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|source| io(&path, source))?;
        if file_type.is_symlink() {
            continue;
        }
        let raw = entry.file_name();
        let name = raw.to_string_lossy();
        if name.eq_ignore_ascii_case(PETALFALL_META_DIR) {
            continue;
        }
        let child = format!("{relative}/{name}");
        if file_type.is_dir() {
            walk(&path, &child, excluded, out)?;
            continue;
        }
        // session.lock 是游戏打开存档时的锁文件：游戏开着时对它持有排他锁，读它会失败；导入的人也用不上。
        if !file_type.is_file()
            || name.to_lowercase().ends_with(BACKUP_SUFFIX)
            || name.eq_ignore_ascii_case(SESSION_LOCK)
            || excluded.contains(&path_key(&path))
        {
            continue;
        }
        let safe = match (raw.to_str(), SafeRelativePath::new(child.clone())) {
            (Some(_), Ok(safe)) => safe,
            _ => {
                out.skipped.push(child);
                continue;
            }
        };
        let size = entry.metadata().map_err(|source| io(&path, source))?.len();
        out.files.push(PlannedFile {
            path: safe,
            source: path,
            size,
            content: None,
        });
    }
    Ok(())
}

/// 把写包进度节流成下载进度事件；发送端全部释放后补发最后一份再结束。
async fn forward_progress(mut rx: watch::Receiver<DownloadProgress>, sink: EventSink) {
    while rx.changed().await.is_ok() {
        let snapshot = *rx.borrow_and_update();
        if sink.send(CoreEvent::Download(snapshot)).is_err() {
            break;
        }
        tokio::time::sleep(PROGRESS_EVENT_INTERVAL).await;
    }
}

/// 写包进度里最后一个文件写完的那一刻，写包回调据此转入校验一步。写包在每块之后、每个文件写完时与收尾时都回调，
/// 文件数追平总数之后收尾还要再回调一次，只认第一次追平；没有要打包的文件时收尾那一下就追平，校验这一步照样报到。
#[derive(Debug, Default)]
struct LastFileWritten {
    seen: bool,
}

impl LastFileWritten {
    fn reached(&mut self, progress: WriteProgress) -> bool {
        if self.seen || progress.files_done != progress.files_total {
            return false;
        }
        self.seen = true;
        true
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;
    use crate::config::PetalfallConfig;
    use crate::ledger::{Ledger, LedgerEntry, UpdateHold};
    use crate::modpack::{InstanceModpack, ModpackSource};
    use petalfall_download::CancellationToken;
    use petalfall_instance::{IsolationOverride, VersionSettings, VersionSettingsStore};
    use petalfall_modpack::PackFormat;
    use sha1::{Digest, Sha1};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const ID: &str = "樱花林";
    const FABRIC_JSON: &str = r#"{"id":"樱花林","inheritsFrom":"test","type":"release",
        "mainClass":"net.fabricmc.loader.impl.launch.knot.KnotClient",
        "libraries":[{"name":"net.fabricmc:fabric-loader:0.15.0"}]}"#;

    fn sha1_of(bytes: &[u8]) -> String {
        Sha1::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// 平台给的 sha512 导出时只转写不复算，造一个与文件一一对应的 128 位值即可（大写，验证写出时转小写）。
    fn fake_sha512(sha1: &str) -> String {
        format!("{sha1}{sha1}{sha1}{}", &sha1[..8]).to_ascii_uppercase()
    }

    fn cdn(project: &str, version: &str, file: &str) -> String {
        format!("https://cdn.modrinth.com/data/{project}/versions/{version}/{file}")
    }

    fn version_json(
        id: &str,
        project: &str,
        file: &str,
        bytes: &[u8],
        url: &str,
    ) -> serde_json::Value {
        let sha1 = sha1_of(bytes);
        serde_json::json!({
            "id": id, "project_id": project, "name": id, "version_number": "1.0",
            "version_type": "release", "date_published": "2026-01-01T00:00:00Z",
            "files": [{
                "hashes": {"sha1": sha1.to_ascii_uppercase(), "sha512": fake_sha512(&sha1)},
                "url": url, "filename": file, "primary": true, "size": bytes.len()
            }]
        })
    }

    struct Fixture {
        _tmp: tempfile::TempDir,
        root: PathBuf,
        petalfall: Petalfall,
        server: MockServer,
        dir: PathBuf,
        jars: BTreeMap<&'static str, Vec<u8>>,
    }

    impl Fixture {
        fn write(&self, relative: &str, bytes: &[u8]) {
            let path = self.dir.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
    }

    /// 一个隔离的 Fabric 实例：
    /// - a.jar、b.jar 卷宗记为 Modrinth，c.jar 没有卷宗但按哈希查得到，d.jar 查不到，
    ///   e.jar 卷宗记为 CurseForge 且查不到，x.jar.disabled 停用；
    /// - mods 里还有 .old 备份、说明文件与子目录里的 jar（都不该导出）；
    /// - config 带一个 .old、一个嵌套的元数据目录、一个 Windows 保留名文件；
    /// - logs、crash-reports、启动器元数据目录与版本 JSON 永远不进包。
    async fn fixture() -> Fixture {
        let server = MockServer::start().await;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let mc = root.join("mc");
        let petalfall =
            Petalfall::for_test(PetalfallConfig::default(), root.join("data"), mc.clone())
                .with_modrinth_base(server.uri())
                .with_fabric_base(server.uri())
                .with_manifest_url(format!("{}/manifest.json", server.uri()));
        let dir = mc.join("versions").join(ID);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{ID}.json")), FABRIC_JSON).unwrap();
        VersionSettingsStore::for_version_dir(&dir)
            .save(&VersionSettings {
                isolation: IsolationOverride::Enabled,
                ..VersionSettings::default()
            })
            .await
            .unwrap();

        let seed = fastrand::u64(..);
        let mut rng = fastrand::Rng::with_seed(seed);
        let mut jars = BTreeMap::new();
        for name in [
            "a.jar",
            "b.jar",
            "c.jar",
            "d.jar",
            "e.jar",
            "x.jar.disabled",
        ] {
            let len = rng.usize(0..=96 * 1024);
            let bytes: Vec<u8> = format!("{name}:{seed}:")
                .into_bytes()
                .into_iter()
                .chain((0..len).map(|_| rng.u8(..)))
                .collect();
            jars.insert(name, bytes);
        }
        let fixture = Fixture {
            _tmp: tmp,
            root,
            petalfall,
            server,
            dir,
            jars,
        };
        for (name, bytes) in &fixture.jars {
            fixture.write(&format!("mods/{name}"), bytes);
        }
        fixture.write("mods/a.jar.old", b"old a");
        fixture.write("mods/readme.txt", b"notes");
        fixture.write("mods/sub/inner.jar", b"nested");
        fixture.write("config/sodium-options.json", b"{\"quality\":1}");
        fixture.write("config/sub/deep.toml", b"deep = true");
        fixture.write("config/stale.toml.OLD", b"backup");
        fixture.write(&format!("config/{PETALFALL_META_DIR}/leak.json"), b"leak");
        fixture.write("config/nul.json", b"reserved");
        fixture.write("options.txt", b"lang:zh_cn");
        fixture.write("resourcepacks/pack.zip", b"PK-pack");
        fixture.write("saves/World/level.dat", b"level");
        fixture.write("logs/latest.log", b"log");
        fixture.write("crash-reports/crash.txt", b"crash");

        let entry =
            |file: &str, platform, project: &str, version: &str, title: Option<&str>| LedgerEntry {
                file_name: file.to_owned(),
                platform,
                project_id: project.to_owned(),
                version_id: version.to_owned(),
                version_number: None,
                title: title.map(str::to_owned),
                icon_url: None,
                sha1: None,
                installed_at: 1_754_000_000,
                installed_as_dependency_of: None,
                update_hold: None,
            };
        fixture
            .petalfall
            .ledger_store(ID)
            .save(&Ledger {
                entries: vec![
                    entry("a.jar", Platform::Modrinth, "pa", "va", Some("Sodium")),
                    entry("b.jar", Platform::Modrinth, "pb", "vb", None),
                    entry(
                        "e.jar",
                        Platform::CurseForge,
                        "238222",
                        "4712866",
                        Some("JEI"),
                    ),
                ],
            })
            .await
            .unwrap();
        fixture
    }

    /// 卷宗两个版本走 /versions，c.jar 按哈希查得到，`c_url` 是平台给它的下载地址。
    async fn mount_modrinth(fixture: &Fixture, c_url: &str) {
        let jars = &fixture.jars;
        Mock::given(method("GET"))
            .and(path("/versions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                version_json(
                    "va",
                    "pa",
                    "a.jar",
                    &jars["a.jar"],
                    &cdn("pa", "va", "a.jar")
                ),
                version_json(
                    "vb",
                    "pb",
                    "b.jar",
                    &jars["b.jar"],
                    &cdn("pb", "vb", "b.jar")
                ),
            ])))
            .mount(&fixture.server)
            .await;
        let c_sha1 = sha1_of(&jars["c.jar"]);
        Mock::given(method("POST"))
            .and(path("/version_files"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                c_sha1: version_json("vc", "pc", "c.jar", &jars["c.jar"], c_url)
            })))
            .mount(&fixture.server)
            .await;
    }

    async fn requests_to(server: &MockServer, route: &str) -> Vec<wiremock::Request> {
        server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .filter(|r| r.url.path() == route)
            .collect()
    }

    fn names<T>(items: &[T], name: impl Fn(&T) -> &str) -> Vec<String> {
        items.iter().map(|i| name(i).to_owned()).collect()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn preview_splits_mods_by_ledger_and_hash_and_counts_extras() {
        let fixture = fixture().await;
        mount_modrinth(&fixture, &cdn("pc", "vc", "c.jar")).await;
        let preview = fixture.petalfall.preview_modpack_export(ID).await.unwrap();

        assert_eq!(
            (preview.name.as_str(), preview.version.as_str()),
            (ID, "1.0.0")
        );
        assert_eq!(preview.minecraft, "test");
        assert_eq!(
            preview.loader,
            Some(PackLoader {
                kind: PackLoaderKind::Fabric,
                version: "0.15.0".into()
            })
        );
        let size = |name: &str| fixture.jars[name].len() as u64;
        let indexed: Vec<_> = preview
            .indexed
            .iter()
            .map(|m| {
                (
                    m.path.as_str(),
                    m.project_id.as_str(),
                    m.version_id.as_str(),
                    m.title.as_deref(),
                    m.matched_by,
                    m.size,
                )
            })
            .collect();
        assert_eq!(
            indexed,
            vec![
                (
                    "mods/a.jar",
                    "pa",
                    "va",
                    Some("Sodium"),
                    ExportMatch::Ledger,
                    size("a.jar")
                ),
                (
                    "mods/b.jar",
                    "pb",
                    "vb",
                    None,
                    ExportMatch::Ledger,
                    size("b.jar")
                ),
                (
                    "mods/c.jar",
                    "pc",
                    "vc",
                    None,
                    ExportMatch::Hash,
                    size("c.jar")
                ),
            ]
        );
        let bundled: Vec<_> = preview
            .bundled
            .iter()
            .map(|m| (m.file_name.as_str(), m.reason, m.size))
            .collect();
        assert_eq!(
            bundled,
            vec![
                ("d.jar", BundleReason::Unlisted, size("d.jar")),
                ("e.jar", BundleReason::CurseForge, size("e.jar")),
            ]
        );
        assert_eq!(
            names(&preview.disabled, |m| &m.path),
            vec!["mods/x.jar.disabled"]
        );
        assert_eq!(preview.disabled[0].reason, BundleReason::Disabled);

        let stats: Vec<_> = preview
            .extras
            .iter()
            .map(|s| (s.extra, s.files, s.bytes, s.default_selected))
            .collect();
        let config_bytes = (b"{\"quality\":1}".len() + b"deep = true".len()) as u64;
        assert_eq!(
            stats,
            vec![
                (ExportExtra::Config, 2, config_bytes, true),
                (ExportExtra::DefaultConfigs, 0, 0, false),
                (ExportExtra::KubeJs, 0, 0, false),
                (ExportExtra::ResourcePacks, 1, 7, true),
                (ExportExtra::ShaderPacks, 0, 0, false),
                (ExportExtra::Options, 1, 10, true),
                (ExportExtra::Saves, 1, 5, false),
            ]
        );
        assert!(preview.mod_skipped.is_empty());
        let skipped: Vec<_> = preview
            .extras
            .iter()
            .filter(|s| !s.skipped.is_empty())
            .map(|s| (s.extra, s.skipped.clone()))
            .collect();
        assert_eq!(
            skipped,
            vec![(ExportExtra::Config, vec!["config/nul.json".to_owned()])]
        );

        let ledger_requests = requests_to(&fixture.server, "/versions").await;
        assert_eq!(ledger_requests.len(), 1);
        let ids = ledger_requests[0]
            .url
            .query_pairs()
            .find(|(k, _)| k == "ids")
            .map(|(_, v)| v.into_owned())
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Vec<String>>(&ids).unwrap(),
            vec!["va", "vb"]
        );
        let lookups = requests_to(&fixture.server, "/version_files").await;
        assert_eq!(lookups.len(), 1);
        let body: serde_json::Value = serde_json::from_slice(&lookups[0].body).unwrap();
        assert_eq!(body["algorithm"], "sha1");
        let mut asked: Vec<String> = body["hashes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h.as_str().unwrap().to_owned())
            .collect();
        asked.sort();
        let mut expected: Vec<String> = ["c.jar", "d.jar", "e.jar"]
            .iter()
            .map(|n| sha1_of(&fixture.jars[n]))
            .collect();
        expected.sort();
        assert_eq!(asked, expected);
        for ledger_hit in ["a.jar", "b.jar"] {
            assert!(
                !asked.contains(&sha1_of(&fixture.jars[ledger_hit])),
                "卷宗认出的 {ledger_hit} 不再按哈希查"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn files_the_platform_serves_from_other_hosts_are_bundled() {
        let fixture = fixture().await;
        mount_modrinth(&fixture, "https://example.com/c.jar").await;
        let preview = fixture.petalfall.preview_modpack_export(ID).await.unwrap();
        assert_eq!(
            names(&preview.indexed, |m| &m.file_name),
            vec!["a.jar", "b.jar"]
        );
        let bundled: Vec<_> = preview
            .bundled
            .iter()
            .map(|m| (m.file_name.as_str(), m.reason))
            .collect();
        assert_eq!(
            bundled,
            vec![
                ("c.jar", BundleReason::Unlisted),
                ("d.jar", BundleReason::Unlisted),
                ("e.jar", BundleReason::CurseForge),
            ]
        );
    }

    /// 卷宗里记的版本已经不含这个文件（被手动换过）时，改按哈希查。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ledger_versions_without_the_local_file_fall_back_to_the_hash_lookup() {
        let fixture = fixture().await;
        fixture.write("mods/a.jar", b"replaced by hand");
        mount_modrinth(&fixture, &cdn("pc", "vc", "c.jar")).await;
        let preview = fixture.petalfall.preview_modpack_export(ID).await.unwrap();
        assert_eq!(
            names(&preview.indexed, |m| &m.file_name),
            vec!["b.jar", "c.jar"]
        );
        assert_eq!(
            names(&preview.bundled, |m| &m.file_name),
            vec!["a.jar", "d.jar", "e.jar"]
        );
        let lookups = requests_to(&fixture.server, "/version_files").await;
        let body: serde_json::Value = serde_json::from_slice(&lookups[0].body).unwrap();
        let hashes = body["hashes"].as_array().unwrap();
        assert_eq!(hashes.len(), 4);
        assert!(hashes.contains(&serde_json::json!(sha1_of(b"replaced by hand"))));
    }

    /// 忽略更新与锁定版本只管检查更新：整合包照样按卷宗记的版本写进索引，接收方装到的是同一个文件。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn update_holds_do_not_change_how_mods_are_indexed() {
        let fixture = fixture().await;
        fixture
            .petalfall
            .set_mod_update_hold(ID, "a.jar", Some(UpdateHold::Ignored))
            .await
            .unwrap();
        fixture
            .petalfall
            .set_mod_update_hold(
                ID,
                "b.jar",
                Some(UpdateHold::Pinned {
                    version_id: "vb".into(),
                }),
            )
            .await
            .unwrap();
        mount_modrinth(&fixture, &cdn("pc", "vc", "c.jar")).await;
        let preview = fixture.petalfall.preview_modpack_export(ID).await.unwrap();

        let indexed: Vec<_> = preview
            .indexed
            .iter()
            .map(|m| (m.file_name.as_str(), m.version_id.as_str(), m.matched_by))
            .collect();
        assert_eq!(
            indexed,
            vec![
                ("a.jar", "va", ExportMatch::Ledger),
                ("b.jar", "vb", ExportMatch::Ledger),
                ("c.jar", "vc", ExportMatch::Hash),
            ]
        );
        assert_eq!(
            names(&preview.bundled, |m| &m.file_name),
            vec!["d.jar", "e.jar"]
        );
        let by_ids = requests_to(&fixture.server, "/versions").await;
        assert_eq!(by_ids.len(), 1);
        let ids = by_ids[0].url.query().unwrap().to_owned();
        assert!(ids.contains("va") && ids.contains("vb"), "{ids}");
        let ledger = fixture.petalfall.ledger_store(ID).load().await.unwrap();
        let held: Vec<_> = ledger
            .entries
            .iter()
            .filter_map(|e| e.update_hold.as_ref().map(|h| (e.file_name.as_str(), h)))
            .collect();
        assert_eq!(
            held,
            vec![
                ("a.jar", &UpdateHold::Ignored),
                (
                    "b.jar",
                    &UpdateHold::Pinned {
                        version_id: "vb".into()
                    }
                ),
            ]
        );
    }

    fn bare_petalfall(root: &Path) -> Petalfall {
        Petalfall::for_test(
            PetalfallConfig::default(),
            root.join("data"),
            root.join("mc"),
        )
        .with_modrinth_base("http://127.0.0.1:9")
    }

    fn bare_instance(root: &Path, id: &str, json: &str) -> PathBuf {
        let dir = root.join("mc").join("versions").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{id}.json")), json).unwrap();
        dir
    }

    /// 1.20.1 的 NeoForge 沿用 Forge 的坐标与启动参数，探测同时命中两个：装的是 NeoForge，不算双加载器。
    #[tokio::test]
    async fn neoforge_on_1_20_1_exports_as_neoforge() {
        let tmp = tempfile::tempdir().unwrap();
        let petalfall = bare_petalfall(tmp.path());
        bare_instance(
            tmp.path(),
            "新锻造",
            r#"{"id":"新锻造","inheritsFrom":"1.20.1","type":"release",
                "mainClass":"cpw.mods.bootstraplauncher.BootstrapLauncher",
                "arguments":{"game":["--fml.forgeVersion","47.1.106","--fml.forgeGroup","net.neoforged"],"jvm":[]},
                "libraries":[{"name":"net.neoforged:forge:1.20.1-47.1.106:universal"}]}"#,
        );

        let preview = petalfall.preview_modpack_export("新锻造").await.unwrap();

        assert_eq!(
            preview.loader,
            Some(PackLoader {
                kind: PackLoaderKind::NeoForge,
                version: "47.1.106".to_owned(),
            })
        );
    }

    #[tokio::test]
    async fn instances_that_cannot_become_a_pack_say_why() {
        let tmp = tempfile::tempdir().unwrap();
        let petalfall = bare_petalfall(tmp.path());
        let cases = [
            (
                "带光影",
                r#"{"id":"带光影","inheritsFrom":"1.20.1","type":"release","mainClass":"M",
                    "libraries":[{"name":"optifine:OptiFine:1.20.1_HD_U_I6"}]}"#,
                ExportBlock::LoaderUnsupported,
                Some("OptiFine"),
            ),
            (
                "双加载器",
                r#"{"id":"双加载器","inheritsFrom":"1.20.1","type":"release","mainClass":"M",
                    "libraries":[{"name":"net.fabricmc:fabric-loader:0.15.0"},
                                 {"name":"org.quiltmc:quilt-loader:0.26.0"}]}"#,
                ExportBlock::MultipleLoaders,
                None,
            ),
            (
                "没版本号",
                r#"{"id":"没版本号","inheritsFrom":"1.20.1","type":"release",
                    "mainClass":"net.fabricmc.loader.impl.launch.knot.KnotClient","libraries":[]}"#,
                ExportBlock::LoaderVersionUnknown,
                None,
            ),
            (
                "认不出",
                r#"{"id":"认不出","type":"release","mainClass":"M","libraries":[]}"#,
                ExportBlock::VersionUnknown,
                None,
            ),
        ];
        for (id, json, reason, loader) in cases {
            bare_instance(tmp.path(), id, json);
            let err = petalfall.preview_modpack_export(id).await.unwrap_err();
            let CoreError::ModpackExportUnsupported {
                id: got_id,
                reason: got,
                loader: got_loader,
            } = &err
            else {
                panic!("{id}: {err:?}");
            };
            assert_eq!(
                (got_id.as_str(), *got, got_loader.as_deref()),
                (id, reason, loader)
            );
            assert_eq!(err.to_string(), reason.describe(loader), "{id}");
        }
        assert_eq!(
            CoreError::ModpackExportUnsupported {
                id: "x".into(),
                reason: ExportBlock::LoaderUnsupported,
                loader: Some("LiteLoader".into()),
            }
            .to_string(),
            "整合包格式不支持 LiteLoader，只支持 Fabric、Quilt、Forge、NeoForge"
        );

        // 未完成标记压过一切：版本 JSON 看起来正常也一样。
        let dir = bare_instance(
            tmp.path(),
            "装了一半",
            r#"{"id":"装了一半","inheritsFrom":"1.21.1","type":"release","mainClass":"M"}"#,
        );
        MarkerStore::for_version_dir(&dir)
            .save(&petalfall_instance::IncompleteMarker::pending(
                petalfall_instance::IncompleteKind::Repair,
                1,
            ))
            .await
            .unwrap();
        let err = petalfall
            .preview_modpack_export("装了一半")
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                CoreError::ModpackExportUnsupported {
                    reason: ExportBlock::Incomplete,
                    ..
                }
            ),
            "{err:?}"
        );
        assert_eq!(err.to_string(), "实例还没装完整，修复后再导出");

        let missing = petalfall
            .preview_modpack_export("没有这个")
            .await
            .unwrap_err();
        assert!(
            matches!(missing, CoreError::VersionNotInstalled { .. }),
            "{missing:?}"
        );
    }

    /// 从整合包导入的实例默认用它记下的名字与版本；没记版本时为 1.0.0。原版实例没有加载器，
    /// 也没有 Mod 要查，不发任何请求。
    #[tokio::test]
    async fn defaults_come_from_the_recorded_modpack() {
        let tmp = tempfile::tempdir().unwrap();
        let petalfall = bare_petalfall(tmp.path());
        let dir = bare_instance(
            tmp.path(),
            "导入的",
            r#"{"id":"导入的","inheritsFrom":"1.21.1","type":"release","mainClass":"M"}"#,
        );
        std::fs::create_dir_all(dir.join(PETALFALL_META_DIR)).unwrap();
        for (version, expected) in [(Some("3.4.5"), "3.4.5"), (None, "1.0.0")] {
            let info = InstanceModpack {
                schema: 1,
                format: PackFormat::Modrinth,
                platform: None,
                project_id: None,
                version_id: None,
                name: "Fabulously Optimized".into(),
                version: version.map(str::to_owned),
                minecraft: "1.21.1".into(),
                loader: None,
                remote_files: 0,
                override_files: 0,
                installed_at: 1,
                recommended_ram_mb: None,
            };
            std::fs::write(
                dir.join(PETALFALL_META_DIR).join("modpack.json"),
                serde_json::to_vec(&info).unwrap(),
            )
            .unwrap();
            let preview = petalfall.preview_modpack_export("导入的").await.unwrap();
            assert_eq!(preview.name, "Fabulously Optimized");
            assert_eq!(preview.version, expected);
            assert_eq!(preview.minecraft, "1.21.1");
            assert_eq!(preview.loader, None);
            assert!(preview.indexed.is_empty() && preview.bundled.is_empty());
            assert!(
                preview
                    .extras
                    .iter()
                    .all(|s| s.files == 0 && !s.default_selected)
            );
        }
    }

    #[test]
    fn extras_parse_by_their_wire_names_in_interface_order() {
        let wire: Vec<String> = ExportExtra::ALL
            .iter()
            .map(|e| {
                serde_json::to_value(e)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        assert_eq!(
            wire,
            vec![
                "config",
                "defaultconfigs",
                "kubejs",
                "resourcepacks",
                "shaderpacks",
                "options",
                "saves"
            ]
        );
        for (extra, name) in ExportExtra::ALL.iter().zip(&wire) {
            assert_eq!(ExportExtra::parse(name), Some(*extra));
        }
        for bad in ["Config", "option", "", "mods", "options.txt"] {
            assert_eq!(ExportExtra::parse(bad), None, "{bad}");
        }
        assert_eq!(ExportExtra::Options.relative(), "options.txt");
        assert_eq!(ExportExtra::KubeJs.relative(), "kubejs");
    }

    #[test]
    fn target_part_sits_next_to_the_destination_with_an_mrpack_extension() {
        let destination = PathBuf::from("D:/导出/樱花林 1.0.0.mrpack");
        let target = ModpackExportTarget::new(destination.clone());
        assert_eq!(target.destination(), destination);
        let part = target.part_path();
        assert_eq!(part.parent(), destination.parent());
        let name = part.file_name().unwrap().to_str().unwrap();
        let random = name
            .strip_prefix("樱花林 1.0.0.")
            .and_then(|rest| rest.strip_suffix(".part.mrpack"))
            .unwrap_or_else(|| panic!("{name}"));
        assert_eq!(random.len(), 8);
        assert!(
            random
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        );
    }

    #[tokio::test]
    async fn discarding_removes_only_the_part_file() {
        let tmp = tempfile::tempdir().unwrap();
        let target = ModpackExportTarget::new(tmp.path().join("包.mrpack"));
        target.discard().await;
        std::fs::write(target.destination(), b"keep").unwrap();
        std::fs::write(target.part_path(), b"half").unwrap();
        target.discard().await;
        assert!(!target.part_path().exists());
        assert_eq!(std::fs::read(target.destination()).unwrap(), b"keep");
    }

    /// 写包时进程没了，临时文件留在用户选的目录里。只收这两种命名，别的文件一概不碰。
    #[tokio::test]
    async fn leftovers_of_an_interrupted_export_are_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        for name in [
            "存档.zip",
            "存档.zip.part",
            "樱花 1.0.0.mrpack",
            "樱花 1.0.0.0a1b2c3d.part.mrpack",
            "樱花 1.0.0.notahex!.part.mrpack",
            "樱花 2.0.0.0a1b2c3d.part.mrpack",
            "别的.zip.part",
        ] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }

        assert_eq!(discard_export_leftovers(&dir.join("存档.zip")).await, 1);
        assert_eq!(
            discard_export_leftovers(&dir.join("樱花 1.0.0.mrpack")).await,
            1
        );

        let mut left: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(
            left,
            [
                "别的.zip.part",
                "存档.zip",
                "樱花 1.0.0.mrpack",
                "樱花 1.0.0.notahex!.part.mrpack",
                "樱花 2.0.0.0a1b2c3d.part.mrpack",
            ]
        );
    }

    #[tokio::test]
    async fn a_vanished_directory_has_no_leftovers() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("不在了").join("存档.zip");
        assert_eq!(discard_export_leftovers(&dest).await, 0);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn links_and_junctions_inside_extras_are_not_followed() {
        let tmp = tempfile::tempdir().unwrap();
        let petalfall = bare_petalfall(tmp.path());
        let dir = bare_instance(
            tmp.path(),
            "链接",
            r#"{"id":"链接","inheritsFrom":"1.21.1","type":"release","mainClass":"M"}"#,
        );
        VersionSettingsStore::for_version_dir(&dir)
            .save(&VersionSettings {
                isolation: IsolationOverride::Enabled,
                ..VersionSettings::default()
            })
            .await
            .unwrap();
        std::fs::create_dir_all(dir.join("config")).unwrap();
        std::fs::write(dir.join("config").join("real.txt"), b"real").unwrap();
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), b"secret").unwrap();
        for link in [dir.join("config").join("linked"), dir.join("resourcepacks")] {
            let made = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(&link)
                .arg(&outside)
                .output()
                .unwrap();
            assert!(made.status.success(), "{made:?}");
            assert!(
                std::fs::symlink_metadata(&link)
                    .unwrap()
                    .file_type()
                    .is_symlink(),
                "目录联接应被认作链接"
            );
        }
        let preview = petalfall.preview_modpack_export("链接").await.unwrap();
        let count = |extra: ExportExtra| {
            preview
                .extras
                .iter()
                .find(|s| s.extra == extra)
                .map(|s| s.files)
                .unwrap()
        };
        assert_eq!(count(ExportExtra::Config), 1, "只有 real.txt");
        assert_eq!(
            count(ExportExtra::ResourcePacks),
            0,
            "目录本身是联接也不跟随"
        );
    }

    // ---- 导出 ----

    fn options(extras: Vec<ExportExtra>, include_disabled: bool) -> ModpackExportOptions {
        ModpackExportOptions {
            name: "樱花林整合包".into(),
            version: "2.1.0".into(),
            summary: Some("春天的整合包".into()),
            extras,
            include_disabled,
        }
    }

    fn read_entry(pack: &Path, name: &str) -> Vec<u8> {
        let mut archive = zip::ZipArchive::new(std::fs::File::open(pack).unwrap()).unwrap();
        let mut bytes = Vec::new();
        archive
            .by_name(name)
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        bytes
    }

    fn entry_names(pack: &Path) -> Vec<String> {
        let archive = zip::ZipArchive::new(std::fs::File::open(pack).unwrap()).unwrap();
        let mut names: Vec<String> = archive.file_names().map(str::to_owned).collect();
        names.sort();
        names
    }

    fn leftovers(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".part.mrpack"))
            .collect()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn export_indexes_platform_mods_and_bundles_the_rest() {
        let fixture = fixture().await;
        mount_modrinth(&fixture, &cdn("pc", "vc", "c.jar")).await;
        let out = fixture.root.join("out");
        std::fs::create_dir_all(&out).unwrap();
        let target = ModpackExportTarget::new(out.join("樱花林整合包 2.1.0.mrpack"));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        let outcome = fixture
            .petalfall
            .export_modpack(
                ID,
                &options(vec![ExportExtra::Config], false),
                &target,
                Some(&tx),
            )
            .await
            .unwrap();
        drop(tx);

        let pack = target.destination();
        assert_eq!(outcome.path, pack);
        assert_eq!(outcome.bytes, std::fs::metadata(pack).unwrap().len());
        assert_eq!(
            (outcome.indexed_files, outcome.override_files),
            (3, 4),
            "d、e 两个 jar 加两个配置文件"
        );
        assert_eq!(outcome.bundled_mods, vec!["d.jar", "e.jar"]);
        assert_eq!(outcome.disabled_mods, 0);
        assert_eq!(outcome.skipped, vec!["config/nul.json"]);
        assert!(leftovers(&out).is_empty());

        let index: serde_json::Value =
            serde_json::from_slice(&read_entry(pack, "modrinth.index.json")).unwrap();
        assert_eq!(index["name"], "樱花林整合包");
        assert_eq!(index["versionId"], "2.1.0");
        assert_eq!(index["summary"], "春天的整合包");
        assert_eq!(
            index["dependencies"],
            serde_json::json!({"minecraft": "test", "fabric-loader": "0.15.0"})
        );
        let files = index["files"].as_array().unwrap();
        assert_eq!(files.len(), 3);
        for (file, (name, project, version)) in files.iter().zip([
            ("a.jar", "pa", "va"),
            ("b.jar", "pb", "vb"),
            ("c.jar", "pc", "vc"),
        ]) {
            let bytes = &fixture.jars[name];
            let sha1 = sha1_of(bytes);
            assert_eq!(file["path"], format!("mods/{name}"));
            assert_eq!(file["hashes"]["sha1"], sha1);
            assert_eq!(
                file["hashes"]["sha512"],
                fake_sha512(&sha1).to_ascii_lowercase()
            );
            assert_eq!(file["fileSize"], bytes.len());
            assert_eq!(
                file["downloads"],
                serde_json::json!([cdn(project, version, name)])
            );
            assert!(file.get("env").is_none());
        }

        assert_eq!(
            entry_names(pack),
            vec![
                "modrinth.index.json",
                "overrides/config/sodium-options.json",
                "overrides/config/sub/deep.toml",
                "overrides/mods/d.jar",
                "overrides/mods/e.jar",
            ]
        );
        assert_eq!(
            read_entry(pack, "overrides/mods/d.jar"),
            fixture.jars["d.jar"]
        );

        let mut stages = Vec::new();
        let mut warnings = Vec::new();
        let mut downloads = Vec::new();
        while let Some(event) = rx.recv().await {
            match event {
                CoreEvent::Stage(s) => stages.push(s),
                CoreEvent::Warning(w) => warnings.push(w),
                CoreEvent::Download(p) => downloads.push(p),
                CoreEvent::Phase { .. } | CoreEvent::Source { .. } => {}
            }
        }
        assert_eq!(
            stages,
            vec![
                "读取 5 个 Mod 文件".to_owned(),
                "向 Modrinth 查询 5 个 Mod".to_owned(),
                "打包 4 个文件".to_owned(),
                "校验导出的整合包".to_owned(),
                format!("已导出到 {}", pack.display()),
            ]
        );
        assert_eq!(
            warnings,
            vec![
                "2 个 Mod 不在 Modrinth 上，已直接打包进文件",
                "1 个文件名不能安全写进整合包，已跳过"
            ]
        );
        let last = downloads.last().expect("应有打包进度");
        let override_bytes = (fixture.jars["d.jar"].len()
            + fixture.jars["e.jar"].len()
            + b"{\"quality\":1}".len()
            + b"deep = true".len()) as u64;
        assert_eq!((last.total, last.finished), (4, 4));
        assert_eq!(last.bytes, override_bytes);
        assert_eq!(last.total_bytes, Some(override_bytes));
        assert_eq!(last.transferred, 0, "打包只写本机文件，没有网络传输");
    }

    /// 弹窗按预览给的分项和当前勾选算「会跳过几个」，这里钉死它与导出结果同一口径：
    /// mods 目录的总算，额外项的只算勾上的。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn skipped_files_of_unselected_extras_are_not_reported() {
        let fixture = fixture().await;
        mount_modrinth(&fixture, &cdn("pc", "vc", "c.jar")).await;
        fixture.write("mods/con.jar", b"reserved");
        fixture.write("mods/aux.jar", b"reserved");
        fixture.write("saves/World/prn.dat", b"reserved");

        let preview = fixture.petalfall.preview_modpack_export(ID).await.unwrap();
        assert_eq!(preview.mod_skipped, vec!["mods/aux.jar", "mods/con.jar"]);

        let cases: [(&[ExportExtra], &[&str]); 4] = [
            (&[], &["mods/aux.jar", "mods/con.jar"]),
            (
                &[ExportExtra::Config],
                &["config/nul.json", "mods/aux.jar", "mods/con.jar"],
            ),
            (
                &[ExportExtra::Saves],
                &["mods/aux.jar", "mods/con.jar", "saves/World/prn.dat"],
            ),
            (
                &ExportExtra::ALL,
                &[
                    "config/nul.json",
                    "mods/aux.jar",
                    "mods/con.jar",
                    "saves/World/prn.dat",
                ],
            ),
        ];
        for (index, (selected, expected)) in cases.into_iter().enumerate() {
            let target = ModpackExportTarget::new(fixture.root.join(format!("跳过{index}.mrpack")));
            let outcome = fixture
                .petalfall
                .export_modpack(ID, &options(selected.to_vec(), false), &target, None)
                .await
                .unwrap();
            assert_eq!(outcome.skipped, expected, "勾选 {selected:?}");

            let mut counted = preview.mod_skipped.clone();
            for stat in preview
                .extras
                .iter()
                .filter(|s| selected.contains(&s.extra))
            {
                counted.extend(stat.skipped.iter().cloned());
            }
            counted.sort();
            assert_eq!(counted, outcome.skipped, "勾选 {selected:?}");
        }
    }

    /// 名字不安全的停用 jar 只在带上停用的 Mod 时才算跳过：没带它们本来就不进包，算进去就把数报多了。
    /// 启用的 jar 不看这个勾选。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unsafe_disabled_jars_count_as_skipped_only_when_disabled_mods_are_included() {
        let fixture = fixture().await;
        mount_modrinth(&fixture, &cdn("pc", "vc", "c.jar")).await;
        fixture.write("mods/con.jar", b"reserved");
        fixture.write("mods/aux.jar.disabled", b"reserved");
        fixture.write("mods/prn.jar.disabled", b"reserved");

        let preview = fixture.petalfall.preview_modpack_export(ID).await.unwrap();
        assert_eq!(preview.mod_skipped, vec!["mods/con.jar"]);
        assert_eq!(
            preview.disabled_skipped,
            vec!["mods/aux.jar.disabled", "mods/prn.jar.disabled"]
        );

        let without = ModpackExportTarget::new(fixture.root.join("不带停用.mrpack"));
        let outcome = fixture
            .petalfall
            .export_modpack(ID, &options(Vec::new(), false), &without, None)
            .await
            .unwrap();
        assert_eq!(outcome.skipped, vec!["mods/con.jar"]);
        assert_eq!(outcome.disabled_mods, 0);

        let with = ModpackExportTarget::new(fixture.root.join("带上停用.mrpack"));
        let outcome = fixture
            .petalfall
            .export_modpack(ID, &options(Vec::new(), true), &with, None)
            .await
            .unwrap();
        assert_eq!(
            outcome.skipped,
            vec![
                "mods/aux.jar.disabled",
                "mods/con.jar",
                "mods/prn.jar.disabled"
            ]
        );
        assert_eq!(outcome.disabled_mods, 1, "名字安全的那一个照常带上");
    }

    /// 存档可能很大：导出只遍历勾选的额外项，勾重了也只走一遍，预览才遍历全部。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn planning_walks_only_the_requested_extras() {
        let fixture = fixture().await;
        mount_modrinth(&fixture, &cdn("pc", "vc", "c.jar")).await;
        let walked = |plan: &ExportPlan| plan.extras.keys().copied().collect::<Vec<_>>();

        let none = fixture
            .petalfall
            .plan_export(ID, &[], None, None, None, false)
            .await;
        assert_eq!(walked(&none.unwrap()), Vec::<ExportExtra>::new());

        let some = fixture
            .petalfall
            .plan_export(
                ID,
                &[
                    ExportExtra::Options,
                    ExportExtra::Config,
                    ExportExtra::Options,
                ],
                None,
                None,
                None,
                false,
            )
            .await
            .unwrap();
        assert_eq!(walked(&some), [ExportExtra::Config, ExportExtra::Options]);
        assert_eq!(some.extras[&ExportExtra::Config].files.len(), 2);
        assert_eq!(some.extras[&ExportExtra::Options].files.len(), 1);

        let all = fixture
            .petalfall
            .plan_export(ID, &ExportExtra::ALL, None, None, None, true);
        assert_eq!(walked(&all.await.unwrap()), ExportExtra::ALL);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn disabled_mods_ride_along_only_when_asked_and_existing_files_are_replaced() {
        let fixture = fixture().await;
        mount_modrinth(&fixture, &cdn("pc", "vc", "c.jar")).await;
        let destination = fixture.root.join("已有.mrpack");
        std::fs::write(&destination, b"an older export").unwrap();
        let target = ModpackExportTarget::new(destination.clone());

        let outcome = fixture
            .petalfall
            .export_modpack(ID, &options(ExportExtra::ALL.to_vec(), true), &target, None)
            .await
            .unwrap();
        assert_eq!(outcome.disabled_mods, 1);
        let names = entry_names(&destination);
        for expected in [
            "overrides/mods/x.jar.disabled",
            "overrides/options.txt",
            "overrides/resourcepacks/pack.zip",
            "overrides/saves/World/level.dat",
        ] {
            assert!(
                names.contains(&expected.to_owned()),
                "{expected}: {names:?}"
            );
        }
        for name in &names {
            let lower = name.to_lowercase();
            assert!(!lower.contains(PETALFALL_META_DIR), "{name}");
            assert!(!lower.ends_with(".old"), "{name}");
            assert!(
                !lower.contains("logs/") && !lower.contains("crash-reports/"),
                "{name}"
            );
            assert!(
                !lower.contains("mods/sub/") && !lower.ends_with("readme.txt"),
                "{name}"
            );
            assert!(
                !lower.ends_with(".json")
                    || lower.starts_with("overrides/config/")
                    || lower == "modrinth.index.json",
                "{name}"
            );
        }
        assert_eq!(
            read_entry(&destination, "overrides/mods/x.jar.disabled"),
            fixture.jars["x.jar.disabled"]
        );
        assert!(leftovers(&fixture.root).is_empty());
    }

    /// 游戏开着存档时对 session.lock 持有排他锁，别的句柄读它会失败；它对导入的人也没有意义。
    /// 带存档导出时任何层级的 session.lock 都不读、不进包，预览的计数同样不算它。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn saves_leave_out_session_locks_even_while_the_game_holds_one() {
        let fixture = fixture().await;
        mount_modrinth(&fixture, &cdn("pc", "vc", "c.jar")).await;
        fixture.write("saves/World/session.lock", "\u{2603}".as_bytes());
        fixture.write("saves/World/DIM-1/SESSION.LOCK", b"x");
        fixture.write("saves/World/region/r.0.0.mca", b"region");
        let holder = std::fs::OpenOptions::new()
            .write(true)
            .open(fixture.dir.join("saves/World/session.lock"))
            .unwrap();
        holder.lock().unwrap();

        let preview = fixture.petalfall.preview_modpack_export(ID).await;
        let target = ModpackExportTarget::new(fixture.root.join("开着存档.mrpack"));
        let outcome = fixture
            .petalfall
            .export_modpack(ID, &options(vec![ExportExtra::Saves], false), &target, None)
            .await;
        holder.unlock().unwrap();
        let outcome = outcome.unwrap();

        let saves = preview
            .unwrap()
            .extras
            .into_iter()
            .find(|s| s.extra == ExportExtra::Saves)
            .unwrap();
        assert_eq!(
            (saves.files, saves.bytes),
            (2, (b"level".len() + b"region".len()) as u64)
        );
        assert_eq!(outcome.override_files, 4, "d、e 两个 jar 加两个存档文件");
        let saves_entries: Vec<String> = entry_names(target.destination())
            .into_iter()
            .filter(|n| n.starts_with("overrides/saves/"))
            .collect();
        assert_eq!(
            saves_entries,
            vec![
                "overrides/saves/World/level.dat",
                "overrides/saves/World/region/r.0.0.mca",
            ]
        );
        assert!(leftovers(&fixture.root).is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_pre_cancelled_export_leaves_neither_destination_nor_part() {
        let fixture = fixture().await;
        mount_modrinth(&fixture, &cdn("pc", "vc", "c.jar")).await;
        let token = CancellationToken::new();
        token.cancel();
        let petalfall = fixture.petalfall.clone().with_cancel(token);
        let target = ModpackExportTarget::new(fixture.root.join("取消.mrpack"));
        let err = petalfall
            .export_modpack(ID, &options(vec![ExportExtra::Config], true), &target, None)
            .await
            .unwrap_err();
        assert!(err.is_cancelled(), "{err:?}");
        assert!(!target.destination().exists());
        assert!(!target.part_path().exists());
        assert!(leftovers(&fixture.root).is_empty());
        assert!(fixture.server.received_requests().await.unwrap().is_empty());
    }

    // ---- 导出的步骤 ----

    /// 收下通道里的全部事件；发送端要先全部释放。
    async fn drain(mut rx: tokio::sync::mpsc::UnboundedReceiver<CoreEvent>) -> Vec<CoreEvent> {
        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }
        events
    }

    /// 步骤事件的下标，按出现先后；每一条带的都必须是导出那张完整的表。
    fn export_steps(events: &[CoreEvent]) -> Vec<usize> {
        events
            .iter()
            .filter_map(|event| match event {
                CoreEvent::Phase { plan, index } => {
                    assert_eq!(plan.as_slice(), EXPORT_PHASES.as_slice());
                    Some(*index)
                }
                _ => None,
            })
            .collect()
    }

    /// 进入 `phase` 那条步骤事件在事件序列里的位置。
    fn step_at(events: &[CoreEvent], phase: InstallPhase) -> usize {
        events
            .iter()
            .position(|e| matches!(e, CoreEvent::Phase { plan, index } if plan[*index] == phase))
            .unwrap_or_else(|| panic!("没有报 {phase:?}：{events:?}"))
    }

    /// 以 `prefix` 开头的那条阶段文字在事件序列里的位置。
    fn stage_at(events: &[CoreEvent], prefix: &str) -> usize {
        events
            .iter()
            .position(|e| matches!(e, CoreEvent::Stage(s) if s.starts_with(prefix)))
            .unwrap_or_else(|| panic!("没有「{prefix}」：{events:?}"))
    }

    /// 四步报全，每一步落在它盖住的那段工作开头：算哈希是开工后的第一条事件，认 Mod 在读完 Mod 之后、问
    /// Modrinth 之前，凑包在问完之后、两条告警与「打包」之前，校验在「打包」之后、「校验」与「已导出」之前。
    /// 写包进度都落在「打包」与「已导出」之间；节流转发的终值快照常常晚于进入校验一步，所以不要求它们排在校验之前。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_export_announces_its_four_steps_in_order() {
        use InstallPhase::*;
        let fixture = fixture().await;
        mount_modrinth(&fixture, &cdn("pc", "vc", "c.jar")).await;
        let target = ModpackExportTarget::new(fixture.root.join("分步.mrpack"));
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

        let outcome = fixture
            .petalfall
            .export_modpack(
                ID,
                &options(vec![ExportExtra::Config], false),
                &target,
                Some(&tx),
            )
            .await
            .unwrap();
        drop(tx);
        let events = drain(rx).await;

        assert_eq!((outcome.indexed_files, outcome.override_files), (3, 4));
        assert_eq!(export_steps(&events), [0, 1, 2, 3]);
        let (hash, identify, pack, verify) = (
            step_at(&events, ExportHash),
            step_at(&events, ExportIdentify),
            step_at(&events, ExportPack),
            step_at(&events, ExportVerify),
        );
        let reading = stage_at(&events, "读取 5 个 Mod 文件");
        let querying = stage_at(&events, "向 Modrinth 查询 5 个 Mod");
        let packing = stage_at(&events, "打包 4 个文件");
        let verifying = stage_at(&events, "校验导出的整合包");
        let done = stage_at(
            &events,
            &format!("已导出到 {}", target.destination().display()),
        );
        assert_eq!(hash, 0, "开工后的第一条事件就是第一步：{events:?}");
        assert!(
            hash < reading && reading < identify && identify < querying,
            "{events:?}"
        );
        assert!(querying < pack && pack < packing, "{events:?}");
        assert!(
            packing < verify && verify < verifying && verifying < done,
            "{events:?}"
        );
        assert_eq!(done, events.len() - 1, "{events:?}");

        let warnings: Vec<usize> = (0..events.len())
            .filter(|&i| matches!(events[i], CoreEvent::Warning(_)))
            .collect();
        assert_eq!(warnings.len(), 2, "{events:?}");
        assert!(
            warnings.iter().all(|&i| pack < i && i < packing),
            "{events:?}"
        );
        let downloads: Vec<usize> = (0..events.len())
            .filter(|&i| matches!(events[i], CoreEvent::Download(_)))
            .collect();
        assert!(!downloads.is_empty(), "应有打包进度");
        assert!(
            downloads.iter().all(|&i| packing < i && i < done),
            "{events:?}"
        );
    }

    /// 没有启用的 Mod 时不问 Modrinth，认 Mod 这一步瞬间过去也照报：表是固定的，跳过一步步号会断档。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_instance_without_enabled_mods_still_announces_every_step() {
        use InstallPhase::*;
        let fixture = fixture().await;
        for name in ["a.jar", "b.jar", "c.jar", "d.jar", "e.jar"] {
            std::fs::remove_file(fixture.dir.join("mods").join(name)).unwrap();
        }
        let target = ModpackExportTarget::new(fixture.root.join("没有 Mod.mrpack"));
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

        let outcome = fixture
            .petalfall
            .export_modpack(
                ID,
                &options(vec![ExportExtra::Config], true),
                &target,
                Some(&tx),
            )
            .await
            .unwrap();
        drop(tx);
        let events = drain(rx).await;

        assert_eq!(
            (
                outcome.indexed_files,
                outcome.override_files,
                outcome.disabled_mods
            ),
            (0, 3, 1),
            "停用的 x.jar 加两个配置文件"
        );
        assert_eq!(export_steps(&events), [0, 1, 2, 3]);
        let identify = step_at(&events, ExportIdentify);
        assert!(
            stage_at(&events, "读取 0 个 Mod 文件") < identify,
            "{events:?}"
        );
        assert!(identify < step_at(&events, ExportPack), "{events:?}");
        assert!(
            step_at(&events, ExportPack) < stage_at(&events, "打包 3 个文件"),
            "{events:?}"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, CoreEvent::Stage(s) if s.starts_with("向 Modrinth"))),
            "{events:?}"
        );
        assert!(fixture.server.received_requests().await.unwrap().is_empty());
    }

    /// 没有要打包的文件（Mod 全在 Modrinth 上、不带额外项）时写包不进逐个文件的循环，收尾后的那一下回调照样
    /// 转到校验：这一步与「校验导出的整合包」都报到。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_export_with_nothing_to_bundle_still_reaches_verification() {
        use InstallPhase::*;
        let fixture = fixture().await;
        mount_modrinth(&fixture, &cdn("pc", "vc", "c.jar")).await;
        for name in ["d.jar", "e.jar"] {
            std::fs::remove_file(fixture.dir.join("mods").join(name)).unwrap();
        }
        let target = ModpackExportTarget::new(fixture.root.join("只有清单.mrpack"));
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

        let outcome = fixture
            .petalfall
            .export_modpack(ID, &options(Vec::new(), false), &target, Some(&tx))
            .await
            .unwrap();
        drop(tx);
        let events = drain(rx).await;

        assert_eq!((outcome.indexed_files, outcome.override_files), (3, 0));
        assert!(outcome.bundled_mods.is_empty());
        assert_eq!(
            entry_names(target.destination()),
            vec!["modrinth.index.json"]
        );
        assert_eq!(export_steps(&events), [0, 1, 2, 3]);
        let verify = step_at(&events, ExportVerify);
        assert!(stage_at(&events, "打包 0 个文件") < verify, "{events:?}");
        let verifying = stage_at(&events, "校验导出的整合包");
        assert!(verify < verifying, "{events:?}");
        assert!(verifying < stage_at(&events, "已导出到 "), "{events:?}");
    }

    fn write_progress(
        files_done: u64,
        files_total: u64,
        bytes_done: u64,
        bytes_total: u64,
    ) -> WriteProgress {
        WriteProgress {
            files_done,
            files_total,
            bytes_done,
            bytes_total,
        }
    }

    /// 按写包回调的先后把进度逐份喂给同一个 [`LastFileWritten`]，记下每一份是否转入校验。
    fn verify_turns(progress: &[WriteProgress]) -> Vec<bool> {
        let mut last_file = LastFileWritten::default();
        progress.iter().map(|p| last_file.reached(*p)).collect()
    }

    /// 只在第一次写完的文件数追平总数时转入校验，收尾那一下再追平不算。
    #[test]
    fn verification_starts_once_when_the_last_file_is_written() {
        // 两个文件各 8 字节、每块 4 字节：前一个的两块、写完它、后一个的两块、写完它、收尾（终点换成实际写入的）。
        let two = [
            write_progress(0, 2, 4, 16),
            write_progress(0, 2, 8, 16),
            write_progress(1, 2, 8, 16),
            write_progress(1, 2, 12, 16),
            write_progress(1, 2, 16, 16),
            write_progress(2, 2, 16, 16),
            write_progress(2, 2, 16, 16),
        ];
        assert_eq!(
            verify_turns(&two),
            [false, false, false, false, false, true, false]
        );
        // 没有要打包的文件：只有收尾那一下，它就追平。
        assert_eq!(verify_turns(&[write_progress(0, 0, 0, 0)]), [true]);
        // 只有一个文件：两块、写完、收尾。
        let one = [
            write_progress(0, 1, 5, 9),
            write_progress(0, 1, 9, 9),
            write_progress(1, 1, 9, 9),
            write_progress(1, 1, 9, 9),
        ];
        assert_eq!(verify_turns(&one), [false, false, true, false]);
    }

    /// 随机的文件数（含 0）与每个文件的块数（含 0，空文件不回调块），照写包回调的形状造序列：恰好一次转入校验，
    /// 落在最后一个文件写完那一下；没有文件时落在收尾那一下。
    #[test]
    fn verification_starts_right_after_the_last_file_for_any_pack_shape() {
        let seed = fastrand::get_seed();
        let chunks: Vec<Vec<u64>> = (0..fastrand::usize(0..=6))
            .map(|_| {
                (0..fastrand::usize(0..=4))
                    .map(|_| fastrand::u64(1..=4096))
                    .collect()
            })
            .collect();
        let files = chunks.len() as u64;
        let total: u64 = chunks.iter().flatten().sum();
        let mut sequence = Vec::new();
        let mut bytes = 0;
        for (done, file) in (0..).zip(&chunks) {
            for chunk in file {
                bytes += chunk;
                sequence.push(write_progress(done, files, bytes, total));
            }
            sequence.push(write_progress(done + 1, files, bytes, total));
        }
        sequence.push(write_progress(files, files, bytes, bytes));

        let expected = sequence.len() - if files == 0 { 1 } else { 2 };
        let turned: Vec<usize> = verify_turns(&sequence)
            .into_iter()
            .enumerate()
            .filter_map(|(at, turned)| turned.then_some(at))
            .collect();
        assert_eq!(turned, vec![expected], "seed {seed}：{sequence:?}");
    }

    /// 预览不属于任何任务，规划时不带播报器：事件出口在，也只有两条阶段文字，一条步骤都不报。
    /// 预览本身连事件出口都没有，这里直接走它用的那次规划。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn planning_for_a_preview_announces_no_steps() {
        let fixture = fixture().await;
        mount_modrinth(&fixture, &cdn("pc", "vc", "c.jar")).await;
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

        let plan = fixture
            .petalfall
            .plan_export(ID, &ExportExtra::ALL, None, Some(&tx), None, true)
            .await
            .unwrap();
        drop(tx);

        assert_eq!((plan.indexed.len(), plan.bundled.len()), (3, 2));
        assert_eq!(
            drain(rx).await,
            vec![
                CoreEvent::stage("读取 5 个 Mod 文件"),
                CoreEvent::stage("向 Modrinth 查询 5 个 Mod"),
            ]
        );
    }

    /// 在认 Mod 这一步取消：联网的等待立即放弃，以取消错误返回，后面的步骤一条都不再报。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelling_during_a_step_announces_no_later_steps() {
        let fixture = fixture().await;
        Mock::given(method("GET"))
            .and(path("/versions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!([]))
                    .set_delay(Duration::from_secs(30)),
            )
            .mount(&fixture.server)
            .await;
        let token = CancellationToken::new();
        let petalfall = fixture.petalfall.clone().with_cancel(token.clone());
        let target = ModpackExportTarget::new(fixture.root.join("中途取消.mrpack"));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let watcher = tokio::spawn(async move {
            let mut events = Vec::new();
            while let Some(event) = rx.recv().await {
                if matches!(&event, CoreEvent::Phase { plan, index } if plan[*index] == InstallPhase::ExportIdentify)
                {
                    token.cancel();
                }
                events.push(event);
            }
            events
        });

        let started = Instant::now();
        let err = petalfall
            .export_modpack(
                ID,
                &options(vec![ExportExtra::Config], false),
                &target,
                Some(&tx),
            )
            .await
            .unwrap_err();
        let elapsed = started.elapsed();
        drop(tx);
        let events = watcher.await.unwrap();

        assert!(err.is_cancelled(), "{err:?}");
        assert!(
            elapsed < Duration::from_secs(10),
            "取消后应立即放弃等待 Modrinth：{elapsed:?}"
        );
        assert_eq!(
            events,
            vec![
                CoreEvent::Phase {
                    plan: EXPORT_PHASES.to_vec(),
                    index: 0
                },
                CoreEvent::stage("读取 5 个 Mod 文件"),
                CoreEvent::Phase {
                    plan: EXPORT_PHASES.to_vec(),
                    index: 1
                },
                CoreEvent::stage("向 Modrinth 查询 5 个 Mod"),
            ]
        );
        assert!(!target.destination().exists());
        assert!(!target.part_path().exists());
    }

    /// 开工前就已取消的导出连第一步都不报，一条事件都没有。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_pre_cancelled_export_announces_no_step() {
        let fixture = fixture().await;
        let token = CancellationToken::new();
        token.cancel();
        let petalfall = fixture.petalfall.clone().with_cancel(token);
        let target = ModpackExportTarget::new(fixture.root.join("先取消.mrpack"));
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

        let err = petalfall
            .export_modpack(ID, &options(Vec::new(), false), &target, Some(&tx))
            .await
            .unwrap_err();
        drop(tx);

        assert!(err.is_cancelled(), "{err:?}");
        assert_eq!(drain(rx).await, Vec::<CoreEvent>::new());
    }

    /// 没有事件出口时播报器照样按表推进、只是不发：校验那一步在阻塞线程里进入也不出错，导出照常成功。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_export_without_an_event_sink_succeeds() {
        let fixture = fixture().await;
        mount_modrinth(&fixture, &cdn("pc", "vc", "c.jar")).await;
        let target = ModpackExportTarget::new(fixture.root.join("无出口.mrpack"));

        let outcome = fixture
            .petalfall
            .export_modpack(
                ID,
                &options(vec![ExportExtra::Config], false),
                &target,
                None,
            )
            .await
            .unwrap();

        assert_eq!((outcome.indexed_files, outcome.override_files), (3, 4));
        assert_eq!(outcome.bundled_mods, vec!["d.jar", "e.jar"]);
        assert_eq!(
            entry_names(target.destination()),
            vec![
                "modrinth.index.json",
                "overrides/config/sodium-options.json",
                "overrides/config/sub/deep.toml",
                "overrides/mods/d.jar",
                "overrides/mods/e.jar",
            ]
        );
        assert!(leftovers(&fixture.root).is_empty());
    }

    /// 把清单里的正式 CDN 地址改写到本地 mock，导入侧对清单地址的白名单校验不变。
    struct CdnToMock(String);

    impl petalfall_download::SourceResolver for CdnToMock {
        fn resolve(
            &self,
            url: &str,
            _source: petalfall_download::MirrorSource,
        ) -> petalfall_download::Result<String> {
            Ok(url.replace("https://cdn.modrinth.com/", &format!("{}/cdn/", self.0)))
        }
    }

    fn mod_files(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap())
            .filter(|e| e.file_type().unwrap().is_file())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".jar") || n.ends_with(".jar.disabled"))
            .collect();
        names.sort();
        names
    }

    /// 导出的包经现有导入流程装成新实例：下载的、打包的、停用的 Mod 一个不少，整合包信息是导出时填的。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_exported_pack_imports_back_into_the_same_mods() {
        let fixture = fixture().await;
        mount_modrinth(&fixture, &cdn("pc", "vc", "c.jar")).await;
        let base = fixture.server.uri();
        Mock::given(method("GET"))
            .and(path("/manifest.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(format!(
                r#"{{"latest":{{"release":"test","snapshot":"test"}},
                    "versions":[{{"id":"test","type":"release","url":"{base}/test.json",
                    "time":"t","releaseTime":"t"}}]}}"#
            )))
            .mount(&fixture.server)
            .await;
        Mock::given(method("GET"))
            .and(path("/test.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"id":"test","type":"release","mainClass":"net.minecraft.client.main.Main"}"#,
            ))
            .mount(&fixture.server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v2/versions/loader/test/0.15.0/profile/json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"id":"fabric-loader-0.15.0-test","inheritsFrom":"test","type":"release",
                    "mainClass":"net.fabricmc.loader.impl.launch.knot.KnotClient",
                    "libraries":[]}"#,
            ))
            .mount(&fixture.server)
            .await;
        for (name, project, version) in [
            ("a.jar", "pa", "va"),
            ("b.jar", "pb", "vb"),
            ("c.jar", "pc", "vc"),
        ] {
            Mock::given(method("GET"))
                .and(path(format!(
                    "/cdn/data/{project}/versions/{version}/{name}"
                )))
                .respond_with(ResponseTemplate::new(200).set_body_bytes(fixture.jars[name].clone()))
                .mount(&fixture.server)
                .await;
        }
        let petalfall = fixture
            .petalfall
            .clone()
            .with_download_resolver(Arc::new(CdnToMock(base.clone())));
        let target = ModpackExportTarget::new(fixture.root.join("往返.mrpack"));
        petalfall
            .export_modpack(ID, &options(vec![ExportExtra::Config], true), &target, None)
            .await
            .unwrap();

        let imported = petalfall
            .install_modpack(
                &ModpackSource::File {
                    path: target.destination().to_path_buf(),
                },
                Some("导入回来"),
                &crate::modpack::ImportSelection::default(),
                "roundtrip",
                None,
            )
            .await
            .unwrap();
        assert_eq!(imported.modpack.name, "樱花林整合包");
        assert_eq!(imported.modpack.version.as_deref(), Some("2.1.0"));
        assert_eq!(imported.modpack.remote_files, 3);
        let new_mods = imported.version_dir.join("mods");
        assert_eq!(mod_files(&new_mods), mod_files(&fixture.dir.join("mods")));
        assert_eq!(
            mod_files(&new_mods),
            vec![
                "a.jar",
                "b.jar",
                "c.jar",
                "d.jar",
                "e.jar",
                "x.jar.disabled"
            ]
        );
        for (name, bytes) in &fixture.jars {
            assert_eq!(
                &std::fs::read(new_mods.join(name)).unwrap(),
                bytes,
                "{name}"
            );
        }
        assert_eq!(
            std::fs::read(
                imported
                    .version_dir
                    .join("config")
                    .join("sub")
                    .join("deep.toml")
            )
            .unwrap(),
            b"deep = true"
        );
        let cdn_hits = fixture
            .server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path().starts_with("/cdn/"))
            .count();
        assert_eq!(cdn_hits, 3, "走索引的三个 Mod 各下载一次");
    }

    // ---- 启动器补进模组默认设置的行 ----

    const YOSBR: &str = "config/yosbr/options.txt";
    const DEFAULT_OPTIONS: &str = "config/defaultoptions/options.txt";
    const EXTRA_OPTIONS: &str = "config/defaultoptions/extra/options.txt";

    /// 照启动前那样补全 options.txt：工作目录取启动用的同一个解析（`resolve_working_dir`），系统语言是简体中文，
    /// 跳过引导。返回工作目录。
    async fn fill_like_a_launch(petalfall: &Petalfall, id: &str, scratch: &Path) -> PathBuf {
        let layout = petalfall_install::GameLayout::new(scratch.join("assets-root"));
        let index = layout.asset_index_json("lang-test").unwrap();
        std::fs::create_dir_all(index.parent().unwrap()).unwrap();
        let objects = BTreeMap::from([(
            "minecraft/lang/zh_cn.json".to_owned(),
            petalfall_version::AssetObject {
                hash: "ab".repeat(20),
                size: 1,
            },
        )]);
        let index_json = petalfall_version::AssetObjectsIndex {
            objects,
            ..petalfall_version::AssetObjectsIndex::default()
        };
        std::fs::write(&index, serde_json::to_vec(&index_json).unwrap()).unwrap();
        let working_dir = petalfall.resolve_working_dir(id).await.unwrap().working_dir;
        let inputs = crate::game_lang::OptionsInputs {
            system: Some("zh-CN"),
            asset_index_id: Some("lang-test"),
            skip_onboarding: true,
            follow_system: true,
        };
        let report = crate::game_lang::fill_missing_options(&inputs, &layout, &working_dir).await;
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        working_dir
    }

    /// 包里 `overrides/<prefix>` 下的条目与各自的字节数。
    fn entry_sizes(pack: &Path, prefix: &str) -> BTreeMap<String, u64> {
        let mut archive = zip::ZipArchive::new(std::fs::File::open(pack).unwrap()).unwrap();
        (0..archive.len())
            .map(|at| {
                let entry = archive.by_index(at).unwrap();
                (entry.name().to_owned(), entry.size())
            })
            .filter(|(name, _)| name.starts_with(&format!("overrides/{prefix}")))
            .collect()
    }

    fn read_all(dir: &Path, files: &[&str]) -> Vec<Vec<u8>> {
        files
            .iter()
            .map(|file| std::fs::read(dir.join(file)).unwrap())
            .collect()
    }

    /// 启动前补进模组默认设置的语言与引导行是按本机补的，不随包带走：包里那几份是作者原来的字节，作者自己写的
    /// `lang:zh_cn` 照留；玩家自己的 options.txt 照原样带上。盘上的文件不动，记账文件不进包，预览报的配置大小
    /// 与写进包里的一致。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn lines_the_launcher_filled_into_mod_defaults_stay_out_of_the_pack() {
        let fixture = fixture().await;
        mount_modrinth(&fixture, &cdn("pc", "vc", "c.jar")).await;
        std::fs::remove_file(fixture.dir.join(OPTIONS_FILE)).unwrap();
        let authored: [(&str, &[u8]); 3] = [
            (YOSBR, b"version:3955\r\nfov:0.8\r\n\xff\xfe:1\r\n"),
            (DEFAULT_OPTIONS, b"lang:zh_cn\nfov:0.6\n"),
            (EXTRA_OPTIONS, b"fov:0.7\n"),
        ];
        for (file, bytes) in authored {
            fixture.write(file, bytes);
        }

        let working_dir = fill_like_a_launch(&fixture.petalfall, ID, &fixture.root).await;

        assert_eq!(working_dir, fixture.dir);
        assert_eq!(
            read_all(&fixture.dir, &[YOSBR, DEFAULT_OPTIONS, EXTRA_OPTIONS]),
            [
                &b"version:3955\r\nfov:0.8\r\n\xff\xfe:1\r\nlang:zh_cn\r\nonboardAccessibility:false\r\n"[..],
                b"lang:zh_cn\nfov:0.6\nonboardAccessibility:false\n",
                b"fov:0.7\nlang:zh_cn\nonboardAccessibility:false\n",
            ]
        );
        let record = crate::game_lang::filled_lines_path(&fixture.dir);
        assert!(record.is_file(), "补行之后应有记账");
        fixture.write(OPTIONS_FILE, b"lang:zh_cn\nonboardAccessibility:false\n");
        let on_disk = read_all(
            &fixture.dir,
            &[YOSBR, DEFAULT_OPTIONS, EXTRA_OPTIONS, OPTIONS_FILE],
        );
        let record_bytes = std::fs::read(&record).unwrap();

        let preview = fixture.petalfall.preview_modpack_export(ID).await.unwrap();
        let target = ModpackExportTarget::new(fixture.root.join("补行.mrpack"));
        fixture
            .petalfall
            .export_modpack(
                ID,
                &options(vec![ExportExtra::Config, ExportExtra::Options], false),
                &target,
                None,
            )
            .await
            .unwrap();

        let pack = target.destination();
        for (file, bytes) in authored {
            assert_eq!(
                read_entry(pack, &format!("overrides/{file}")),
                bytes,
                "{file}"
            );
        }
        assert_eq!(
            read_entry(pack, "overrides/options.txt"),
            b"lang:zh_cn\nonboardAccessibility:false\n",
            "options.txt 是玩家自己的设置，原样带上"
        );
        let record_name = record.file_name().unwrap().to_string_lossy().to_lowercase();
        for name in entry_names(pack) {
            let lower = name.to_lowercase();
            assert!(
                !lower.contains(PETALFALL_META_DIR) && !lower.ends_with(&record_name),
                "{name}"
            );
        }
        assert_eq!(
            read_all(
                &fixture.dir,
                &[YOSBR, DEFAULT_OPTIONS, EXTRA_OPTIONS, OPTIONS_FILE]
            ),
            on_disk
        );
        assert_eq!(std::fs::read(&record).unwrap(), record_bytes);

        let config = preview
            .extras
            .iter()
            .find(|stat| stat.extra == ExportExtra::Config)
            .unwrap();
        let written = entry_sizes(pack, "config/");
        assert_eq!(config.files, written.len() as u64);
        assert_eq!(config.bytes, written.values().sum::<u64>());
    }

    /// 记账之后文件被人改过：记着的行不在了就不去，不拿别的行顶替；记账里没有的文件，作者写的同样的行也照留。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn lines_that_changed_since_they_were_filled_stay_in_the_pack() {
        let fixture = fixture().await;
        mount_modrinth(&fixture, &cdn("pc", "vc", "c.jar")).await;
        std::fs::remove_file(fixture.dir.join(OPTIONS_FILE)).unwrap();
        fixture.write(YOSBR, b"fov:0.8\n");
        fill_like_a_launch(&fixture.petalfall, ID, &fixture.root).await;
        assert_eq!(
            std::fs::read(fixture.dir.join(YOSBR)).unwrap(),
            b"fov:0.8\nlang:zh_cn\nonboardAccessibility:false\n"
        );

        fixture.write(
            YOSBR,
            b"fov:0.8\nlang:ja_jp\nonboardAccessibility:false\r\n",
        );
        fixture.write(EXTRA_OPTIONS, b"lang:zh_cn\nonboardAccessibility:false\n");
        let target = ModpackExportTarget::new(fixture.root.join("改过.mrpack"));
        fixture
            .petalfall
            .export_modpack(
                ID,
                &options(vec![ExportExtra::Config], false),
                &target,
                None,
            )
            .await
            .unwrap();

        let pack = target.destination();
        assert_eq!(
            read_entry(pack, &format!("overrides/{YOSBR}")),
            b"fov:0.8\nlang:ja_jp\n"
        );
        assert_eq!(
            read_entry(pack, &format!("overrides/{EXTRA_OPTIONS}")),
            b"lang:zh_cn\nonboardAccessibility:false\n"
        );
    }

    /// 不隔离的实例工作目录是游戏目录：补行与记账都落在游戏目录下，导出从同一处读，两边对得上。实例目录里不出现
    /// 记账。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_shared_working_dir_keeps_its_record_next_to_the_shared_config() {
        let tmp = tempfile::tempdir().unwrap();
        let petalfall = bare_petalfall(tmp.path());
        let dir = bare_instance(
            tmp.path(),
            "共用",
            r#"{"id":"共用","inheritsFrom":"1.21.1","type":"release","mainClass":"M"}"#,
        );
        VersionSettingsStore::for_version_dir(&dir)
            .save(&VersionSettings {
                isolation: IsolationOverride::Disabled,
                ..VersionSettings::default()
            })
            .await
            .unwrap();
        let mc = tmp.path().join("mc");
        std::fs::create_dir_all(mc.join("config/yosbr")).unwrap();
        std::fs::write(mc.join(YOSBR), b"fov:0.8\r\n").unwrap();

        let working_dir = fill_like_a_launch(&petalfall, "共用", tmp.path()).await;

        assert_eq!(working_dir, mc);
        assert!(crate::game_lang::filled_lines_path(&mc).is_file());
        assert!(!crate::game_lang::filled_lines_path(&dir).exists());
        let filled = b"fov:0.8\r\nlang:zh_cn\r\nonboardAccessibility:false\r\n";
        assert_eq!(std::fs::read(mc.join(YOSBR)).unwrap(), filled);

        let target = ModpackExportTarget::new(tmp.path().join("共用.mrpack"));
        petalfall
            .export_modpack(
                "共用",
                &options(vec![ExportExtra::Config], false),
                &target,
                None,
            )
            .await
            .unwrap();

        assert_eq!(
            entry_names(target.destination()),
            ["modrinth.index.json", "overrides/config/yosbr/options.txt"]
        );
        assert_eq!(
            read_entry(target.destination(), "overrides/config/yosbr/options.txt"),
            b"fov:0.8\r\n"
        );
        assert_eq!(std::fs::read(mc.join(YOSBR)).unwrap(), filled);
    }

    /// 记账读不懂时带配置的导出照实报错，不当成没记过（那样补的行就随包带走了）；不带配置的导出不读它，照常
    /// 导出。预览只报统计，不为它失败（导出弹窗要等预览成功才出表单）：配置按盘上的原样算，补过行的那份也是。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_unreadable_record_fails_only_exports_that_take_the_config() {
        let fixture = fixture().await;
        mount_modrinth(&fixture, &cdn("pc", "vc", "c.jar")).await;
        fixture.write(YOSBR, b"fov:0.8\nlang:zh_cn\nonboardAccessibility:false\n");
        let record = crate::game_lang::filled_lines_path(&fixture.dir);
        std::fs::write(&record, b"{not json").unwrap();

        let preview = fixture.petalfall.preview_modpack_export(ID).await.unwrap();
        let config = preview
            .extras
            .iter()
            .find(|stat| stat.extra == ExportExtra::Config)
            .unwrap();
        // sodium-options.json 13 字节、sub/deep.toml 11 字节；YOSBR 那份按盘上整份的 46 字节算，不扣补的两行。
        assert_eq!((config.files, config.bytes), (3, 13 + 11 + 46));

        let with_config = ModpackExportTarget::new(fixture.root.join("带配置.mrpack"));
        let err = fixture
            .petalfall
            .export_modpack(
                ID,
                &options(vec![ExportExtra::Config], false),
                &with_config,
                None,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&err, CoreError::JsonFileInvalid { path, .. } if path == &record),
            "{err:?}"
        );
        assert!(!with_config.destination().exists());

        let without = ModpackExportTarget::new(fixture.root.join("不带配置.mrpack"));
        fixture
            .petalfall
            .export_modpack(
                ID,
                &options(vec![ExportExtra::Options], false),
                &without,
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            read_entry(without.destination(), "overrides/options.txt"),
            b"lang:zh_cn"
        );
    }
}
