//! 整合包导入为新实例。
//!
//! 来源有两种：平台上某整合包的一个版本（先下载到缓存目录），或用户选的本地 `.mrpack` / `.zip`。
//! 导入总是新建一个实例目录 `versions/<实例名>/`，按顺序：
//!
//! 1. 读清单、去掉用户选择跳过的可选文件、校验实例名并查重、确认包里没有文件落在实例自己的版本文件上
//!    （`ensure_pack_spares_instance_files`）、确认原版与加载器要落的版本目录名下不是别的东西
//!    （`Petalfall::ensure_natural_versions_usable`）；CurseForge 包先把全部文件 id 换成下载
//!    地址——有作者禁止第三方分发、（设置打开时）拼出的地址也下不了、用户也还没手动下载认领的文件就在这
//!    一步整体报错，不先装几百 MB 游戏再告诉用户装不完。这一步不动游戏目录（[`Petalfall::prepare_modpack`]），调用方可以在这之后占住实例名再往下走。
//! 2. 核对游戏目录的数据格式记号，把原版与加载器要落的版本目录再查一遍（准备之后可能有实例被改名或移到
//!    那个名字下），原子地建目录（同名即报错），写下带任务 id 的未完成标记（[`Petalfall::import_modpack`]
//!    从这里开始）。
//! 3. 装整合包要求的原版与加载器（与「安装版本」同一条路径，落在它们自然的版本目录里）。这次新落下的目录
//!    记进依赖版本记录，不进实例库；目录原本就在的是玩家的，不记。
//! 4. 在实例目录写一份以实例名为 id 的版本 JSON：有加载器时是加载器 JSON 的副本，原版时只
//!    `inheritsFrom` 原版。启动按 `inheritsFrom` 找客户端 jar，实例目录里不需要 jar。
//! 5. 实例强制隔离：整合包的 Mod 与配置只该出现在这个实例里。
//! 6. 放入认领到的手动下载文件、下载远程文件、解出覆盖文件（覆盖文件在后，作者打进包里的配置压过
//!    下载来的默认值）。
//! 7. 写整合包信息，去掉未完成标记。
//!
//! 写下未完成标记之后任何一步失败或取消，导入自己删掉这次建出的实例目录；标记没写下、删不掉或导入的 future
//! 被丢弃时，由调用方经 [`Petalfall::discard_modpack_import`] 再清。名下的目录是不是这次建的只认
//! [`InstanceDirCreated`]，不看未完成标记；建目录时撞上已有目录的，什么都不删。
//! 原版与加载器的自然版本目录留着：它们是完整可用的版本，别的实例也可能在用。
//!
//! 进度按步骤报（`CoreEvent::Phase`）。开工时还没读到包（平台整合包要先下载），加载器、有没有要放的文件与覆盖
//! 文件都不知道，只能先按原版包预排一份表（`import_prelude_plan`）；读完包把还没报到的步骤改写成确切的表
//! （`import_plan`），已经报过的拿包、读包两步原样不动。准备与落盘分两步调时，报到哪随准备结果交给落盘一步
//! 接着报，不从头报。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use petalfall_download::{CredentialScope, DownloadCredential, DownloadTask};
use petalfall_install::GameLayout;
use petalfall_instance::{
    DiscoveredVersion, IncompleteKind, IncompleteMarker, IsolationOverride, MarkerStore,
    NameViolation, PETALFALL_META_DIR, VERSIONS_DIR, VersionSettings, VersionSettingsStore,
    check_name_format, discover_version,
};
use petalfall_modpack::{
    CurseForgeFileRef, ModrinthFile, PackArchive, PackFormat, PackLoader, PackLoaderKind,
    PackManifest, RemoteFiles, SafeRelativePath,
};
use petalfall_modplatform::{CurseForgeClient, CurseForgeFile, CurseForgeMod, Platform};
use petalfall_version::LoaderKind;
use serde::{Deserialize, Serialize};

use crate::blocked::{BlockedFile, CLAIMS_DIR, ClaimedCopy, PACK_CACHE_DIR, place_claimed};
use crate::data_format::{GameDirCheck, GuardMode};
use crate::error::{CoreError, Result};
use crate::event::{CoreEvent, EventSink, InstallPhase, emit};
use crate::facade::Petalfall;
use crate::install::LoaderChoice;
use crate::instance_install::{
    MarkerSettlement, PhaseAnnouncer, PhaseCursor, now_millis, template_stage,
};
use crate::resources::{
    curseforge_fallback_url, is_safe_file_name, join_landing, task_for_file_url,
};

/// 实例目录里的整合包信息文件名（在启动器元数据目录下）。
const MODPACK_INFO_FILE: &str = "modpack.json";
/// 整合包信息的格式版本；字段不兼容地变化时递增。
pub(crate) const MODPACK_INFO_SCHEMA: u32 = 1;
/// CurseForge 批量接口一次带多少个 id。接口文档没写上限，大整合包有四五百个文件，
/// 分批是为了让单个请求体与响应都保持在常规大小。
const CURSEFORGE_BATCH: usize = 200;
/// 平台整合包的下载缓存放多久还没删就算残留。
const STALE_PACK_CACHE_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

// CurseForge 的资源大类 id（与发现页分类表同一来源：官方分类接口的真实响应）。
const CF_CLASS_RESOURCE_PACKS: u32 = 12;
const CF_CLASS_SHADERS: u32 = 6552;
const CF_CLASS_DATA_PACKS: u32 = 6945;
const CF_CLASS_MODS: u32 = 6;

/// 整合包从哪来。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModpackSource {
    /// 平台上的一个整合包版本（Modrinth 版本 id / CurseForge 文件 id）。
    Platform {
        platform: Platform,
        project_id: String,
        version_id: String,
    },
    /// 本地 `.mrpack` / `.zip`。
    File { path: PathBuf },
}

/// 本地整合包文件的预览，供导入前确认。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModpackPreview {
    pub format: PackFormat,
    pub name: String,
    pub version: Option<String>,
    pub summary: Option<String>,
    pub author: Option<String>,
    pub minecraft: String,
    pub loader: Option<PackLoader>,
    /// 要下载的远程文件数。
    pub remote_files: usize,
    /// 导入后落进 `mods/` 的 Mod 数，与平台摘要同一口径（见 `count_pack_mods`）。
    pub mod_files: usize,
    /// 远程文件总字节数；平台没给齐每个文件的大小时为 `None`。
    pub remote_bytes: Option<u64>,
    /// 包内自带的覆盖文件数。
    pub override_files: usize,
    /// 覆盖文件总字节数。
    pub override_bytes: u64,
    /// 导入后落进 `mods/` 的每个文件，与 `mod_files` 同一口径（`mods.len() == mod_files`）：同一落点只留
    /// 一份、覆盖层胜出，按路径不分大小写排序。
    pub mods: Vec<PackModEntry>,
    /// 不直接落进 `mods/` 的可选远程文件（资源包、光影、配置等），排序同上。
    pub optional_extras: Vec<PackModEntry>,
    /// 作者不允许启动器直接下载、（设置打开时）拼出的 CDN 地址也下不了、暂存位里也还没有校验通过的副本的
    /// 文件：导入前要用户手动下载。
    pub blocked_files: Vec<BlockedFile>,
}

/// 整合包里的一个文件从哪来。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PackFileOrigin {
    /// 清单里的远程文件，导入时下载。
    Remote,
    /// 包里自带的覆盖文件。
    Override,
}

/// 整合包里的一个文件导入时怎么拿到。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PackFileAvailability {
    /// 启动器直接下载。
    Download,
    /// 包里自带。
    Bundled,
    /// 作者不允许启动器直接下载，要用户手动下载。
    Blocked,
    /// 受限文件，但已经从下载文件夹认领到了校验通过的副本。
    Claimed,
}

/// 预览里列出的一个文件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackModEntry {
    /// 相对实例目录的落点，正斜杠分隔。
    pub path: String,
    pub file_name: String,
    pub origin: PackFileOrigin,
    /// 可选文件（mrpack 里 `env.client` 为 `optional`），导入时可以跳过。
    pub optional: bool,
    /// CurseForge 远程文件的工程与文件 id；Modrinth 文件与覆盖文件为 `None`。
    pub project_id: Option<u32>,
    pub file_id: Option<u32>,
    pub availability: PackFileAvailability,
}

/// 导入时用户的选择。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportSelection {
    /// 跳过不装的可选文件，取清单里的落点写法。
    pub excluded_optional: Vec<String>,
}

/// 落点是否直接在 `mods/` 下（加载器只读这一层）。`key` 已转小写。
fn directly_in_mods(key: &str) -> bool {
    key.strip_prefix("mods/")
        .is_some_and(|name| !name.is_empty() && !name.contains('/'))
}

/// 整合包导入后落进 `mods/` 的 Mod 数，平台摘要与本地预览都用它：清单里落点直接在 `mods/` 下的远程
/// 文件（CurseForge 是工程大类为 Mod 的那些），加上覆盖层里直接在 `mods/` 下的 `.jar`。
///
/// 覆盖层的 `mods/` 里常带说明与配置，所以只认 `.jar`；子目录里的文件加载器不读，也不算。同一落点只算
/// 一次：覆盖文件会盖掉同名的下载文件，叠起来的几层覆盖也只留一份。比较不分大小写，与 Windows 上落盘
/// 的结果一致。`overrides` 给去掉覆盖层前缀之后、相对实例目录的路径。
pub(crate) fn count_pack_mods<'a>(
    remote: impl IntoIterator<Item = &'a str>,
    overrides: impl IntoIterator<Item = &'a str>,
) -> usize {
    let mut landed: HashSet<String> = HashSet::new();
    for path in remote {
        let key = path.to_lowercase();
        if directly_in_mods(&key) {
            landed.insert(key);
        }
    }
    for path in overrides {
        let key = path.to_lowercase();
        if directly_in_mods(&key) && key.ends_with(".jar") {
            landed.insert(key);
        }
    }
    landed.len()
}

/// 一次整合包导入的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModpackInstallOutcome {
    /// 新实例的版本 id（即实例名）。
    pub version_id: String,
    /// 新实例目录。
    pub version_dir: PathBuf,
    /// 写进实例的整合包信息。
    pub modpack: InstanceModpack,
}

/// 实例里记下的整合包信息（`<实例>/<元数据目录>/modpack.json`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceModpack {
    pub schema: u32,
    pub format: PackFormat,
    /// 来自平台时的平台与工程、版本；来自本地文件时都为 `None`。
    pub platform: Option<Platform>,
    pub project_id: Option<String>,
    pub version_id: Option<String>,
    /// 整合包自己的名字与版本（实例名可以被用户改成别的）。
    pub name: String,
    pub version: Option<String>,
    pub minecraft: String,
    pub loader: Option<PackLoader>,
    pub remote_files: usize,
    pub override_files: usize,
    /// 导入完成的时刻（Unix 秒）。
    pub installed_at: u64,
    /// 作者写的推荐内存（MiB），自动内存按它估算。早于这一项的记录与没写推荐值的包都没有，所以缺省为 `None`；
    /// 是 `None` 时不写出，旧版落樱读新记录不受影响。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommended_ram_mb: Option<u32>,
}

/// 校验实例名的写法（不查重），返回去掉首尾空白后的名字。规则与新建、改名是同一套。
pub fn validate_instance_name(raw: &str) -> std::result::Result<String, NameViolation> {
    let name = raw.trim();
    check_name_format(name)?;
    Ok(name.to_owned())
}

/// 读实例里的整合包信息；不是从整合包导入的实例返回 `None`。
pub async fn read_instance_modpack(version_dir: &Path) -> Result<Option<InstanceModpack>> {
    let path = version_dir.join(PETALFALL_META_DIR).join(MODPACK_INFO_FILE);
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(petalfall_base::Error::Io { path, source }.into()),
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|source| CoreError::JsonFileInvalid { path, source })
}

/// 一个要下载的远程文件；Modrinth 清单可以给多个地址，前一个失败换下一个。
#[derive(Debug)]
struct RemoteDownload {
    task: DownloadTask,
    fallbacks: Vec<String>,
}

/// 准备好、还没落盘的一次整合包导入：包已在本地、清单已读、实例名已定并查过重，游戏目录还没动。
///
/// 导入拆成准备与落盘两步，是为了让调用方在实例名定下之后、建目录之前占住这个名字：平台整合包
/// 要先下载下来才读得到清单里的名字，而目录一旦建出来，启动、改名、删除就都能看见它。
#[derive(Debug)]
pub struct PreparedModpack {
    source: ModpackSource,
    name: String,
    pack_path: PathBuf,
    archive: PackArchive,
    remote: Vec<RemoteDownload>,
    /// 认领到暂存位的受限文件：导入时复制进实例，不下载。
    claimed: Vec<ClaimedCopy>,
    /// 平台整合包下载到的缓存目录，只属于这一次导入；本地文件为 `None`。
    cache_dir: Option<PathBuf>,
    /// 步骤报到了哪，表已改写成确切的：落盘一步接着报，前端看到的是同一个任务的下一步。
    phases: PhaseCursor,
    /// 落盘一步建出实例目录时置位，见 [`InstanceDirCreated`]。
    dir_created: InstanceDirCreated,
}

/// 这次导入有没有建出实例目录。准备结果交给落盘一步之前先取一份（[`PreparedModpack::dir_created`]）。
///
/// 导入失败或被放弃之后，调用方要按实例名清掉半成品（[`Petalfall::discard_modpack_import`]）。名字查过重、
/// 占住了，也不等于名下的目录就是这次建的：Windows 上同一个目录可以有不止一个名字（生成 8.3 短名的卷上，
/// `ALLTHE~1` 指向「All the Mods 10」；文件系统把「Σ」与「σ」折叠成同一个字母，查重按整串小写比却认不出
/// 词尾的那一个），按目录列表查重看不出来，建目录时才撞上；查重与建目录之间，别的程序也可能抢先建出同名
/// 目录。所以清理只认这个记号：建目录那一步成功才置位，没置位时名下的目录是别人的，一个字节都不能动。
///
/// 置了位也不等于名下的目录永远是这次的：导入失败时自己会把建出来的目录删掉，删成了就再记一笔。那之后名下
/// 再出现的目录是别人的（别的命令把实例改名或移到了这个名字的另一种写法下，或别的程序建的），同样不能动。
#[derive(Debug, Clone, Default)]
pub struct InstanceDirCreated(Arc<AtomicU8>);

/// 建出来了，还没有被导入自己删掉：名下的目录是这次导入的半成品。
const DIR_CREATED: u8 = 1;
/// 建出来过，导入自己的失败路径已经把它删掉。
const DIR_REMOVED_BY_IMPORT: u8 = 2;

impl InstanceDirCreated {
    /// 实例目录是不是这次导入建出来的（后来有没有被导入自己删掉不论）。
    pub fn get(&self) -> bool {
        self.0.load(Ordering::SeqCst) != 0
    }

    fn set(&self) {
        self.0.store(DIR_CREATED, Ordering::SeqCst);
    }

    fn set_removed_by_import(&self) {
        self.0.store(DIR_REMOVED_BY_IMPORT, Ordering::SeqCst);
    }

    fn removed_by_import(&self) -> bool {
        self.0.load(Ordering::SeqCst) == DIR_REMOVED_BY_IMPORT
    }
}

/// [`Petalfall::discard_modpack_import`] 清理的结果。前两种都说明这次导入建出过实例目录、它的半成品现在已经
/// 不在，调用方接着清配置里对这个名字的引用；第三种什么都没动，引用也不该动。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscardedImport {
    /// 这次导入建的实例目录还留着（导入的 future 被丢弃了，或导入自己没删掉），已经删掉。
    Removed,
    /// 这次导入建过实例目录，它已经不在：导入自己的失败路径删掉了（这时不再看盘，名下后来出现的目录不是
    /// 这次的），或来清的时候名下已经没有目录。
    AlreadyGone,
    /// 这次导入没建出实例目录：名下有没有目录都与它无关，没有查也没有动。
    NotCreated,
}

/// 清单里的远程文件准备成的样子：要下载的，与认领到暂存位、直接复制的。
#[derive(Debug, Default)]
struct RemotePlan {
    downloads: Vec<RemoteDownload>,
    claimed: Vec<ClaimedCopy>,
}

impl RemotePlan {
    /// 导入时有没有文件要放进实例：排步骤表与报这一步看的是同一个条件，两处不能各算各的。
    fn places_files(&self) -> bool {
        !self.downloads.is_empty() || !self.claimed.is_empty()
    }
}

impl PreparedModpack {
    /// 导入后的实例名（也是版本 id）。
    pub fn name(&self) -> &str {
        &self.name
    }

    /// 导入要写的自然版本目录：先是导入出的实例将继承的原版（客户端 jar 与 natives 所在），有加载器时再加
    /// 加载器版本。实例 JSON 是加载器 JSON 的副本，它的 `inheritsFrom` 就是这个原版，写出来之前要读加载器目录。
    /// 调用方据此在导入期间占住它们，不必等实例 JSON 写出来再扫。
    pub fn natural_versions(&self) -> Vec<String> {
        natural_versions(&self.archive.manifest)
    }

    /// 落盘一步有没有建出实例目录的记号。导入的 future 可能被丢弃，调用方要在交出准备结果之前取走这一份，
    /// 之后才知道名下的目录该不该由自己清。
    pub fn dir_created(&self) -> InstanceDirCreated {
        self.dir_created.clone()
    }

    /// 不导入了：删掉平台整合包的下载缓存，本地文件不动。
    pub async fn discard(self) {
        remove_pack_cache(self.cache_dir.as_deref()).await;
    }
}

fn natural_versions(manifest: &PackManifest) -> Vec<String> {
    let mut versions = vec![manifest.minecraft.clone()];
    if let Some(loader) = &manifest.loader {
        versions.push(loader_choice(loader.kind).natural_version_id(
            &manifest.minecraft,
            strip_minecraft_prefix(&loader.version, &manifest.minecraft),
        ));
    }
    versions
}

/// 拿到包与读包两步。读包之前就要报，预排的表与确切的表在这一段必须相同，两张表都从这里起头。
fn pack_phases(source: &ModpackSource) -> Vec<InstallPhase> {
    let mut plan = Vec::new();
    if matches!(source, ModpackSource::Platform { .. }) {
        plan.push(InstallPhase::ModpackDownload);
    }
    plan.push(InstallPhase::ModpackRead);
    plan
}

/// 开工时预排的步骤表：包还没读，加载器不知道，先按原版包排，放文件与解覆盖文件都算上。读完包由
/// [`import_plan`] 改写成确切的。
fn import_prelude_plan(source: &ModpackSource) -> Vec<InstallPhase> {
    use InstallPhase::*;
    let mut plan = pack_phases(source);
    plan.extend([
        Metadata,
        Libraries,
        Assets,
        Natives,
        ModpackFiles,
        ModpackOverrides,
    ]);
    plan
}

/// 读完包之后确切的步骤表。装游戏走的是 [`Petalfall::install_reporting`]，原版与加载器的几步照它实际报的顺序
/// 排：原版四步在前，加载器各步在后（Fabric / Quilt 是 LoaderProfile、LoaderLibraries，Forge / NeoForge 是
/// Java、LoaderInstaller、LoaderLibraries、LoaderProcessors），没有 Verify。新建实例的表（`install_plan`）走的是
/// 另一条路，不能照搬：那里 Fabric / Quilt 的 LoaderProfile 排在 Libraries 之前、没有 LoaderLibraries，
/// Forge / NeoForge 末尾还有 Verify。没有文件要放、包里没有覆盖文件、配置没开默认设置时，对应的一步不排，
/// 免得前端画出一步永远走不到的步骤。
fn import_plan(
    source: &ModpackSource,
    loader: Option<PackLoaderKind>,
    places_files: bool,
    has_overrides: bool,
    template: bool,
) -> Vec<InstallPhase> {
    use InstallPhase::*;
    let mut plan = pack_phases(source);
    plan.extend([Metadata, Libraries, Assets, Natives]);
    match loader {
        None => {}
        Some(PackLoaderKind::Fabric | PackLoaderKind::Quilt) => {
            plan.extend([LoaderProfile, LoaderLibraries]);
        }
        Some(PackLoaderKind::Forge | PackLoaderKind::NeoForge) => {
            plan.extend([Java, LoaderInstaller, LoaderLibraries, LoaderProcessors]);
        }
    }
    if places_files {
        plan.push(ModpackFiles);
    }
    if has_overrides {
        plan.push(ModpackOverrides);
    }
    if template {
        plan.push(Template);
    }
    plan
}

/// 下载来的包只是导入的中间产物；删不掉不影响任何实例，留日志即可。下载在建目录之前就失败时目录不存在。
async fn remove_pack_cache(dir: Option<&Path>) {
    let Some(dir) = dir else {
        return;
    };
    if let Err(err) = tokio::fs::remove_dir_all(dir).await
        && err.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(dir = %dir.display(), error = %err, "整合包下载缓存未能删除");
    }
}

/// 清掉下载缓存根下的残留，认领暂存位不动。
///
/// 导入结束后缓存就删了；进程被杀、或取消宽限期满丢弃了导入，目录会留下来，而它只属于那一次导入，不会再有
/// 人接手。比 [`STALE_PACK_CACHE_AGE`] 老的才算残留：再慢的导入也用不了这么久，不会删到别的导入手里的包。
/// 与删缓存同理，清不掉不影响导入，留日志即可。
async fn sweep_stale_pack_caches(root: &Path, now: SystemTime) {
    let mut entries = match tokio::fs::read_dir(root).await {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
        Err(err) => {
            tracing::warn!(dir = %root.display(), error = %err, "读不了整合包下载缓存目录，这次不清残留");
            return;
        }
    };
    loop {
        let entry = match entries.next_entry().await {
            Ok(Some(entry)) => entry,
            Ok(None) => return,
            Err(err) => {
                tracing::warn!(dir = %root.display(), error = %err, "读不了整合包下载缓存目录，这次不清残留");
                return;
            }
        };
        if entry.file_name() == CLAIMS_DIR {
            continue;
        }
        let dir = entry.path();
        let stale = match entry.metadata().await.and_then(|meta| meta.modified()) {
            Ok(modified) => now
                .duration_since(modified)
                .is_ok_and(|age| age > STALE_PACK_CACHE_AGE),
            Err(err) => {
                tracing::warn!(dir = %dir.display(), error = %err, "读不了残留缓存的修改时间，留着");
                false
            }
        };
        if stale {
            remove_pack_cache(Some(&dir)).await;
        }
    }
}

impl Petalfall {
    /// 预览本地整合包文件：只读清单与条目表，不解压。CurseForge 清单有远程文件时要问两个批量接口才
    /// 数得出 Mod、算得出下载量（导入同样要问），没配 API key 或断网时与导入报同样的错。
    ///
    /// CurseForge 包里没给下载地址的文件逐个问 download-url 端点，设置 `download_restricted_curseforge_files`
    /// 打开时端点也不给的再拼 CDN 地址探测（能下的导入时直接下载，不列），都拿不到地址的列进
    /// `blocked_files`，暂存位里已有校验通过副本的记为已认领，落点被覆盖层盖住的不用下载、也不列。受限文件
    /// 不让预览失败：用户要在这一步就看到要手动下载哪些；探测本身出错（网络不通、CDN 回 5xx）照常报错。
    pub async fn preview_modpack_file(&self, path: &Path) -> Result<ModpackPreview> {
        let archive = read_pack_blocking(path.to_path_buf()).await?;
        let remote = self
            .remote_facts(&archive.manifest.remote, BlockedProbe::Probe)
            .await?;
        let covered = override_landings(&archive);
        let mut claimed = HashSet::new();
        let mut blocked = Vec::new();
        for item in remote.entries.iter().filter_map(|e| e.blocked.as_ref()) {
            if covered.contains(&item.destination.to_lowercase()) {
                continue;
            }
            if self.claimed_copy(item).await?.is_some() {
                claimed.insert(item.file_id);
            } else {
                blocked.push(item.clone());
            }
        }
        Ok(preview_of(&archive, remote, &claimed, blocked))
    }

    /// 把整合包导入为新实例：[`Petalfall::prepare_modpack`] 之后紧接 [`Petalfall::import_modpack`]。
    /// 需要在建目录前占住实例名的调用方分开调用这两步。
    ///
    /// 动手之前先以「使用」方式核对当前游戏目录的数据格式记号：目录被更新的落樱用过、记号读不懂时报错，平台整合包
    /// 也不下载。
    pub async fn install_modpack(
        &self,
        source: &ModpackSource,
        name: Option<&str>,
        selection: &ImportSelection,
        task_id: &str,
        events: Option<&EventSink>,
    ) -> Result<ModpackInstallOutcome> {
        self.guard_game_dir(GuardMode::Use).await?;
        let prepared = self
            .prepare_modpack(source, name, selection, events)
            .await?;
        self.import_modpack(prepared, task_id, events).await
    }

    /// 导入的第一步：拿到整合包（平台整合包先下载到缓存目录）、读清单、去掉 `selection` 里跳过的可选
    /// 文件、定下实例名并查重，把远程文件换成下载任务（CurseForge 禁止分发、落点又没被覆盖层盖住的文件在
    /// 设置打开时先拼 CDN 地址探测，能下就直接下载；还拿不到、又没认领到副本的文件在这里整体报错，认领到的
    /// 改为从暂存位复制）。
    /// `name` 缺省用整合包自己的名字。
    ///
    /// 步骤报到读包为止；读完包把步骤表改写成确切的，报到哪随准备结果交给落盘一步接着报。
    ///
    /// 不动游戏目录。之后交给 [`Petalfall::import_modpack`]，不导入了就 [`PreparedModpack::discard`]。
    pub async fn prepare_modpack(
        &self,
        source: &ModpackSource,
        name: Option<&str>,
        selection: &ImportSelection,
        events: Option<&EventSink>,
    ) -> Result<PreparedModpack> {
        let announcer = PhaseAnnouncer::new(events, import_prelude_plan(source));
        let (pack_path, cache_dir) = match source {
            ModpackSource::File { path } => (path.clone(), None),
            ModpackSource::Platform {
                platform,
                project_id,
                version_id,
            } => {
                // 用认领到的手动下载副本时这一步也照报：表是读包之前排的，这一步瞬间就过去。
                announcer.enter(InstallPhase::ModpackDownload);
                self.download_platform_pack(*platform, project_id, version_id, events)
                    .await?
            }
        };
        announcer.enter(InstallPhase::ModpackRead);
        match self
            .read_prepared(source, name, selection, &pack_path, events)
            .await
        {
            Ok((name, archive, remote)) => {
                let phases = announcer.revise(import_plan(
                    source,
                    archive.manifest.loader.as_ref().map(|loader| loader.kind),
                    remote.places_files(),
                    !archive.overrides.is_empty(),
                    self.config().new_instance_template,
                ));
                Ok(PreparedModpack {
                    source: source.clone(),
                    name,
                    pack_path,
                    archive,
                    remote: remote.downloads,
                    claimed: remote.claimed,
                    cache_dir,
                    phases,
                    dir_created: InstanceDirCreated::default(),
                })
            }
            Err(err) => {
                remove_pack_cache(cache_dir.as_deref()).await;
                Err(err)
            }
        }
    }

    /// 导入的第二步：建实例目录、写下带 `task_id` 的未完成标记，装游戏与加载器、下载远程文件、
    /// 解出覆盖文件、写整合包信息。写下标记之后任何一步失败都删掉这个实例目录；目录建成了、标记却没写下，
    /// 任务被强行放弃（future 被丢弃），或这里删不掉时，目录还留着，由调用方经
    /// [`Petalfall::discard_modpack_import`] 再清一次。平台整合包的下载缓存不论成败都删掉。步骤接着准备一步
    /// 报到的地方往下报。
    ///
    /// 实例目录由这一步原子地建出来，名下已经有目录就报重名、什么都不动；建成时置位准备结果带的
    /// [`InstanceDirCreated`]，调用方凭它判断名下的目录是不是这次建的。
    ///
    /// 动手之前先以「使用」方式核对当前游戏目录的数据格式记号：目录被更新的落樱用过、记号读不懂时报错，不下载、
    /// 不建实例目录。游戏目录原本不在时，建好 `versions/` 就补写记号，之后才建实例目录、落任何数据。
    pub async fn import_modpack(
        &self,
        prepared: PreparedModpack,
        task_id: &str,
        events: Option<&EventSink>,
    ) -> Result<ModpackInstallOutcome> {
        let PreparedModpack {
            source,
            name,
            pack_path,
            archive,
            remote,
            claimed,
            cache_dir,
            phases,
            dir_created,
        } = prepared;
        let announcer = PhaseAnnouncer::resume(events, phases);
        let outcome = self
            .import_into_new_dir(
                &archive,
                &pack_path,
                &source,
                name,
                RemotePlan {
                    downloads: remote,
                    claimed,
                },
                &dir_created,
                task_id,
                &announcer,
                events,
            )
            .await;
        remove_pack_cache(cache_dir.as_deref()).await;
        outcome
    }

    /// 删掉一次没成的导入留下的实例目录 `versions/<name>`，结果分三种，见 [`DiscardedImport`]。
    ///
    /// 目录是不是这次导入建的只认 `created`（[`PreparedModpack::dir_created`]）：没置位就立刻返回
    /// [`DiscardedImport::NotCreated`]，游戏目录不核对、名字不校验、盘上什么都不看。调用方占住了名字、查过重也
    /// 不够，见 [`InstanceDirCreated`]。导入自己已经把目录删掉的，同样立刻返回
    /// [`DiscardedImport::AlreadyGone`]、不看盘：从它删掉到这里之间，名下再出现的目录不是这次的，而占住名字的
    /// 守卫按名字的字面比，挡不住名字的另一种写法。不看未完成标记：标记可能还没来得及写下，也可能读不出来，
    /// 按标记找会漏掉这两种半成品。
    ///
    /// 只处理文件，配置里对这个名字的引用不在这里清：导入跑在门面快照上，这里改了配置也到不了门面。半成品目录在的
    /// 时候侧栏固定的校验认这个名字，配置里也可能本来就留着同名的旧固定，所以返回 [`DiscardedImport::Removed`]
    /// 或 [`DiscardedImport::AlreadyGone`] 之后，调用方要趁还占着这个名字，在门面上经
    /// [`Petalfall::forget_moved_instance`] 按导入所在的游戏目录清掉（导入途中当前目录可能已经切走），否则之后
    /// 再建或再导入同名实例时它一出现就是已固定。返回 [`DiscardedImport::NotCreated`] 时名下的目录与这些引用都是
    /// 别人的，不清；返回错误时半成品还在、仍列在实例库里，引用也不清。
    ///
    /// 记号停在「建了」时，动手之前先以「使用」方式核对当前游戏目录的数据格式记号：目录被更新的落樱用过、
    /// 记号读不懂时报错，什么都不删。
    pub async fn discard_modpack_import(
        &self,
        name: &str,
        created: &InstanceDirCreated,
    ) -> Result<DiscardedImport> {
        // 放在最前面：没置位时连报错都不该有，调用方拿到错误会去改写名下那个目录的未完成标记。
        if !created.get() {
            return Ok(DiscardedImport::NotCreated);
        }
        if created.removed_by_import() {
            return Ok(DiscardedImport::AlreadyGone);
        }
        self.guard_game_dir(GuardMode::Use).await?;
        let dir = self.instance_dir(name)?;
        match tokio::fs::metadata(&dir).await {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => return Ok(DiscardedImport::AlreadyGone),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(DiscardedImport::AlreadyGone);
            }
            Err(source) => return Err(petalfall_base::Error::Io { path: dir, source }.into()),
        }
        remove_instance_dir(&dir).await?;
        Ok(DiscardedImport::Removed)
    }

    /// 半成品删不掉时（文件被占用）把它的未完成标记落成这次导入的结局：否则标记一直停在进行中，
    /// 列表里看不出导入其实已经结束。保留原标记的开始时刻；原标记读不出来时取此刻，新标记覆盖它。
    ///
    /// 动手之前先以「使用」方式核对当前游戏目录的数据格式记号：目录被更新的落樱用过、记号读不懂时报错，标记不动。
    pub async fn settle_modpack_import(
        &self,
        name: &str,
        task_id: &str,
        settlement: MarkerSettlement,
    ) -> Result<()> {
        self.guard_game_dir(GuardMode::Use).await?;
        let dir = self.instance_dir(name)?;
        let started_at = match MarkerStore::for_version_dir(&dir).load().await {
            Ok(Some(marker)) => marker.started_at,
            Ok(None) => now_millis(),
            Err(err) => {
                tracing::warn!(dir = %dir.display(), error = %err, "导入半成品的未完成标记读不出来，改写一份新的");
                now_millis()
            }
        };
        let marker = IncompleteMarker::pending(
            IncompleteKind::Modpack {
                task_id: task_id.to_owned(),
            },
            started_at,
        );
        self.settle_instance_marker(name, marker, settlement).await
    }

    /// 读清单、去掉跳过的可选文件、定实例名并查重、把远程文件换成下载任务。
    async fn read_prepared(
        &self,
        source: &ModpackSource,
        name: Option<&str>,
        selection: &ImportSelection,
        pack_path: &Path,
        events: Option<&EventSink>,
    ) -> Result<(String, PackArchive, RemotePlan)> {
        let mut archive = read_pack_blocking(pack_path.to_path_buf()).await?;
        let skipped = archive
            .manifest
            .remote
            .exclude_optional(&selection.excluded_optional)?;
        let manifest = &archive.manifest;
        let requested = name.unwrap_or(&manifest.name);
        let name = validate_instance_name(requested).map_err(|rule| name_error(requested, rule))?;
        if name.eq_ignore_ascii_case(&manifest.minecraft) {
            return Err(name_error(&name, NameViolation::Duplicate));
        }
        self.ensure_modpack_name_free(&name).await?;
        ensure_pack_spares_instance_files(&self.layout(), &name, &archive)?;
        self.ensure_natural_versions_usable(manifest).await?;
        let version_dir = self.game_dir().join(VERSIONS_DIR).join(&name);

        emit(
            events,
            CoreEvent::stage(format!(
                "读取整合包 {}：{} 个远程文件，{} 个覆盖文件",
                manifest.name,
                manifest.remote.len(),
                archive.overrides.len()
            )),
        );
        let remote = self
            .remote_downloads(&manifest.remote, &override_landings(&archive), &version_dir)
            .await?;
        tracing::debug!(%name, ?source, remote = remote.downloads.len(), claimed = remote.claimed.len(), skipped, "整合包导入已准备好");
        Ok((name, archive, remote))
    }

    /// 整合包要用的原版与加载器装在各自的自然版本目录里（[`natural_versions`]）。那个名字下已经有一个别的版本时
    /// 不能导入：安装会把它的版本 JSON 换成原版或那个加载器的。玩家给一个装了加载器的实例起了游戏版本号当名字
    /// （别的启动器里这是常见的默认名），导入一个同游戏版本的整合包，那个实例的加载器就没了、Mod 不再加载。
    ///
    /// 只在认得出它是别的东西时才拦（[`occupied_by`]）。不拦的还有两种：目录不在、缺版本 JSON 或 JSON 读不懂
    /// 的，那里没有认得出的版本可丢，安装会补上；启动器自己落下、玩家没拿来当实例用的依赖版本目录。后一种在
    /// 加载器的位置上，导入出的实例各有一份版本 JSON 的副本，重装它不影响它们（1.20.1 上 Forge 与 NeoForge 的
    /// 版本号撞上时，两个整合包就共用这样一个目录名）；可要是有别的版本直接继承着它（别的启动器建的叠加版本，
    /// 或移动实例时连基底一起带过来的），换掉它就换了那个版本的底，照样拦。在原版的位置上，导入出的实例本来就
    /// 直接继承它，重装等于把它还原成这些实例要的那个原版，不看有没有人继承。
    ///
    /// 查两遍。准备阶段一遍：这时还没占名字、没建目录，平台整合包虽然已经下载，游戏与加载器都还没装，早一步
    /// 报出来。落盘一步建实例目录之前再一遍：准备阶段查过之后还要解析、探测远程文件，调用方再去占名字，这期间
    /// 别的命令可能把一个实例改名或移到这个名字下；到了落盘，调用方已经占住这几个名字，改名、新建、删除这些
    /// 命令进不来。
    async fn ensure_natural_versions_usable(&self, manifest: &PackManifest) -> Result<()> {
        let wanted = std::iter::once(None).chain(manifest.loader.as_ref().map(Some));
        for (id, loader) in natural_versions(manifest).iter().zip(wanted) {
            let Some(found) = discover_version(self.game_dir(), id).await? else {
                continue;
            };
            let Some(found) = occupied_by(&found, loader) else {
                continue;
            };
            let unclaimed = crate::instances::is_unclaimed_base_version(self.game_dir(), id).await;
            if unclaimed && loader.is_none() {
                continue;
            }
            // 拒收的报文要点出继承着它的版本：有人继承时改名会被拒，「改个名字」的出路走不通。
            let inherited_by =
                crate::instances::base_version_dependents(self.game_dir(), id).await?;
            if unclaimed && inherited_by.is_empty() {
                continue;
            }
            return Err(CoreError::BaseVersionOccupied {
                id: id.clone(),
                wanted: match loader {
                    None => format!("原版 {}", manifest.minecraft),
                    Some(loader) => format!(
                        "加载器 {} {}",
                        detected_kind(loader.kind).as_str(),
                        strip_minecraft_prefix(&loader.version, &manifest.minecraft)
                    ),
                },
                found,
                inherited_by,
                claimed: !unclaimed,
            });
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn import_into_new_dir(
        &self,
        archive: &PackArchive,
        pack_path: &Path,
        source: &ModpackSource,
        name: String,
        remote: RemotePlan,
        created: &InstanceDirCreated,
        task_id: &str,
        announcer: &PhaseAnnouncer,
        events: Option<&EventSink>,
    ) -> Result<ModpackInstallOutcome> {
        let checked = self.guard_game_dir(GuardMode::Use).await?;
        let versions = self.game_dir().join(VERSIONS_DIR);
        tokio::fs::create_dir_all(&versions)
            .await
            .map_err(|source| petalfall_base::Error::Io {
                path: versions.clone(),
                source,
            })?;
        // 游戏目录是上一步才建出来的：开头核对时它还不在，记号没处写，现在补上。放在建实例目录之前，进程中途被杀
        // 或后面哪一步失败时，留下的依赖版本记录与版本目录也落在一个有记号的目录里。
        if checked == GameDirCheck::Absent {
            self.guard_game_dir(GuardMode::Use).await?;
        }
        self.ensure_natural_versions_usable(&archive.manifest)
            .await?;
        let version_dir = versions.join(&name);
        create_instance_dir(&version_dir, &name, created)?;
        // 名字下还留着的记录属于在启动器之外被删掉的同名旧实例，与新建实例同一处理：放在目录建成之后，
        // 撞名时那份记录是现有实例的；清不掉不拦导入，改动已记下待补，在这个实例的第一局记下之前一定先补上。
        if let Err(err) = self.forget_play_records(&name).await {
            tracing::warn!(%name, error = %err, "清掉同名旧实例的游玩记录失败，已记下待补");
        }
        write_marker(&version_dir, task_id).await?;
        let populated = self
            .populate_instance(
                archive,
                pack_path,
                source,
                &name,
                &version_dir,
                remote,
                announcer,
                events,
            )
            .await;
        match populated {
            Ok(modpack) => Ok(ModpackInstallOutcome {
                version_id: name,
                version_dir,
                modpack,
            }),
            Err(err) => {
                match remove_instance_dir(&version_dir).await {
                    // 删成了就当场记下，与上一行之间没有让出点：调用方凭它知道不必、也不该再按名字删一次。
                    Ok(()) => created.set_removed_by_import(),
                    Err(cleanup) => {
                        tracing::error!(dir = %version_dir.display(), error = %cleanup, "导入失败后未能删除半成品实例");
                    }
                }
                Err(err)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn populate_instance(
        &self,
        archive: &PackArchive,
        pack_path: &Path,
        source: &ModpackSource,
        name: &str,
        version_dir: &Path,
        remote: RemotePlan,
        announcer: &PhaseAnnouncer,
        events: Option<&EventSink>,
    ) -> Result<InstanceModpack> {
        let manifest = &archive.manifest;
        let loader = manifest.loader.as_ref();
        let loader_version =
            loader.map(|l| strip_minecraft_prefix(&l.version, &manifest.minecraft));
        // 在安装之前看目录在不在：装完就分不出哪些是这次落下的。先记后装，下载途中的目录不露出来，装到一半被杀、
        // 导入被放弃时也没有人再来补记。目录不分大小写。
        let present: HashSet<String> = self
            .instance_names()
            .await?
            .iter()
            .map(|id| id.to_lowercase())
            .collect();
        let is_new = |id: &String| !present.contains(&id.to_lowercase());
        let landing: Vec<String> = natural_versions(manifest)
            .into_iter()
            .filter(is_new)
            .collect();
        self.record_base_versions(&landing).await?;
        let report_phase = |phase| announcer.enter(phase);
        let installed = self
            .install_reporting(
                &manifest.minecraft,
                loader.map(|l| loader_choice(l.kind)),
                loader_version,
                events,
                Some(&report_phase),
            )
            .await;
        // 成败都核对一遍：装失败时后面的目录没落下；早期 Forge 的 legacy 安装器不一定按算出的 id 落目录。
        if let Err(err) = self.forget_unlanded_base_versions(&landing).await {
            tracing::warn!(error = %err, "未能收回没落下的依赖版本记录");
        }
        let installed = installed?;
        // 以安装结果为准：算出的 id 可能不对，并行的导入失败时也可能收回了同一个 id。
        let landed: Vec<String> = std::iter::once(&installed.vanilla.id)
            .chain(installed.loader.as_ref().map(|loader| &loader.id))
            .filter(|id| is_new(id))
            .cloned()
            .collect();
        self.record_base_versions(&landed).await?;
        let natural = installed
            .loader
            .as_ref()
            .map_or(installed.vanilla.id.as_str(), |l| l.id.as_str());
        // 原版或加载器的自然版本 id 与实例同名时，安装已经把它写进了实例目录，两者没法共存，按重名
        // 处理。失败清理会连它一起删掉：同名目录在建实例前不存在，一定是这次装出来的。
        if natural.eq_ignore_ascii_case(name) || installed.vanilla.id.eq_ignore_ascii_case(name) {
            return Err(name_error(name, NameViolation::Duplicate));
        }
        self.materialize_version_json(
            name,
            version_dir,
            &installed.vanilla.id,
            installed.loader.is_some().then_some(natural),
        )
        .await?;

        VersionSettingsStore::for_version_dir(version_dir)
            .save(&VersionSettings {
                isolation: IsolationOverride::Enabled,
                ..VersionSettings::default()
            })
            .await?;
        // 版本目录里有 mods/ 时启动链路无论全局档位如何都按隔离处理，这一步让导入的实例
        // 在任何隔离设置下都只用自己目录里的 Mod。
        let mods_dir = version_dir.join("mods");
        tokio::fs::create_dir_all(&mods_dir)
            .await
            .map_err(|source| petalfall_base::Error::Io {
                path: mods_dir,
                source,
            })?;

        if remote.places_files() {
            announcer.enter(InstallPhase::ModpackFiles);
        }
        if !remote.claimed.is_empty() {
            emit(
                events,
                CoreEvent::stage(format!("放入 {} 个手动下载的文件", remote.claimed.len())),
            );
            place_claimed(&remote.claimed).await?;
        }
        let downloads = remote.downloads.len();
        if downloads > 0 {
            emit(
                events,
                CoreEvent::stage(format!("下载 {downloads} 个整合包文件")),
            );
            self.download_remote(remote.downloads, events).await?;
        }
        // 跳过的可选文件已从清单里去掉，记的是这个实例实际装进来的远程文件数（含手动下载的）。
        let remote_files = manifest.remote.len();

        let override_files = archive.overrides.len();
        if override_files > 0 {
            announcer.enter(InstallPhase::ModpackOverrides);
            emit(
                events,
                CoreEvent::stage(format!("解出 {override_files} 个覆盖文件")),
            );
            let pack = pack_path.to_path_buf();
            let entries = archive.overrides.clone();
            let dest = version_dir.to_path_buf();
            let cancel = self.cancel_token().cloned();
            tokio::task::spawn_blocking(move || {
                let is_cancelled = || cancel.as_ref().is_some_and(|t| t.is_cancelled());
                petalfall_modpack::extract_overrides(&pack, &entries, &dest, &is_cancelled)
            })
            .await??;
        }

        // 包里写着游戏默认语言的那一行不算作者的选择：去掉之后，启动前的补全才会按系统语言来（见
        // game_lang）。放在套默认设置之前：模板里的语言是玩家自己存下的，不在此列。
        if self.config().follow_system_game_language {
            let changed = crate::game_lang::drop_pack_default_language(version_dir).await?;
            if !changed.is_empty() {
                tracing::info!(name, files = ?changed, "已去掉整合包自带的默认语言行");
            }
        }
        // 空的 options.txt（本来就空，或上一步去掉默认语言行后变空）不是谁的设置，留着却会挡住模组的默认
        // 设置与下面的套默认设置，还让游戏把首次无障碍引导标成走过。不看语言开关。
        if crate::game_lang::remove_blank_options_file(version_dir).await? {
            tracing::info!(name, "整合包带来的 options.txt 是空的，已删掉");
        }

        // 覆盖文件之后才补：包里自带的 options.txt 是作者调好的，按只补缺的规则原样留着，缺的服务器列表
        // 之类由默认设置补上。导入的实例一律隔离，套不套只看配置这一道闸。失败与其他步骤一样让导入失败。
        // 报不报这一步只看准备时排定的表、不看配置：准备与落盘是两次调用，调用方可以在两次之间改配置或换一个门面，
        // 再读一遍配置会报出表外的步骤。表里有而配置已关时照报这一步，套默认设置自己按配置跳过。
        if announcer.plans(InstallPhase::Template) {
            announcer.enter(InstallPhase::Template);
        }
        let template = self.apply_template(name).await?;
        // 留给模组的也要说：模板的文件一个都没补进来时，不说玩家就看不出默认设置为什么没生效。
        if let crate::template::TemplateOutcome::Applied {
            copied,
            left_to_mods,
            ..
        } = &template
            && (!copied.is_empty() || !left_to_mods.is_empty())
        {
            emit(events, CoreEvent::stage(template_stage(&template)));
        }

        let (platform, project_id, version_id) = match source {
            ModpackSource::Platform {
                platform,
                project_id,
                version_id,
            } => (
                Some(*platform),
                Some(project_id.clone()),
                Some(version_id.clone()),
            ),
            ModpackSource::File { .. } => (None, None, None),
        };
        let info = InstanceModpack {
            schema: MODPACK_INFO_SCHEMA,
            format: manifest.format,
            platform,
            project_id,
            version_id,
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            minecraft: manifest.minecraft.clone(),
            loader: manifest.loader.clone(),
            remote_files,
            override_files,
            installed_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(CoreError::SystemClock)?
                .as_secs(),
            recommended_ram_mb: manifest.recommended_ram_mb,
        };
        let meta = version_dir.join(PETALFALL_META_DIR);
        let bytes = serde_json::to_vec_pretty(&info).map_err(CoreError::JsonSerialize)?;
        petalfall_base::fs::atomic_write(meta.join(MODPACK_INFO_FILE), &bytes).await?;
        MarkerStore::for_version_dir(version_dir).clear().await?;
        emit(
            events,
            CoreEvent::stage(format!("整合包已导入为实例 {name}")),
        );
        Ok(info)
    }

    /// 在实例目录写以实例名为 id 的版本 JSON。
    async fn materialize_version_json(
        &self,
        name: &str,
        version_dir: &Path,
        vanilla_id: &str,
        loader_id: Option<&str>,
    ) -> Result<()> {
        let layout = self.layout();
        let json =
            match loader_id {
                // 复制原文而不是经模型重新序列化：加载器 JSON 里有模型不认识的字段，丢了就启动不了。
                Some(loader_id) => {
                    let path = layout.version_json(loader_id);
                    let bytes = tokio::fs::read(&path).await.map_err(|source| {
                        petalfall_base::Error::Io {
                            path: path.clone(),
                            source,
                        }
                    })?;
                    let mut value: serde_json::Value = serde_json::from_slice(&bytes)
                        .map_err(|source| CoreError::JsonFileInvalid { path, source })?;
                    value["id"] = serde_json::Value::String(name.to_owned());
                    value
                }
                None => {
                    let path = layout.version_json(vanilla_id);
                    let bytes = tokio::fs::read(&path).await.map_err(|source| {
                        petalfall_base::Error::Io {
                            path: path.clone(),
                            source,
                        }
                    })?;
                    let vanilla: serde_json::Value = serde_json::from_slice(&bytes)
                        .map_err(|source| CoreError::JsonFileInvalid { path, source })?;
                    let mut value = serde_json::json!({ "id": name, "inheritsFrom": vanilla_id });
                    // 版本类型决定按「非正式版」隔离的档位，跟原版走。
                    if let Some(kind) = vanilla.get("type") {
                        value["type"] = kind.clone();
                    }
                    value
                }
            };
        let bytes = serde_json::to_vec_pretty(&json).map_err(CoreError::JsonSerialize)?;
        petalfall_base::fs::atomic_write(version_dir.join(format!("{name}.json")), &bytes).await?;
        Ok(())
    }

    /// 不区分大小写地检查 `versions/` 下有没有同名目录；有就报与准备阶段相同的重名错误。
    ///
    /// 准备阶段已经查过一次；调用方占住实例名之后再查一次，两次之间别的命令可能抢先建了同名实例，早一步
    /// 报重名。查过了也只说明目录列表里此刻没有这个名字：别名认不出来，之后才冒出来的目录也挡不住。名下的目录
    /// 是不是这次导入建的不看这里，只认 [`InstanceDirCreated`]。
    pub async fn ensure_modpack_name_free(&self, name: &str) -> Result<()> {
        let versions = self.game_dir().join(VERSIONS_DIR);
        let mut entries = match tokio::fs::read_dir(&versions).await {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(source) => {
                return Err(petalfall_base::Error::Io {
                    path: versions,
                    source,
                }
                .into());
            }
        };
        let wanted = name.to_lowercase();
        while let Some(entry) =
            entries
                .next_entry()
                .await
                .map_err(|source| petalfall_base::Error::Io {
                    path: versions.clone(),
                    source,
                })?
        {
            if entry.file_name().to_string_lossy().to_lowercase() == wanted {
                return Err(name_error(name, NameViolation::Duplicate));
            }
        }
        Ok(())
    }

    /// 平台整合包下载到缓存目录，返回包路径与它所在的临时目录。
    ///
    /// 目录每次导入一个、名字带随机后缀：同一个版本同时导入两次时，先结束的一路删目录不能动另一路手里的包。
    /// 代价是下载失败后分片没有下一次来续传，当场连目录删掉。
    ///
    /// CurseForge 包本体也可能不允许启动器直接下载。设置 `download_restricted_curseforge_files` 打开、拼出的
    /// 地址能下时照常下载到缓存目录；拿不到能下的地址时报的受限清单里只有这个包，用户从网页下载、认领进
    /// 暂存位后，重新安装仍拿不到地址就读暂存副本，没有要删的临时目录（暂存副本留给下次用）。
    async fn download_platform_pack(
        &self,
        platform: Platform,
        project_id: &str,
        version_id: &str,
        events: Option<&EventSink>,
    ) -> Result<(PathBuf, Option<PathBuf>)> {
        // 版本 id 要拼进缓存目录名。
        if !is_safe_file_name(version_id) {
            return Err(CoreError::InvalidProjectId {
                platform: platform.display_name(),
                project_id: format!("{project_id}/{version_id}"),
            });
        }
        let root = self.cache_root().join(PACK_CACHE_DIR);
        let platform_slug = match platform {
            Platform::Modrinth => "modrinth",
            Platform::CurseForge => "curseforge",
        };
        let suffix = fastrand::u32(..);
        let dir = root.join(format!("{platform_slug}-{version_id}-{suffix:08x}"));
        let (file_name, task) = match self
            .platform_file_task(platform, project_id, version_id, &dir, "")
            .await
        {
            Ok(found) => found,
            Err(err) => {
                let (pack, staged) = self.claimed_instead_of(err).await?;
                emit(
                    events,
                    CoreEvent::stage(format!("使用手动下载的整合包 {}", pack.file_name)),
                );
                return Ok((staged, None));
            }
        };
        sweep_stale_pack_caches(&root, SystemTime::now()).await;
        emit(events, CoreEvent::stage(format!("下载整合包 {file_name}")));
        let dest = task.dest.clone();
        let failure = match self
            .download_pool_reporting(events)
            .download_all(vec![task], None)
            .await
        {
            Ok(report) => report.failures.into_iter().next().map(|f| f.error),
            Err(err) => Some(err),
        };
        if let Some(err) = failure {
            remove_pack_cache(Some(&dir)).await;
            return Err(err.into());
        }
        Ok((dest, Some(dir)))
    }

    /// 把清单里的远程文件变成下载任务（落点都在实例目录下）；CurseForge 的受限文件拿不到能下的地址时，
    /// 认领到了就从暂存位复制，落点在 `covered`（见 [`override_landings`]）里的不用管。
    async fn remote_downloads(
        &self,
        remote: &RemoteFiles,
        covered: &HashSet<String>,
        version_dir: &Path,
    ) -> Result<RemotePlan> {
        match remote {
            RemoteFiles::Modrinth(files) => {
                let scope = self.curseforge_cdn_scope();
                let credential = modrinth_cdn_credential(files, &scope, || {
                    self.curseforge_credential(&self.curseforge_client()?)
                })?;
                let mut downloads = Vec::with_capacity(files.len());
                for file in files {
                    let Some((first, rest)) = file.urls.split_first() else {
                        return Err(petalfall_modpack::Error::ManifestInvalid {
                            document: "modrinth.index.json",
                            reason: format!("{} 没有下载地址", file.path),
                        }
                        .into());
                    };
                    let mut task =
                        DownloadTask::new(first.clone(), file.path.resolve_under(version_dir))
                            .with_sha1(file.sha1.clone())
                            .with_size(file.size);
                    // 凭据自带作用域，换到备用地址后引擎照样逐跳判定，所以只要有一个地址在 CDN 上就挂。
                    if let Some(credential) = &credential
                        && file.urls.iter().any(|url| on_cdn(&scope, url))
                    {
                        task = task.with_credential(credential.clone());
                    }
                    downloads.push(RemoteDownload {
                        task,
                        fallbacks: rest.to_vec(),
                    });
                }
                Ok(RemotePlan {
                    downloads,
                    claimed: Vec::new(),
                })
            }
            RemoteFiles::CurseForge(refs) if refs.is_empty() => Ok(RemotePlan::default()),
            RemoteFiles::CurseForge(refs) => {
                let client = self.curseforge_client()?;
                self.curseforge_downloads(&client, refs, covered, version_dir)
                    .await
            }
        }
    }

    /// CurseForge 清单里的文件 id 换成下载任务：落点按 [`CurseForgeIndex::locate`]，地址先用文件对象自带的，
    /// 没给的再问 download-url 端点，这两种任务挂上 CDN 下载凭据。设置 `download_restricted_curseforge_files`
    /// 打开时，端点也不给、落点又没被覆盖层盖住的再按文件编号与文件名拼出 CDN 地址、探测能下就用（见
    /// [`curseforge_fallback_url`]），这种任务不挂凭据：作者没把文件开放给第三方分发，不以落樱的 key 去取。
    ///
    /// 都拿不到地址的是作者禁止第三方分发的文件：落点被覆盖层盖住的跳过，暂存位里有校验通过的副本就改为复制；
    /// 还有没认领到的就汇总成一个错误，只列这些。落点被盖住的文件不拼地址、不探测：它导入后反正会被包里自带
    /// 的那份盖掉，为它探测只会多依赖一次 CDN，探测出错还会让整个导入失败；端点给了地址的被盖住文件照旧下载
    /// （随后被盖掉）。认领副本排在拼地址之后，所以设置开着时能下的受限文件不再用认领过的副本。
    async fn curseforge_downloads(
        &self,
        client: &CurseForgeClient,
        refs: &[CurseForgeFileRef],
        covered: &HashSet<String>,
        version_dir: &Path,
    ) -> Result<RemotePlan> {
        let index = CurseForgeIndex::fetch(client, refs).await?;
        let credential = self.curseforge_credential(client)?;
        let derive_restricted = self.config().download_restricted_curseforge_files;
        let mut plan = RemotePlan::default();
        let mut blocked = Vec::new();
        // 逐个串行地要地址就够了：没给地址的只有作者禁止第三方分发的那几个文件，每个多一次端点请求、
        // 设置开着时再多一次 HEAD，并发省不下多少时间。
        for reference in refs {
            let (file, sub) = index.locate(reference)?;
            let dest = version_dir.join(sub).join(&file.file_name);
            let task = match file.to_download_task(&dest) {
                Some(task) => task.with_credential(credential.clone()),
                None => {
                    let landing_covered =
                        covered.contains(&join_landing(sub, &file.file_name).to_lowercase());
                    match curseforge_fallback_url(
                        client,
                        reference.project_id,
                        reference.file_id,
                        &file.file_name,
                        derive_restricted && !landing_covered,
                    )
                    .await?
                    {
                        Some(url) => {
                            url.into_task(|url| task_for_file_url(url, &dest, file), &credential)
                        }
                        None => {
                            let item = index.blocked(reference, file, sub)?;
                            if landing_covered {
                                continue;
                            }
                            match self.claimed_copy(&item).await? {
                                Some(staged) => plan.claimed.push(ClaimedCopy { staged, dest }),
                                None => blocked.push(item),
                            }
                            continue;
                        }
                    }
                }
            };
            plan.downloads.push(RemoteDownload {
                task,
                fallbacks: Vec::new(),
            });
        }
        if !blocked.is_empty() {
            return Err(CoreError::FileDistributionBlocked { files: blocked });
        }
        Ok(plan)
    }

    /// 远程文件的落点与总字节数。Modrinth 清单自带；CurseForge 要多问两个批量接口，没配 API key 时
    /// 与导入一样报 key 缺失（该源被禁用），没有远程文件时不问。`probe` 决定要不要逐个探出受限文件；探的时候
    /// 按设置 `download_restricted_curseforge_files` 决定要不要再拼 CDN 地址探测，与导入同一口径（落点被
    /// 覆盖层盖住的文件除外，见 [`curseforge_remote_facts`]）。
    pub(crate) async fn remote_facts(
        &self,
        remote: &RemoteFiles,
        probe: BlockedProbe,
    ) -> Result<RemoteFacts> {
        match remote {
            RemoteFiles::Modrinth(files) => Ok(RemoteFacts {
                entries: files
                    .iter()
                    .map(|f| RemoteEntry {
                        destination: f.path.as_str().to_owned(),
                        optional: f.optional,
                        project_id: None,
                        file_id: None,
                        blocked: None,
                    })
                    .collect(),
                bytes: Some(files.iter().map(|f| f.size).sum()),
            }),
            RemoteFiles::CurseForge(refs) if refs.is_empty() => Ok(RemoteFacts {
                entries: Vec::new(),
                bytes: Some(0),
            }),
            RemoteFiles::CurseForge(refs) => {
                curseforge_remote_facts(
                    &self.curseforge_client()?,
                    refs,
                    probe,
                    self.config().download_restricted_curseforge_files,
                )
                .await
            }
        }
    }

    fn curseforge_cdn_scope(&self) -> CredentialScope {
        #[cfg(test)]
        if let Some(scope) = self.injected_curseforge_cdn_scope() {
            return scope.clone();
        }
        CredentialScope::curseforge_cdn()
    }

    /// 批量下载远程文件；失败的文件有备用地址就换地址再下一轮。
    async fn download_remote(
        &self,
        mut pending: Vec<RemoteDownload>,
        events: Option<&EventSink>,
    ) -> Result<()> {
        let pool = self.download_pool_reporting(events);
        loop {
            let tasks = pending.iter().map(|r| r.task.clone()).collect();
            let report = pool.download_all(tasks, None).await?;
            if report.failures.is_empty() {
                return Ok(());
            }
            let mut failed: HashMap<PathBuf, petalfall_download::Error> = report
                .failures
                .into_iter()
                .map(|f| (f.task.dest, f.error))
                .collect();
            let mut retry = Vec::new();
            for mut item in pending {
                let Some(error) = failed.remove(&item.task.dest) else {
                    continue;
                };
                if item.fallbacks.is_empty() {
                    return Err(error.into());
                }
                tracing::warn!(url = %item.task.url, error = %error, "整合包文件下载失败，换备用地址");
                item.task.url = item.fallbacks.remove(0);
                retry.push(item);
            }
            pending = retry;
        }
    }
}

fn on_cdn(scope: &CredentialScope, url: &str) -> bool {
    reqwest::Url::parse(url).is_ok_and(|url| scope.allows(&url))
}

/// Modrinth 清单文件要用的 CDN 凭据。别的启动器导出的 .mrpack 会把 CurseForge 上的 Mod 写成 forgecdn 地址，
/// 这类地址与平台给出的地址一样，下载时带 key；清单里没有这类地址时不去要 key，没配 key 也照常导入。
///
/// 没配 key 时，每个文件都还有别的地址可换就只记日志（CDN 那个地址下不成时换下一个）；有文件只能从 CDN
/// 下载就在这里报错，不先装几百 MB 游戏再告诉用户装不完。key 配了但不合法是用户要改的错，不在此列。
fn modrinth_cdn_credential(
    files: &[ModrinthFile],
    scope: &CredentialScope,
    resolve: impl FnOnce() -> Result<DownloadCredential>,
) -> Result<Option<DownloadCredential>> {
    let has_cdn_url = files
        .iter()
        .any(|file| file.urls.iter().any(|url| on_cdn(scope, url)));
    if !has_cdn_url {
        return Ok(None);
    }
    match resolve() {
        Ok(credential) => Ok(Some(credential)),
        Err(CoreError::ModPlatform(petalfall_modplatform::Error::CurseForgeKeyMissing))
            if files
                .iter()
                .all(|file| file.urls.iter().any(|url| !on_cdn(scope, url))) =>
        {
            tracing::warn!(
                "整合包里有 CurseForge CDN 地址但没配下载密钥，这些地址会失败，改走备用地址"
            );
            Ok(None)
        }
        Err(err) => Err(err),
    }
}

/// CurseForge 清单引用的文件与各自所属工程的大类。导入换下载任务、摘要与预览数 Mod 和算下载量都从这里
/// 取落点，保证「预先报的数」与「导入后真正落进去的」是同一条规则。
struct CurseForgeIndex {
    files: HashMap<u32, CurseForgeFile>,
    /// 批量工程接口回的整份工程：落点要它的大类，受限文件的网页链接要它的 links 与 slug。
    projects: HashMap<u32, CurseForgeMod>,
}

impl CurseForgeIndex {
    /// 全部文件 id 走批量文件接口、去重后的工程 id 走批量工程接口，各一个请求（超过一批时分批）。
    async fn fetch(client: &CurseForgeClient, refs: &[CurseForgeFileRef]) -> Result<Self> {
        let file_ids: Vec<u32> = refs.iter().map(|r| r.file_id).collect();
        let mut files: HashMap<u32, CurseForgeFile> = HashMap::with_capacity(refs.len());
        for chunk in file_ids.chunks(CURSEFORGE_BATCH) {
            for file in client.get_files(chunk).await? {
                files.insert(file.id, file);
            }
        }
        let mut mod_ids: Vec<u32> = refs
            .iter()
            .map(|r| r.project_id)
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        mod_ids.sort_unstable();
        let mut projects: HashMap<u32, CurseForgeMod> = HashMap::with_capacity(mod_ids.len());
        for chunk in mod_ids.chunks(CURSEFORGE_BATCH) {
            for project in client.get_mods(chunk).await? {
                projects.insert(project.id, project);
            }
        }
        Ok(Self { files, projects })
    }

    /// 清单引用的文件与它在实例里的落点目录（工程的大类决定落进哪个目录）。平台响应里没有这个文件报
    /// 版本不存在，文件名带路径成分报不安全：两者导入都装不了，预先报数时也不该装作没事。
    fn locate(&self, reference: &CurseForgeFileRef) -> Result<(&CurseForgeFile, &'static str)> {
        let file =
            self.files
                .get(&reference.file_id)
                .ok_or_else(|| CoreError::ModVersionNotFound {
                    platform: Platform::CurseForge.display_name(),
                    project_id: reference.project_id.to_string(),
                    version_id: reference.file_id.to_string(),
                })?;
        if !is_safe_file_name(&file.file_name) {
            return Err(CoreError::UnsafeFileName {
                platform: Platform::CurseForge.display_name(),
                file_name: file.file_name.clone(),
            });
        }
        let class_id = self
            .projects
            .get(&reference.project_id)
            .and_then(|project| project.class_id);
        let sub = match class_id {
            Some(CF_CLASS_RESOURCE_PACKS) => "resourcepacks",
            Some(CF_CLASS_SHADERS) => "shaderpacks",
            // 数据包在整合包里是给全局数据包类 Mod 加载的，放实例根下的 datapacks/。
            Some(CF_CLASS_DATA_PACKS) => "datapacks",
            Some(CF_CLASS_MODS) => "mods",
            other => {
                tracing::warn!(project_id = reference.project_id, class_id = ?other, file = %file.file_name, "整合包文件的工程大类不认识，按 Mod 处理");
                "mods"
            }
        };
        Ok((file, sub))
    }

    /// 把一个受限文件拼成清单项。网页链接要工程信息：批量工程接口没回这个工程时，拼不出能打开的页面，
    /// 与文件缺失一样报不存在。
    fn blocked(
        &self,
        reference: &CurseForgeFileRef,
        file: &CurseForgeFile,
        sub: &str,
    ) -> Result<BlockedFile> {
        let project = self.projects.get(&reference.project_id).ok_or_else(|| {
            CoreError::ModVersionNotFound {
                platform: Platform::CurseForge.display_name(),
                project_id: reference.project_id.to_string(),
                version_id: reference.file_id.to_string(),
            }
        })?;
        Ok(BlockedFile::new(
            project,
            file,
            join_landing(sub, &file.file_name),
        ))
    }
}

/// 读远程文件的事实时要不要逐个探出受限文件：本地预览要（用户在导入前就该知道要手动下载哪些），平台
/// 摘要不要（只为数 Mod、报下载量，不为它多发请求）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BlockedProbe {
    Probe,
    Skip,
}

/// 清单里的远程文件导入后落在哪、一共多大：平台摘要与本地预览据此数 Mod、报下载量。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteFacts {
    /// 清单顺序的每个远程文件。
    pub(crate) entries: Vec<RemoteEntry>,
    /// 总字节数；平台没给齐每个文件的大小时为 `None`，不拿缺了几项的和冒充总量。
    pub(crate) bytes: Option<u64>,
}

impl RemoteFacts {
    /// 相对实例目录的落点，正斜杠分隔。
    pub(crate) fn destinations(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|entry| entry.destination.as_str())
    }
}

/// 一个远程文件的事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteEntry {
    /// 相对实例目录的落点，正斜杠分隔。
    pub(crate) destination: String,
    pub(crate) optional: bool,
    /// CurseForge 的工程与文件 id；Modrinth 文件为 `None`。
    pub(crate) project_id: Option<u32>,
    pub(crate) file_id: Option<u32>,
    /// 作者不允许启动器直接下载时的清单项；没探（[`BlockedProbe::Skip`]）或不受限为 `None`。
    pub(crate) blocked: Option<BlockedFile>,
}

/// CurseForge 清单只有工程与文件 id：落点与大小经 [`CurseForgeIndex`] 向批量接口要。`probe` 为
/// [`BlockedProbe::Probe`] 时，没给下载地址的文件再经 [`curseforge_fallback_url`] 逐个判定，与导入同一口径：
/// 先问 download-url 端点，`derive_restricted`（配置里的 `download_restricted_curseforge_files`）为真时端点
/// 也不给的再拼 CDN 地址探测，能下的不算受限，探测出错照常报错。这里不知道覆盖层，落点被盖住的受限文件
/// 照样要问、设置开着时照样探测，由调用方再筛掉；导入那边不为被盖住的文件探测（见
/// `Petalfall::curseforge_downloads`）。[`BlockedProbe::Skip`] 时一个都不问，`derive_restricted` 不起作用。
pub(crate) async fn curseforge_remote_facts(
    client: &CurseForgeClient,
    refs: &[CurseForgeFileRef],
    probe: BlockedProbe,
    derive_restricted: bool,
) -> Result<RemoteFacts> {
    let index = CurseForgeIndex::fetch(client, refs).await?;
    let mut entries = Vec::with_capacity(refs.len());
    let mut bytes = Some(0u64);
    // 逐个串行地问就够了：要问的只有作者禁止第三方分发的那几个文件，每个多一次端点请求、设置开着时
    // 再多一次 HEAD，并发省不下多少时间。
    for reference in refs {
        let (file, sub) = index.locate(reference)?;
        let blocked = match probe {
            // 空白地址算没给：这条规则要与 `CurseForgeFile::to_download_task` 保持一致，导入按它决定要不要
            // 问端点，两边口径一岔开，预览说能下的文件导入时就会报受限。
            BlockedProbe::Probe
                if file
                    .download_url
                    .as_deref()
                    .is_none_or(|url| url.trim().is_empty()) =>
            {
                match curseforge_fallback_url(
                    client,
                    reference.project_id,
                    reference.file_id,
                    &file.file_name,
                    derive_restricted,
                )
                .await?
                {
                    Some(_) => None,
                    None => Some(index.blocked(reference, file, sub)?),
                }
            }
            BlockedProbe::Probe | BlockedProbe::Skip => None,
        };
        entries.push(RemoteEntry {
            destination: join_landing(sub, &file.file_name),
            optional: false,
            project_id: Some(reference.project_id),
            file_id: Some(reference.file_id),
            blocked,
        });
        bytes = bytes
            .zip(file.file_length)
            .map(|(sum, length)| sum + length);
    }
    Ok(RemoteFacts { entries, bytes })
}

/// 覆盖文件的落点，已转小写，与 Windows 上落盘的结果一致。覆盖文件在下载之后解出，落点相同的远程文件
/// 反正会被盖掉，受限的那些不必让用户手动下载。
fn override_landings(archive: &PackArchive) -> HashSet<String> {
    archive
        .overrides
        .iter()
        .map(|entry| entry.path.as_str().to_lowercase())
        .collect()
}

/// 整合包的落点不许撞上实例自己的版本文件：实例目录里与实例同名的版本 JSON、主 jar 与 natives 目录。
///
/// 导入先写版本 JSON，再下清单文件、解覆盖文件，落在同一个名字上的那份会把它盖掉，实例从此按包里那份 JSON
/// 启动。主 jar 与 natives 目录这次导入不写，名下的位置却归日后的补全与改装用，一并不让占。撞上就整包拒收，
/// 不悄悄跳过那一个文件：包里放了它，说明作者以为它会生效。实例名是导入时才定的，换一个名字同一个包就能装。
///
/// 只看落点的第一段，路径校验（[`SafeRelativePath`]）已经挡掉了 `..`、短文件名与结尾的点和空格这些别名写法。
/// CurseForge 清单里的文件落在按类型定的子目录里，到不了实例目录这一层，不用查。
///
/// 比较不分大小写，而且逐个字符先转大写再转小写，不用整串的 `to_lowercase`：文件系统折叠大小写不看上下文，
/// 整串转小写却会把词尾的「Σ」写成「ς」，实例名里有以「Σ」收尾的词、后面跟着空格或连字符时（natives 目录
/// 就是名字后面接连字符），包里写成「σ」的同一个文件就比不上了。各个卷的折叠表不一样（格式化时写下的，文件
/// 系统之间也不同），所以往宽里折：多认了只是让人换个实例名，少认了丢的是版本 JSON。「ς」与「Σ」、「ſ」与
/// 「s」这类有的卷认、有的卷不认的一律当成同名；「ß」与「ss」这类没有哪个卷认的也跟着多认了。
fn ensure_pack_spares_instance_files(
    layout: &GameLayout,
    name: &str,
    archive: &PackArchive,
) -> Result<()> {
    let key = |path: PathBuf| -> String {
        let path = path.to_string_lossy();
        path.chars()
            .flat_map(char::to_uppercase)
            .flat_map(char::to_lowercase)
            .collect()
    };
    let version_dir = layout.version_dir(name);
    let owned = [
        layout.version_json(name),
        layout.version_jar(name),
        layout.natives_dir(name),
    ]
    .map(key);
    let lands_on_owned =
        |path: &SafeRelativePath| owned.contains(&key(version_dir.join(path.top_level())));
    let remote = match &archive.manifest.remote {
        RemoteFiles::Modrinth(files) => files.as_slice(),
        RemoteFiles::CurseForge(_) => &[],
    };
    let collision = remote
        .iter()
        .find(|file| lands_on_owned(&file.path))
        .map(|file| file.path.as_str())
        .or_else(|| {
            archive
                .overrides
                .iter()
                .find(|entry| lands_on_owned(&entry.path))
                .map(|entry| entry.entry_name.as_str())
        });
    match collision {
        Some(path) => Err(petalfall_modpack::Error::InstanceFileCollision {
            path: path.to_owned(),
            instance: name.to_owned(),
        }
        .into()),
        None => Ok(()),
    }
}

fn file_name_of(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_owned()
}

/// 预览里的 Mod 清单与其他可选文件。Mod 清单与 [`count_pack_mods`] 同一口径：直接落在 `mods/` 下的远程
/// 文件，加上覆盖层 `mods/` 下的 `.jar`；同一落点不分大小写只留一份，覆盖层胜出（它会盖掉下载来的那份）。
fn pack_entries(
    archive: &PackArchive,
    remote: &RemoteFacts,
    claimed: &HashSet<u32>,
) -> (Vec<PackModEntry>, Vec<PackModEntry>) {
    let mut mods: HashMap<String, PackModEntry> = HashMap::new();
    let mut extras = Vec::new();
    for entry in &remote.entries {
        let availability = match &entry.blocked {
            Some(item) if claimed.contains(&item.file_id) => PackFileAvailability::Claimed,
            Some(_) => PackFileAvailability::Blocked,
            None => PackFileAvailability::Download,
        };
        let item = PackModEntry {
            path: entry.destination.clone(),
            file_name: file_name_of(&entry.destination),
            origin: PackFileOrigin::Remote,
            optional: entry.optional,
            project_id: entry.project_id,
            file_id: entry.file_id,
            availability,
        };
        let key = entry.destination.to_lowercase();
        if directly_in_mods(&key) {
            mods.insert(key, item);
        } else if entry.optional {
            extras.push(item);
        }
    }
    for entry in &archive.overrides {
        let key = entry.path.as_str().to_lowercase();
        if directly_in_mods(&key) && key.ends_with(".jar") {
            mods.insert(
                key,
                PackModEntry {
                    path: entry.path.as_str().to_owned(),
                    file_name: file_name_of(entry.path.as_str()),
                    origin: PackFileOrigin::Override,
                    optional: false,
                    project_id: None,
                    file_id: None,
                    availability: PackFileAvailability::Bundled,
                },
            );
        }
    }
    let mut mods: Vec<PackModEntry> = mods.into_values().collect();
    mods.sort_by_cached_key(|entry| entry.path.to_lowercase());
    extras.sort_by_cached_key(|entry| entry.path.to_lowercase());
    (mods, extras)
}

fn preview_of(
    archive: &PackArchive,
    remote: RemoteFacts,
    claimed: &HashSet<u32>,
    blocked_files: Vec<BlockedFile>,
) -> ModpackPreview {
    let manifest = &archive.manifest;
    let (mods, optional_extras) = pack_entries(archive, &remote, claimed);
    ModpackPreview {
        format: manifest.format,
        name: manifest.name.clone(),
        version: manifest.version.clone(),
        summary: manifest.summary.clone(),
        author: manifest.author.clone(),
        minecraft: manifest.minecraft.clone(),
        loader: manifest.loader.clone(),
        remote_files: manifest.remote.len(),
        mod_files: count_pack_mods(
            remote.destinations(),
            archive.overrides.iter().map(|entry| entry.path.as_str()),
        ),
        remote_bytes: remote.bytes,
        override_files: archive.overrides.len(),
        override_bytes: archive.overrides_bytes,
        mods,
        optional_extras,
        blocked_files,
    }
}

pub(crate) async fn read_pack_blocking(path: PathBuf) -> Result<PackArchive> {
    Ok(tokio::task::spawn_blocking(move || petalfall_modpack::read_pack(&path)).await??)
}

fn name_error(name: &str, rule: NameViolation) -> CoreError {
    CoreError::InstanceNameInvalid {
        name: name.to_owned(),
        rule,
    }
}

fn loader_choice(kind: PackLoaderKind) -> LoaderChoice {
    match kind {
        PackLoaderKind::Fabric => LoaderChoice::Fabric,
        PackLoaderKind::Quilt => LoaderChoice::Quilt,
        PackLoaderKind::Forge => LoaderChoice::Forge,
        PackLoaderKind::NeoForge => LoaderChoice::NeoForge,
    }
}

/// 整合包清单里的加载器种类，对应到版本 JSON 上探测出的种类。
fn detected_kind(kind: PackLoaderKind) -> LoaderKind {
    match kind {
        PackLoaderKind::Fabric => LoaderKind::Fabric,
        PackLoaderKind::Quilt => LoaderKind::Quilt,
        PackLoaderKind::Forge => LoaderKind::Forge,
        PackLoaderKind::NeoForge => LoaderKind::NeoForge,
    }
}

/// 盘上这个版本若认得出不是整合包要装在这个名字下的东西，说出它是什么（接在「它」后面的短语）；是同一个东西、
/// 或认不出它是别的东西时为 `None`。`loader` 为 `None` 表示这个名字下要装的是原版。
///
/// 原版的位置上，装着任何加载器、或继承着别的版本的都不是原版。加载器的位置上，探得到的加载器不是这一种、或
/// 除了它还装着别的（比如 OptiFine）才算；一个加载器都探不出来的版本 JSON 说不清是什么，照常重装。
fn occupied_by(found: &DiscoveredVersion, loader: Option<&PackLoader>) -> Option<String> {
    let kinds: Vec<LoaderKind> = found.loaders.iter().map(|loader| loader.kind).collect();
    let names = |kinds: &[LoaderKind]| {
        let names: Vec<&str> = kinds.iter().map(|kind| kind.as_str()).collect();
        names.join("、")
    };
    match loader {
        None if !kinds.is_empty() => Some(format!("装着 {}", names(&kinds))),
        None => found
            .json
            .inherits_from
            .as_deref()
            .map(str::trim)
            .filter(|parent| !parent.is_empty())
            .map(|parent| format!("继承自 {parent}")),
        Some(loader) => {
            let wanted = detected_kind(loader.kind);
            let others: Vec<LoaderKind> = kinds
                .iter()
                .copied()
                .filter(|kind| *kind != wanted)
                .collect();
            match others.len() {
                0 => None,
                // 要的那种也在：只说「装着 A、B」读起来像它已经是这个加载器了，把多出来的点出来。
                extra if extra < kinds.len() => Some(format!(
                    "除了 {} 还装着 {}",
                    wanted.as_str(),
                    names(&others)
                )),
                _ => Some(format!("装着 {}", names(&kinds))),
            }
        }
    }
}

/// 有的导出工具把 Forge 版本写成 `1.20.1-47.2.0`，安装器地址要的是不带 MC 前缀的部分。
fn strip_minecraft_prefix<'a>(version: &'a str, minecraft: &str) -> &'a str {
    version
        .strip_prefix(minecraft)
        .and_then(|rest| rest.strip_prefix('-'))
        .unwrap_or(version)
}

/// 原子地建实例目录（`versions/` 由调用方先建好）：同名目录已存在（含并发导入抢先建好，含名字是另一个目录
/// 的别名）即报重名。建成才置位 `created`。
///
/// 同步建：建目录与置位之间不能有让出点。导入的 future 随时可能被丢弃，异步建的话目录也许已经建出来、记号却
/// 没来得及置位，半成品就没人清了。只是一次建目录的系统调用，不值得为它挪到阻塞线程上。
fn create_instance_dir(version_dir: &Path, name: &str, created: &InstanceDirCreated) -> Result<()> {
    match std::fs::create_dir(version_dir) {
        Ok(()) => {
            created.set();
            Ok(())
        }
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            Err(name_error(name, NameViolation::Duplicate))
        }
        Err(source) => Err(petalfall_base::Error::Io {
            path: version_dir.to_path_buf(),
            source,
        }
        .into()),
    }
}

async fn write_marker(version_dir: &Path, task_id: &str) -> Result<()> {
    let marker = IncompleteMarker::pending(
        IncompleteKind::Modpack {
            task_id: task_id.to_owned(),
        },
        now_millis(),
    );
    MarkerStore::for_version_dir(version_dir)
        .save(&marker)
        .await?;
    Ok(())
}

async fn remove_instance_dir(version_dir: &Path) -> Result<()> {
    tokio::fs::remove_dir_all(version_dir)
        .await
        .map_err(|source| {
            petalfall_base::Error::Io {
                path: version_dir.to_path_buf(),
                source,
            }
            .into()
        })
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;
    use crate::config::PetalfallConfig;
    use petalfall_instance::{IsolationPolicy, discover_versions};
    use sha1::{Digest, Sha1};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn write_pack(dest: &Path, index: &serde_json::Value, entries: &[(&str, &[u8])]) {
        let mut writer = zip::ZipWriter::new(std::fs::File::create(dest).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        writer.start_file("modrinth.index.json", options).unwrap();
        writer
            .write_all(&serde_json::to_vec(index).unwrap())
            .unwrap();
        for (name, bytes) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap();
    }

    fn index(dependencies: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "formatVersion": 1, "game": "minecraft", "versionId": "1.0.0", "name": "测试包",
            "summary": "只用于测试", "files": [], "dependencies": dependencies
        })
    }

    /// 让 mock 提供一个「空壳」原版 test：版本 JSON 无下载、无库，安装只落版本 JSON。
    async fn mount_vanilla(server: &MockServer) {
        let base = server.uri();
        Mock::given(method("GET"))
            .and(path("/manifest.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(format!(
                r#"{{"latest":{{"release":"test","snapshot":"test"}},
                    "versions":[{{"id":"test","type":"release","url":"{base}/test.json",
                    "time":"t","releaseTime":"t"}}]}}"#
            )))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/test.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"id":"test","type":"release","mainClass":"net.minecraft.client.main.Main"}"#,
            ))
            .mount(server)
            .await;
    }

    /// 别的启动器导出的 .mrpack 会把 CurseForge 上的 Mod 写成 forgecdn 地址，读包时放行。这些任务与平台
    /// 给出的 CurseForge 地址一样，挂上同一份下载凭据；只有 Modrinth 地址的任务不带。
    #[tokio::test]
    async fn forgecdn_files_in_a_modrinth_pack_carry_the_download_credential() {
        const EDGE: &str = "https://edge.forgecdn.net/files/3040/523/jei.jar";
        const MEDIA: &str = "https://mediafilez.forgecdn.net/files/3040/524/rei.jar";
        const CDN: &str = "https://cdn.modrinth.com/data/x/sodium.jar";
        let server = MockServer::start().await;
        let tmp = tempfile::tempdir().unwrap();
        let petalfall = petalfall(tmp.path(), &server).with_curseforge_key("pack-key");
        let file = |name: &str, urls: &[&str]| {
            let urls: Vec<String> = urls.iter().map(|url| (*url).to_owned()).collect();
            modrinth_file(&format!("mods/{name}"), &urls, b"x")
        };
        let remote = RemoteFiles::Modrinth(vec![
            file("jei.jar", &[EDGE]),
            file("sodium.jar", &[CDN]),
            file("rei.jar", &[CDN, MEDIA]),
        ]);

        let plan = petalfall
            .remote_downloads(&remote, &HashSet::new(), Path::new("D:/mc/versions/包"))
            .await
            .unwrap();

        let carried: Vec<bool> = plan
            .downloads
            .iter()
            .map(|download| download.task.credential.is_some())
            .collect();
        assert_eq!(carried, vec![true, false, true]);
        let credential = plan.downloads[0].task.credential.as_ref().unwrap();
        assert_eq!(
            credential.realm(),
            petalfall_download::CredentialRealm::CurseForge
        );
        assert!(credential.applies_to(&reqwest::Url::parse(MEDIA).unwrap()));
        assert!(!credential.applies_to(&reqwest::Url::parse(CDN).unwrap()));
        assert_eq!(plan.downloads[2].fallbacks, vec![MEDIA.to_owned()]);
    }

    fn petalfall(root: &Path, server: &MockServer) -> Petalfall {
        let mc = root.join("mc");
        let mut petalfall = Petalfall::for_test(PetalfallConfig::default(), root.join("data"), mc)
            .with_manifest_url(format!("{}/manifest.json", server.uri()))
            .with_fabric_base(server.uri())
            .with_modrinth_base(server.uri())
            .with_curseforge_base(server.uri());
        // 全局不隔离：导入的实例仍必须隔离，验证的是实例自己的强制隔离。
        petalfall.set_isolation_policy(IsolationPolicy::Disabled);
        petalfall
    }

    fn sha1_hex(bytes: &[u8]) -> String {
        Sha1::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    #[test]
    fn instance_name_rules_cover_each_boundary() {
        let ok = |raw: &str, expected: &str| {
            assert_eq!(
                validate_instance_name(raw).as_deref(),
                Ok(expected),
                "{raw:?}"
            );
        };
        let bad = |raw: &str, rule: NameViolation| {
            assert_eq!(validate_instance_name(raw), Err(rule), "{raw:?}");
        };
        ok("  我的整合包 1.0  ", "我的整合包 1.0");
        ok(&"樱".repeat(32), &"樱".repeat(32));
        ok("COM0", "COM0");
        ok("COM10", "COM10");
        ok("Console", "Console");
        ok("a.b", "a.b");
        bad("", NameViolation::Empty);
        bad(" \t ", NameViolation::Empty);
        bad(&"樱".repeat(33), NameViolation::TooLong);
        for c in ['\\', '/', ':', '*', '?', '"', '<', '>', '|'] {
            bad(&format!("a{c}b"), NameViolation::IllegalChar);
        }
        bad("a\u{7}b", NameViolation::ReservedByWindows);
        bad(".hidden", NameViolation::LeadingDot);
        bad("pack.", NameViolation::ReservedByWindows);
        for reserved in [
            "CON", "prn", "Aux", "nul", "COM1", "com9", "LPT1", "lpt9", "con.txt", "NUL .x",
        ] {
            bad(reserved, NameViolation::ReservedByWindows);
        }
    }

    /// 名字长度按 UTF-16 码元算，与新建、改名和界面输入框同一计法：补充平面字符占两个码元。
    #[test]
    fn instance_name_length_counts_utf16_units() {
        assert_eq!(
            validate_instance_name(&"\u{1F338}".repeat(16)).as_deref(),
            Ok("\u{1F338}".repeat(16).as_str())
        );
        assert_eq!(
            validate_instance_name(&"\u{1F338}".repeat(17)),
            Err(NameViolation::TooLong)
        );
    }

    #[test]
    fn random_names_from_allowed_characters_pass_unchanged() {
        let seed = fastrand::u64(..);
        let mut rng = fastrand::Rng::with_seed(seed);
        let alphabet: Vec<char> = "abcXYZ019 -_+()[]樱花林落樱整合包".chars().collect();
        for _ in 0..500 {
            let len = rng.usize(1..=petalfall_instance::MAX_NAME_UNITS);
            let raw: String = (0..len)
                .map(|_| alphabet[rng.usize(..alphabet.len())])
                .collect();
            let name = raw.trim().to_owned();
            if name.is_empty() {
                continue;
            }
            assert_eq!(
                validate_instance_name(&raw),
                Ok(name.clone()),
                "seed {seed}: {raw:?}"
            );
        }
    }

    /// 本地原版整合包：实例目录、只继承原版的版本 JSON、强制隔离、覆盖文件分层、整合包信息全部落盘。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn local_vanilla_pack_becomes_an_isolated_instance_with_modpack_info() {
        let server = MockServer::start().await;
        mount_vanilla(&server).await;
        let tmp = tempfile::tempdir().unwrap();
        let pack = tmp.path().join("测试包.mrpack");
        write_pack(
            &pack,
            &index(serde_json::json!({"minecraft": "test"})),
            &[
                ("overrides/config/a.txt", b"config-a"),
                ("overrides/options.txt", b"lang:en_us"),
                ("client-overrides/options.txt", b"lang:zh_cn"),
                ("server-overrides/server.properties", b"motd=x"),
            ],
        );
        let petalfall = petalfall(tmp.path(), &server);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        let outcome = petalfall
            .install_modpack(
                &ModpackSource::File { path: pack.clone() },
                Some(" 我的整合包 "),
                &ImportSelection::default(),
                "task-1",
                Some(&tx),
            )
            .await
            .unwrap();

        let dir = petalfall.game_dir().join("versions").join("我的整合包");
        assert_eq!(outcome.version_id, "我的整合包");
        assert_eq!(outcome.version_dir, dir);
        let json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("我的整合包.json")).unwrap()).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"id": "我的整合包", "inheritsFrom": "test", "type": "release"})
        );
        assert_eq!(
            std::fs::read(dir.join("options.txt")).unwrap(),
            b"lang:zh_cn"
        );
        assert_eq!(
            std::fs::read(dir.join("config").join("a.txt")).unwrap(),
            b"config-a"
        );
        assert!(!dir.join("server.properties").exists());
        assert!(dir.join("mods").is_dir());
        let settings = VersionSettingsStore::for_version_dir(&dir)
            .load()
            .await
            .unwrap();
        assert_eq!(settings.isolation, IsolationOverride::Enabled);
        assert_eq!(
            MarkerStore::for_version_dir(&dir).load().await.unwrap(),
            None,
            "导入成功后未完成标记删除"
        );

        let info = read_instance_modpack(&dir)
            .await
            .unwrap()
            .expect("应写有整合包信息");
        assert_eq!(info, outcome.modpack);
        assert_eq!(info.schema, MODPACK_INFO_SCHEMA);
        assert_eq!(info.format, PackFormat::Modrinth);
        assert_eq!(info.platform, None);
        assert_eq!(info.name, "测试包");
        assert_eq!(info.version.as_deref(), Some("1.0.0"));
        assert_eq!(info.minecraft, "test");
        assert_eq!(info.loader, None);
        assert_eq!((info.remote_files, info.override_files), (0, 2));
        assert!(info.installed_at > 1_700_000_000);
        assert_eq!(info.recommended_ram_mb, None, "Modrinth 包没有推荐内存");
        assert!(
            !std::fs::read_to_string(dir.join(PETALFALL_META_DIR).join(MODPACK_INFO_FILE))
                .unwrap()
                .contains("recommended_ram_mb"),
            "没有推荐值时不写这一项，旧版落樱读到的记录与原来一样"
        );

        // 启动链路看到的是一个独立实例：被发现、无加载器、全局不隔离时工作目录仍是实例目录。
        let scan = discover_versions(petalfall.game_dir()).await.unwrap();
        let found = scan
            .versions
            .iter()
            .find(|v| v.id == "我的整合包")
            .expect("实例应被发现");
        assert!(!found.has_mod_loader());
        assert!(found.is_release());
        assert_eq!(
            petalfall
                .resolve_working_dir("我的整合包")
                .await
                .unwrap()
                .working_dir,
            dir
        );

        drop(tx);
        let mut stages = Vec::new();
        while let Some(event) = rx.recv().await {
            if let CoreEvent::Stage(stage) = event {
                stages.push(stage);
            }
        }
        assert!(
            stages.iter().any(|s| s == "整合包已导入为实例 我的整合包"),
            "{stages:?}"
        );
    }

    /// 整合包要的加载器装不上：导入失败，半成品实例被清掉，也不留一个看起来已安装的加载器版本。
    /// 原版是真的装好了，留着。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_failed_import_leaves_no_half_installed_loader_version() {
        let server = MockServer::start().await;
        mount_vanilla(&server).await;
        Mock::given(method("GET"))
            .and(path("/v2/versions/loader/test/0.15.0/profile/json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(format!(
                r#"{{"id":"fabric-loader-0.15.0-test","inheritsFrom":"test","mainClass":"Knot",
                    "libraries":[{{"name":"net.example:loader:0.15.0","url":"{}/"}}]}}"#,
                server.uri()
            )))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/net/example/loader/0.15.0/loader-0.15.0.jar"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let tmp = tempfile::tempdir().unwrap();
        let pack = tmp.path().join("测试包.mrpack");
        write_pack(
            &pack,
            &index(serde_json::json!({"minecraft": "test", "fabric-loader": "0.15.0"})),
            &[],
        );
        let petalfall = petalfall(tmp.path(), &server);

        petalfall
            .install_modpack(
                &ModpackSource::File { path: pack },
                Some("坏掉的包"),
                &ImportSelection::default(),
                "task-failed",
                None,
            )
            .await
            .expect_err("加载器库下不下来，导入应失败");

        let scan = discover_versions(petalfall.game_dir()).await.unwrap();
        let ids: Vec<&str> = scan.versions.iter().map(|v| v.id.as_str()).collect();
        assert_eq!(ids, vec!["test"]);
        assert_eq!(scan.broken, Vec::new());
    }

    /// 包里自带的 options.txt 压过默认设置，包里没有的服务器列表由默认设置补上；配置里关掉时一个都不补。
    /// 包里写着游戏默认语言的那一行在套默认设置之前就去掉了，文件还在，所以默认设置的 options.txt 照样不进来。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn imported_instance_gets_missing_files_from_the_template() {
        let server = MockServer::start().await;
        mount_vanilla(&server).await;
        let tmp = tempfile::tempdir().unwrap();
        let pack = tmp.path().join("测试包.mrpack");
        write_pack(
            &pack,
            &index(serde_json::json!({"minecraft": "test"})),
            &[("overrides/options.txt", b"guiScale:3\nlang:en_us\n")],
        );
        let petalfall = petalfall(tmp.path(), &server);
        let servers: Vec<u8> = (0..fastrand::usize(1..2048))
            .map(|_| fastrand::u8(..))
            .collect();
        let template = petalfall.template_dir();
        std::fs::create_dir_all(&template).unwrap();
        std::fs::write(template.join("options.txt"), b"fov:90").unwrap();
        std::fs::write(template.join("servers.dat"), &servers).unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        let outcome = petalfall
            .install_modpack(
                &ModpackSource::File { path: pack.clone() },
                Some("带模板"),
                &ImportSelection::default(),
                "task-t",
                Some(&tx),
            )
            .await
            .unwrap();
        drop(tx);
        let dir = outcome.version_dir;
        assert_eq!(
            std::fs::read(dir.join("options.txt")).unwrap(),
            b"guiScale:3\n"
        );
        assert_eq!(std::fs::read(dir.join("servers.dat")).unwrap(), servers);
        let mut stages = Vec::new();
        while let Some(event) = rx.recv().await {
            if let CoreEvent::Stage(stage) = event {
                stages.push(stage);
            }
        }
        assert!(
            stages
                .iter()
                .any(|s| s == "沿用默认设置：补上 servers.dat（已有的 options.txt 保留不动）"),
            "{stages:?}"
        );

        let mut off = Petalfall::for_test(
            PetalfallConfig {
                new_instance_template: false,
                ..PetalfallConfig::default()
            },
            tmp.path().join("data"),
            tmp.path().join("mc"),
        )
        .with_manifest_url(format!("{}/manifest.json", server.uri()));
        off.set_isolation_policy(IsolationPolicy::Disabled);
        let outcome = off
            .install_modpack(
                &ModpackSource::File { path: pack },
                Some("不带模板"),
                &ImportSelection::default(),
                "task-u",
                None,
            )
            .await
            .unwrap();
        let dir = outcome.version_dir;
        assert_eq!(
            std::fs::read(dir.join("options.txt")).unwrap(),
            b"guiScale:3\n"
        );
        assert!(!dir.join("servers.dat").exists());
    }

    /// 整合包里写着游戏默认语言的那一行在导入时去掉：游戏自己的 options.txt 与两个模组的默认设置一起看，其余
    /// 字节不动；写着别的语言的是作者有意选的，原样留着。关掉「游戏语言跟随系统语言」时三份文件都与包里的
    /// 逐字节相同。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn imported_pack_loses_its_default_language_lines() {
        const OPTIONS: &[u8] = b"version:3955\nlang:en_us\nfov:0.5\n";
        const DEFAULT_OPTIONS: &[u8] =
            b"version:3955\r\nfov:0.8\r\nlang:en_us\r\nonboardAccessibility:false\r\n";
        const YOSBR: &[u8] = b"lang:ja_jp\nfov:0.6\n";
        let server = MockServer::start().await;
        mount_vanilla(&server).await;
        let tmp = tempfile::tempdir().unwrap();
        let pack = tmp.path().join("测试包.mrpack");
        write_pack(
            &pack,
            &index(serde_json::json!({"minecraft": "test"})),
            &[
                ("overrides/options.txt", OPTIONS),
                (
                    "overrides/config/defaultoptions/options.txt",
                    DEFAULT_OPTIONS,
                ),
                ("overrides/config/yosbr/options.txt", YOSBR),
            ],
        );
        let read = |dir: &Path, relative: &str| std::fs::read(dir.join(relative)).unwrap();

        let outcome = petalfall(tmp.path(), &server)
            .install_modpack(
                &ModpackSource::File { path: pack.clone() },
                Some("跟随系统"),
                &ImportSelection::default(),
                "task-l",
                None,
            )
            .await
            .unwrap();
        let dir = outcome.version_dir;
        assert_eq!(read(&dir, "options.txt"), b"version:3955\nfov:0.5\n");
        assert_eq!(
            read(&dir, "config/defaultoptions/options.txt"),
            b"version:3955\r\nfov:0.8\r\nonboardAccessibility:false\r\n"
        );
        assert_eq!(read(&dir, "config/yosbr/options.txt"), YOSBR);

        let mut off = Petalfall::for_test(
            PetalfallConfig {
                follow_system_game_language: false,
                ..PetalfallConfig::default()
            },
            tmp.path().join("data-off"),
            tmp.path().join("mc-off"),
        )
        .with_manifest_url(format!("{}/manifest.json", server.uri()));
        off.set_isolation_policy(IsolationPolicy::Disabled);
        let outcome = off
            .install_modpack(
                &ModpackSource::File { path: pack },
                Some("不跟随"),
                &ImportSelection::default(),
                "task-m",
                None,
            )
            .await
            .unwrap();
        let dir = outcome.version_dir;
        assert_eq!(read(&dir, "options.txt"), OPTIONS);
        assert_eq!(
            read(&dir, "config/defaultoptions/options.txt"),
            DEFAULT_OPTIONS
        );
        assert_eq!(read(&dir, "config/yosbr/options.txt"), YOSBR);
    }

    /// 整合包带着 Default Options 的默认设置时，默认设置（模板）里的 options.txt 不补进实例：补了，模组就不再铺
    /// 作者的那份。包里没有的服务器列表照常由模板补上，进度里写明 options.txt 留给了模组。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn template_leaves_the_options_file_to_the_mod_defaults_of_the_pack() {
        const DEFAULT_OPTIONS: &[u8] = b"version:3955\nfov:0.8\nlang:zh_cn\n";
        let server = MockServer::start().await;
        mount_vanilla(&server).await;
        let tmp = tempfile::tempdir().unwrap();
        let pack = tmp.path().join("测试包.mrpack");
        write_pack(
            &pack,
            &index(serde_json::json!({"minecraft": "test"})),
            &[(
                "overrides/config/defaultoptions/options.txt",
                DEFAULT_OPTIONS,
            )],
        );
        let petalfall = petalfall(tmp.path(), &server);
        let template = petalfall.template_dir();
        std::fs::create_dir_all(&template).unwrap();
        std::fs::write(template.join("options.txt"), b"fov:90").unwrap();
        std::fs::write(template.join("servers.dat"), b"mine").unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        let outcome = petalfall
            .install_modpack(
                &ModpackSource::File { path: pack },
                Some("带模组默认设置"),
                &ImportSelection::default(),
                "task-d",
                Some(&tx),
            )
            .await
            .unwrap();
        drop(tx);

        let dir = outcome.version_dir;
        assert!(!dir.join("options.txt").exists());
        assert_eq!(
            std::fs::read(dir.join("config/defaultoptions/options.txt")).unwrap(),
            DEFAULT_OPTIONS
        );
        assert_eq!(std::fs::read(dir.join("servers.dat")).unwrap(), b"mine");
        let mut stages = Vec::new();
        while let Some(event) = rx.recv().await {
            if let CoreEvent::Stage(stage) = event {
                stages.push(stage);
            }
        }
        assert!(
            stages
                .iter()
                .any(|s| s
                    == "沿用默认设置：补上 servers.dat（options.txt 留给整合包里模组的默认设置）"),
            "{stages:?}"
        );
    }

    /// 模板里只有 options.txt、又整份留给了模组时，一个文件都没补进来。这时进度里照样要说，不然玩家看不出
    /// 默认设置为什么没生效。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn template_left_entirely_to_the_mods_is_still_announced() {
        let server = MockServer::start().await;
        mount_vanilla(&server).await;
        let tmp = tempfile::tempdir().unwrap();
        let pack = tmp.path().join("测试包.mrpack");
        write_pack(
            &pack,
            &index(serde_json::json!({"minecraft": "test"})),
            &[("overrides/config/yosbr/options.txt", b"fov:0.8\n")],
        );
        let petalfall = petalfall(tmp.path(), &server);
        let template = petalfall.template_dir();
        std::fs::create_dir_all(&template).unwrap();
        std::fs::write(template.join("options.txt"), b"fov:90").unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        let outcome = petalfall
            .install_modpack(
                &ModpackSource::File { path: pack },
                Some("只留给模组"),
                &ImportSelection::default(),
                "task-e",
                Some(&tx),
            )
            .await
            .unwrap();
        drop(tx);

        assert!(!outcome.version_dir.join("options.txt").exists());
        let mut stages = Vec::new();
        while let Some(event) = rx.recv().await {
            if let CoreEvent::Stage(stage) = event {
                stages.push(stage);
            }
        }
        assert!(
            stages
                .iter()
                .any(|s| s == "沿用默认设置：options.txt 留给整合包里模组的默认设置"),
            "{stages:?}"
        );
    }

    /// 整合包带着 YOSBR 自动建的那份空 options.txt 时，它不是作者的设置：模板的 options.txt 照常补进实例，
    /// 空文件原样留着。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn blank_mod_defaults_in_the_pack_do_not_hold_the_template_back() {
        let server = MockServer::start().await;
        mount_vanilla(&server).await;
        let tmp = tempfile::tempdir().unwrap();
        let pack = tmp.path().join("测试包.mrpack");
        write_pack(
            &pack,
            &index(serde_json::json!({"minecraft": "test"})),
            &[("overrides/config/yosbr/options.txt", b"")],
        );
        let petalfall = petalfall(tmp.path(), &server);
        let template = petalfall.template_dir();
        std::fs::create_dir_all(&template).unwrap();
        std::fs::write(template.join("options.txt"), b"fov:90").unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        let outcome = petalfall
            .install_modpack(
                &ModpackSource::File { path: pack },
                Some("带空占位"),
                &ImportSelection::default(),
                "task-f",
                Some(&tx),
            )
            .await
            .unwrap();
        drop(tx);

        let dir = outcome.version_dir;
        assert_eq!(std::fs::read(dir.join("options.txt")).unwrap(), b"fov:90");
        assert_eq!(
            std::fs::read(dir.join("config/yosbr/options.txt")).unwrap(),
            b""
        );
        let mut stages = Vec::new();
        while let Some(event) = rx.recv().await {
            if let CoreEvent::Stage(stage) = event {
                stages.push(stage);
            }
        }
        assert!(
            stages.iter().any(|s| s == "沿用默认设置：补上 options.txt"),
            "{stages:?}"
        );
    }

    /// 模板的文件实例里都已经有、一个没补也没有留给模组的时不发这条进度：包里自带的设置原样留着本来就是
    /// 常态，不用每次导入都说一遍。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn template_with_nothing_to_add_is_not_announced() {
        let server = MockServer::start().await;
        mount_vanilla(&server).await;
        let tmp = tempfile::tempdir().unwrap();
        let pack = tmp.path().join("测试包.mrpack");
        write_pack(
            &pack,
            &index(serde_json::json!({"minecraft": "test"})),
            &[("overrides/options.txt", b"fov:0.8\n")],
        );
        let petalfall = petalfall(tmp.path(), &server);
        let template = petalfall.template_dir();
        std::fs::create_dir_all(&template).unwrap();
        std::fs::write(template.join("options.txt"), b"fov:90").unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        let outcome = petalfall
            .install_modpack(
                &ModpackSource::File { path: pack },
                Some("自带设置"),
                &ImportSelection::default(),
                "task-h",
                Some(&tx),
            )
            .await
            .unwrap();
        drop(tx);

        assert_eq!(
            std::fs::read(outcome.version_dir.join("options.txt")).unwrap(),
            b"fov:0.8\n"
        );
        let mut stages = Vec::new();
        while let Some(event) = rx.recv().await {
            if let CoreEvent::Stage(stage) = event {
                stages.push(stage);
            }
        }
        assert!(
            !stages.iter().any(|s| s.contains("沿用默认设置")),
            "{stages:?}"
        );
    }

    /// 整合包自带的 options.txt 里只有一行游戏默认语言：去掉后这份文件整个删掉，不留一个空文件冒充包里自带的
    /// 设置，模板的 options.txt 照常补进实例；没有模板时实例里就没有 options.txt，留给启动前的补全去建。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_options_file_holding_only_the_default_language_does_not_hold_the_template_back() {
        let server = MockServer::start().await;
        mount_vanilla(&server).await;
        let tmp = tempfile::tempdir().unwrap();
        let pack = tmp.path().join("测试包.mrpack");
        write_pack(
            &pack,
            &index(serde_json::json!({"minecraft": "test"})),
            &[("overrides/options.txt", b"lang:en_us\n")],
        );
        let petalfall = petalfall(tmp.path(), &server);

        let bare = petalfall
            .install_modpack(
                &ModpackSource::File { path: pack.clone() },
                Some("没有模板"),
                &ImportSelection::default(),
                "task-i",
                None,
            )
            .await
            .unwrap();
        assert!(!bare.version_dir.join("options.txt").exists());

        let template = petalfall.template_dir();
        std::fs::create_dir_all(&template).unwrap();
        std::fs::write(template.join("options.txt"), b"fov:90").unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let outcome = petalfall
            .install_modpack(
                &ModpackSource::File { path: pack },
                Some("有模板"),
                &ImportSelection::default(),
                "task-j",
                Some(&tx),
            )
            .await
            .unwrap();
        drop(tx);

        assert_eq!(
            std::fs::read(outcome.version_dir.join("options.txt")).unwrap(),
            b"fov:90"
        );
        let mut stages = Vec::new();
        while let Some(event) = rx.recv().await {
            if let CoreEvent::Stage(stage) = event {
                stages.push(stage);
            }
        }
        assert!(
            stages.iter().any(|s| s == "沿用默认设置：补上 options.txt"),
            "{stages:?}"
        );
    }

    /// 整合包自带的 options.txt 本来就是空的也一样删掉，与语言开关无关：关掉「游戏语言跟随系统语言」时
    /// 模板照样补得进来；写着默认语言的那份这时不去行，原样留着。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_blank_options_file_in_the_pack_is_removed_whatever_the_language_switch() {
        let server = MockServer::start().await;
        mount_vanilla(&server).await;
        let tmp = tempfile::tempdir().unwrap();
        let blank_pack = tmp.path().join("空设置.mrpack");
        write_pack(
            &blank_pack,
            &index(serde_json::json!({"minecraft": "test"})),
            &[("overrides/options.txt", b"\r\n \r\n")],
        );
        let english_pack = tmp.path().join("英文设置.mrpack");
        write_pack(
            &english_pack,
            &index(serde_json::json!({"minecraft": "test"})),
            &[("overrides/options.txt", b"lang:en_us\n")],
        );
        let mut off = Petalfall::for_test(
            PetalfallConfig {
                follow_system_game_language: false,
                ..PetalfallConfig::default()
            },
            tmp.path().join("data"),
            tmp.path().join("mc"),
        )
        .with_manifest_url(format!("{}/manifest.json", server.uri()));
        off.set_isolation_policy(IsolationPolicy::Disabled);
        let template = off.template_dir();
        std::fs::create_dir_all(&template).unwrap();
        std::fs::write(template.join("options.txt"), b"fov:90").unwrap();

        let blank = off
            .install_modpack(
                &ModpackSource::File { path: blank_pack },
                Some("空设置"),
                &ImportSelection::default(),
                "task-k",
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            std::fs::read(blank.version_dir.join("options.txt")).unwrap(),
            b"fov:90"
        );

        let english = off
            .install_modpack(
                &ModpackSource::File { path: english_pack },
                Some("英文设置"),
                &ImportSelection::default(),
                "task-l",
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            std::fs::read(english.version_dir.join("options.txt")).unwrap(),
            b"lang:en_us\n"
        );
    }

    /// 编辑器存成 UTF-8 时会在开头带上 BOM：整合包自带的 options.txt 只有它（或它加空白）时同样是空的，导入时
    /// 删掉；BOM 后面有内容的原样留着。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_options_file_holding_only_a_bom_is_removed_on_import() {
        let server = MockServer::start().await;
        mount_vanilla(&server).await;
        let tmp = tempfile::tempdir().unwrap();
        let petalfall = petalfall(tmp.path(), &server);
        let cases: [(&str, &[u8], bool); 3] = [
            ("BOM", b"\xef\xbb\xbf", false),
            ("BOM-blank", b"\xef\xbb\xbf\r\n \r\n", false),
            ("BOM-fov", b"\xef\xbb\xbffov:0.8\n", true),
        ];
        for (name, pack_options, kept) in cases {
            let pack = tmp.path().join(format!("{name}.mrpack"));
            write_pack(
                &pack,
                &index(serde_json::json!({"minecraft": "test"})),
                &[("overrides/options.txt", pack_options)],
            );

            let outcome = petalfall
                .install_modpack(
                    &ModpackSource::File { path: pack },
                    Some(name),
                    &ImportSelection::default(),
                    &format!("task-{name}"),
                    None,
                )
                .await
                .unwrap();

            let written = match std::fs::read(outcome.version_dir.join("options.txt")) {
                Ok(bytes) => Some(bytes),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
                Err(err) => panic!("读 {name} 的 options.txt 失败：{err}"),
            };
            assert_eq!(written.as_deref(), kept.then_some(pack_options), "{name}");
        }
    }

    /// 整合包的模组默认设置里只有一行游戏默认语言：那一行在套模板之前就去掉了，剩下的空文件不挡模板。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn mod_defaults_emptied_of_the_default_language_do_not_hold_the_template_back() {
        let server = MockServer::start().await;
        mount_vanilla(&server).await;
        let tmp = tempfile::tempdir().unwrap();
        let pack = tmp.path().join("测试包.mrpack");
        write_pack(
            &pack,
            &index(serde_json::json!({"minecraft": "test"})),
            &[("overrides/config/yosbr/options.txt", b"lang:en_us\n")],
        );
        let petalfall = petalfall(tmp.path(), &server);
        let template = petalfall.template_dir();
        std::fs::create_dir_all(&template).unwrap();
        std::fs::write(template.join("options.txt"), b"fov:90").unwrap();

        let outcome = petalfall
            .install_modpack(
                &ModpackSource::File { path: pack },
                Some("只有默认语言"),
                &ImportSelection::default(),
                "task-g",
                None,
            )
            .await
            .unwrap();

        let dir = outcome.version_dir;
        assert_eq!(std::fs::read(dir.join("options.txt")).unwrap(), b"fov:90");
        assert_eq!(
            std::fs::read(dir.join("config/yosbr/options.txt")).unwrap(),
            b""
        );
    }

    /// 去掉默认语言行只针对整合包带来的文件：默认设置里的 options.txt 是玩家自己存下的，补进实例时连同里面的
    /// 语言原样进来。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn language_in_the_template_survives_the_import() {
        const TEMPLATE: &[u8] = b"lang:en_us\nfov:0.9\n";
        let server = MockServer::start().await;
        mount_vanilla(&server).await;
        let tmp = tempfile::tempdir().unwrap();
        let pack = tmp.path().join("测试包.mrpack");
        write_pack(
            &pack,
            &index(serde_json::json!({"minecraft": "test"})),
            &[("overrides/config/a.txt", b"config-a")],
        );
        let petalfall = petalfall(tmp.path(), &server);
        let template = petalfall.template_dir();
        std::fs::create_dir_all(&template).unwrap();
        std::fs::write(template.join("options.txt"), TEMPLATE).unwrap();

        let outcome = petalfall
            .install_modpack(
                &ModpackSource::File { path: pack },
                Some("带模板"),
                &ImportSelection::default(),
                "task-n",
                None,
            )
            .await
            .unwrap();

        assert_eq!(
            std::fs::read(outcome.version_dir.join("options.txt")).unwrap(),
            TEMPLATE
        );
    }

    /// 同名旧实例在启动器之外被删掉后，名字下的游玩记录还留着；导入同名整合包得到的是一个没玩过的新实例。
    /// 撞上现有实例的导入被拦下，不动那个实例的记录。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn imported_instance_does_not_inherit_records_of_a_vanished_namesake() {
        let server = MockServer::start().await;
        mount_vanilla(&server).await;
        let tmp = tempfile::tempdir().unwrap();
        let pack = tmp.path().join("RLCraft.mrpack");
        write_pack(&pack, &index(serde_json::json!({"minecraft": "test"})), &[]);
        let petalfall = petalfall(tmp.path(), &server);
        let hour = 3_600_000;
        for id in ["RLCraft", "现有", "别的"] {
            crate::playtime::record_play(
                petalfall.data_dir(),
                petalfall.game_dir(),
                id,
                0,
                10 * hour,
            )
            .await
            .unwrap();
        }
        std::fs::create_dir_all(petalfall.game_dir().join("versions").join("现有")).unwrap();
        let source = ModpackSource::File { path: pack.clone() };

        petalfall
            .install_modpack(
                &source,
                Some("现有"),
                &ImportSelection::default(),
                "task-dup",
                None,
            )
            .await
            .unwrap_err();
        petalfall
            .install_modpack(
                &source,
                Some("RLCraft"),
                &ImportSelection::default(),
                "task-1",
                None,
            )
            .await
            .unwrap();

        let summaries = petalfall.play_summaries().await.unwrap();
        assert_eq!(summaries.keys().collect::<Vec<_>>(), vec!["别的", "现有"]);
        assert_eq!(summaries["现有"].playtime_min, 600);
    }

    /// Fabric 整合包：实例版本 JSON 是加载器 JSON 的副本（id 换成实例名，仍继承原版），被识别为带加载器。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fabric_pack_copies_the_loader_json_under_the_instance_name() {
        let server = MockServer::start().await;
        mount_vanilla(&server).await;
        Mock::given(method("GET"))
            .and(path("/v2/versions/loader/test/0.15.0/profile/json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"id":"fabric-loader-0.15.0-test","inheritsFrom":"test","type":"release",
                    "mainClass":"net.fabricmc.loader.impl.launch.knot.KnotClient",
                    "arguments":{"game":[],"jvm":["-DFabricMcEmu= net.minecraft.client.main.Main "]},
                    "libraries":[]}"#,
            ))
            .mount(&server)
            .await;
        let tmp = tempfile::tempdir().unwrap();
        let pack = tmp.path().join("fabric.mrpack");
        write_pack(
            &pack,
            &index(serde_json::json!({"minecraft": "test", "fabric-loader": "0.15.0"})),
            &[],
        );
        let petalfall = petalfall(tmp.path(), &server);

        let prepared = petalfall
            .prepare_modpack(
                &ModpackSource::File { path: pack },
                None,
                &ImportSelection::default(),
                None,
            )
            .await
            .unwrap();
        // 导入前预告的目录就是导入实际写的：实例 JSON 继承前者、抄自后者，调用方据此在导入期间占住它们。
        let natural = prepared.natural_versions();
        assert_eq!(natural, ["test", "fabric-loader-0.15.0-test"]);
        let base = natural[0].clone();
        let outcome = petalfall
            .import_modpack(prepared, "task-f", None)
            .await
            .unwrap();
        assert_eq!(outcome.version_id, "测试包");
        assert!(
            petalfall
                .game_dir()
                .join(VERSIONS_DIR)
                .join(&natural[1])
                .join(format!("{}.json", natural[1]))
                .is_file()
        );
        let dir = outcome.version_dir;
        let json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("测试包.json")).unwrap()).unwrap();
        assert_eq!(json["id"], "测试包");
        assert_eq!(json["inheritsFrom"], base.as_str());
        assert_eq!(
            petalfall.inheritance_chain("测试包").await.unwrap(),
            vec![base.clone()]
        );
        assert_eq!(
            json["mainClass"],
            "net.fabricmc.loader.impl.launch.knot.KnotClient"
        );
        assert_eq!(
            json["arguments"]["jvm"][0],
            "-DFabricMcEmu= net.minecraft.client.main.Main "
        );
        assert_eq!(
            outcome.modpack.loader,
            Some(PackLoader {
                kind: PackLoaderKind::Fabric,
                version: "0.15.0".into()
            })
        );
        let scan = discover_versions(petalfall.game_dir()).await.unwrap();
        let found = scan.versions.iter().find(|v| v.id == "测试包").unwrap();
        assert!(found.has_mod_loader(), "加载器应能从实例 JSON 识别出来");
    }

    /// 没有加载器的包只写原版；加载器版本与安装时用的是同一个写法，清单里带 MC 前缀的先去掉。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn natural_versions_follow_the_pack_manifest() {
        let server = MockServer::start().await;
        let tmp = tempfile::tempdir().unwrap();
        let petalfall = petalfall(tmp.path(), &server);
        let pack = tmp.path().join("pack.mrpack");
        for (dependencies, natural) in [
            (serde_json::json!({"minecraft": "1.21"}), vec!["1.21"]),
            (
                serde_json::json!({"minecraft": "1.21", "quilt-loader": "0.26.0"}),
                vec!["1.21", "quilt-loader-0.26.0-1.21"],
            ),
            (
                serde_json::json!({"minecraft": "1.20.1", "forge": "1.20.1-47.2.0"}),
                vec!["1.20.1", "1.20.1-forge-47.2.0"],
            ),
            (
                serde_json::json!({"minecraft": "1.21.1", "neoforge": "21.1.42"}),
                vec!["1.21.1", "neoforge-21.1.42"],
            ),
        ] {
            write_pack(&pack, &index(dependencies), &[]);
            let prepared = petalfall
                .prepare_modpack(
                    &ModpackSource::File { path: pack.clone() },
                    None,
                    &ImportSelection::default(),
                    None,
                )
                .await
                .unwrap();
            assert_eq!(prepared.natural_versions(), natural);
        }
    }

    const NATURAL_FABRIC: &str = "fabric-loader-0.15.0-test";

    fn fabric_profile(library_base: &str) -> String {
        format!(
            r#"{{"id":"{NATURAL_FABRIC}","inheritsFrom":"test","type":"release",
                "mainClass":"net.fabricmc.loader.impl.launch.knot.KnotClient",
                "libraries":[{{"name":"net.fabricmc:fabric-loader:0.15.0","url":"{library_base}/"}}]}}"#
        )
    }

    /// 让 mock 提供 Fabric 0.15.0：profile 与它的 loader jar。
    async fn mount_fabric(server: &MockServer) {
        Mock::given(method("GET"))
            .and(path("/v2/versions/loader/test/0.15.0/profile/json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(fabric_profile(&server.uri())))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path(
                "/net/fabricmc/fabric-loader/0.15.0/fabric-loader-0.15.0.jar",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"loader".to_vec()))
            .mount(server)
            .await;
    }

    fn fabric_pack(root: &Path) -> PathBuf {
        let pack = root.join("fabric.mrpack");
        write_pack(
            &pack,
            &index(serde_json::json!({"minecraft": "test", "fabric-loader": "0.15.0"})),
            &[],
        );
        pack
    }

    async fn import_as(
        petalfall: &Petalfall,
        pack: PathBuf,
        name: &str,
    ) -> Result<ModpackInstallOutcome> {
        petalfall
            .install_modpack(
                &ModpackSource::File { path: pack },
                Some(name),
                &ImportSelection::default(),
                "task-natural",
                None,
            )
            .await
    }

    /// `versions/` 下现有的目录名，按名字排好。
    fn listed_dirs(petalfall: &Petalfall) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(petalfall.game_dir().join("versions"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    async fn listed_ids(petalfall: &Petalfall) -> Vec<String> {
        let list = petalfall.list_instances().await.unwrap();
        list.into_iter().map(|instance| instance.id).collect()
    }

    fn write_game_version(petalfall: &Petalfall, id: &str, json: &str) {
        let dir = petalfall.game_dir().join("versions").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{id}.json")), json).unwrap();
    }

    /// 玩家导入一个整合包，实例列表就只多这一个实例。原版与加载器版本都落在自己的版本目录里：目录留着，不当实例
    /// 列出，最后一个用它们的整合包删掉后也不回来；重名检查照常按目录算。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn imported_pack_adds_one_instance_and_no_base_version_to_the_list() {
        let server = MockServer::start().await;
        mount_vanilla(&server).await;
        mount_fabric(&server).await;
        let tmp = tempfile::tempdir().unwrap();
        let pack = fabric_pack(tmp.path());
        let petalfall = petalfall(tmp.path(), &server);

        let outcome = import_as(&petalfall, pack, "周末整合包").await.unwrap();

        assert_eq!(listed_ids(&petalfall).await, ["周末整合包"]);
        for base in ["test", NATURAL_FABRIC] {
            let json = petalfall
                .game_dir()
                .join("versions")
                .join(base)
                .join(format!("{base}.json"));
            assert!(json.is_file(), "{base} 的目录不删");
        }

        std::fs::remove_dir_all(&outcome.version_dir).unwrap();

        assert!(listed_ids(&petalfall).await.is_empty());
        let err = petalfall.check_new_instance_name("test").await.unwrap_err();
        assert!(
            matches!(
                err,
                CoreError::Instance(crate::InstanceError::InvalidName {
                    violation: NameViolation::Duplicate,
                    ..
                })
            ),
            "{err:?}"
        );
    }

    /// 别的启动器的游戏目录里已有同款原版与加载器版本：导入用上它们，它们仍是玩家的，导入后、整合包删掉后都列着。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn versions_already_on_disk_stay_listed_after_a_pack_uses_them() {
        let server = MockServer::start().await;
        mount_vanilla(&server).await;
        mount_fabric(&server).await;
        let tmp = tempfile::tempdir().unwrap();
        let pack = fabric_pack(tmp.path());
        let petalfall = petalfall(tmp.path(), &server);
        write_game_version(
            &petalfall,
            "test",
            r#"{"id":"test","type":"release","mainClass":"net.minecraft.client.main.Main"}"#,
        );
        write_game_version(
            &petalfall,
            NATURAL_FABRIC,
            &fabric_profile("https://example.invalid"),
        );

        let outcome = import_as(&petalfall, pack, "周末整合包").await.unwrap();

        assert_eq!(
            listed_ids(&petalfall).await,
            [NATURAL_FABRIC, "test", "周末整合包"]
        );
        std::fs::remove_dir_all(&outcome.version_dir).unwrap();
        assert_eq!(listed_ids(&petalfall).await, [NATURAL_FABRIC, "test"]);
    }

    /// 一个装着 Fabric 的版本 JSON，id 由调用方定：玩家给装了加载器的实例起的名字。
    fn fabric_instance_json(id: &str) -> String {
        format!(
            r#"{{"id":"{id}","type":"release","mainClass":"net.fabricmc.loader.impl.launch.knot.KnotClient",
                "libraries":[{{"name":"net.fabricmc:fabric-loader:0.14.0"}}]}}"#
        )
    }

    /// 玩家有一个装了 Fabric 的实例，名字正好是游戏版本号（别的启动器里这是常见的默认名）。整合包要把原版装进
    /// 这个名字的目录，照装会把那个实例的版本 JSON 换成原版，它的加载器就没了。导入在准备阶段就拒收，说明是哪个
    /// 版本、它是什么、该怎么办；那个实例的版本 JSON 一个字节不变，新实例的目录也不建。继承着别的版本的同名目录
    /// 同样不是原版，同样拒收。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_pack_whose_game_version_names_a_modded_instance_is_refused() {
        let server = MockServer::start().await;
        mount_vanilla(&server).await;
        let tmp = tempfile::tempdir().unwrap();
        let petalfall = petalfall(tmp.path(), &server);
        let pack = tmp.path().join("pack.mrpack");
        write_pack(&pack, &index(serde_json::json!({"minecraft": "test"})), &[]);
        let versions = petalfall.game_dir().join("versions");
        let theirs = versions.join("test").join("test.json");

        for (json, found) in [
            (fabric_instance_json("test"), "装着 Fabric"),
            (
                r#"{"id":"test","inheritsFrom":"1.20.1","mainClass":"net.minecraft.client.main.Main"}"#
                    .to_owned(),
                "继承自 1.20.1",
            ),
        ] {
            write_game_version(&petalfall, "test", &json);

            let err = import_as(&petalfall, pack.clone(), "周末包")
                .await
                .unwrap_err();

            assert!(
                matches!(&err, CoreError::BaseVersionOccupied { id, .. } if id == "test"),
                "{err:?}"
            );
            assert_eq!(
                err.to_string(),
                format!(
                    "游戏目录里已有一个叫「test」的版本，它{found}，不是这个整合包要用的原版 test。导入会把它的版本文件换掉，先给它改个名字再导入"
                )
            );
            assert_eq!(std::fs::read_to_string(&theirs).unwrap(), json);
            assert!(!versions.join("周末包").exists());
        }

        // 那个装着 Fabric 的实例还被别的版本直接继承着：改名要求没人继承它，照着「改个名字」做不下去。报文点出
        // 是谁继承着它，出路写全：先把它们移走或删掉，再给它改名。
        let json = fabric_instance_json("test");
        write_game_version(&petalfall, "test", &json);
        let stacked = ["叠加一", "叠加二"];
        for id in stacked {
            write_game_version(
                &petalfall,
                id,
                &format!(r#"{{"id":"{id}","inheritsFrom":"test","mainClass":"M"}}"#),
            );
        }
        let err = import_as(&petalfall, pack.clone(), "周末包")
            .await
            .unwrap_err();
        let CoreError::BaseVersionOccupied {
            id,
            inherited_by,
            claimed,
            ..
        } = &err
        else {
            panic!("{err:?}");
        };
        assert_eq!(id, "test");
        assert_eq!(inherited_by, &stacked);
        assert!(*claimed);
        assert_eq!(
            err.to_string(),
            "游戏目录里已有一个叫「test」的版本，它装着 Fabric，不是这个整合包要用的原版 test。它被「叠加一」、「叠加二」继承着，导入会把它的版本文件换掉；要导入这个整合包，得先把继承它的版本移走或删掉，再给它改个名字"
        );
        assert_eq!(std::fs::read_to_string(&theirs).unwrap(), json);
        assert!(!versions.join("周末包").exists());
        for id in stacked {
            std::fs::remove_dir_all(versions.join(id)).unwrap();
        }
        // 照报文做完第一步（继承它的版本没了）再导入还是拒收，这回说的是改名：所以上面那句把两步一起说了。
        let err = import_as(&petalfall, pack.clone(), "周末包")
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "游戏目录里已有一个叫「test」的版本，它装着 Fabric，不是这个整合包要用的原版 test。导入会把它的版本文件换掉，先给它改个名字再导入"
        );
        assert_eq!(std::fs::read_to_string(&theirs).unwrap(), json);
        assert!(!versions.join("周末包").exists());

        // 同一个目录要是落樱自己落下的（记在依赖版本记录里、没人在里面玩过），装着什么都照常还原成原版；
        // 原版的位置上实例本来就直接继承它，有人继承也不拦。
        write_game_version(&petalfall, "test", &fabric_instance_json("test"));
        write_game_version(
            &petalfall,
            "继承原版的",
            r#"{"id":"继承原版的","inheritsFrom":"test","mainClass":"M"}"#,
        );
        petalfall
            .record_base_versions(&["test".to_owned()])
            .await
            .unwrap();
        import_as(&petalfall, pack, "周末包").await.unwrap();
        let restored = std::fs::read_to_string(&theirs).unwrap();
        assert!(!restored.contains("fabric-loader"), "{restored}");
        assert!(versions.join("周末包").is_dir());
    }

    /// 继承着被占的版本的有一长串时，报文只点前三个、写明总数；正好三个时全点出来，不写总数。目录是玩家的，
    /// 出路说到改名为止；是启动器落下的，说到把继承它的版本移走或删掉为止。
    #[test]
    fn a_long_list_of_versions_inheriting_an_occupied_base_is_cut_to_three_and_counted() {
        let message = |inherited_by: &[&str], claimed: bool| {
            CoreError::BaseVersionOccupied {
                id: "1.20.1".to_owned(),
                wanted: "原版 1.20.1".to_owned(),
                found: "装着 Forge".to_owned(),
                inherited_by: inherited_by.iter().map(|id| (*id).to_owned()).collect(),
                claimed,
            }
            .to_string()
        };
        let head =
            "游戏目录里已有一个叫「1.20.1」的版本，它装着 Forge，不是这个整合包要用的原版 1.20.1。";
        let tail =
            "继承着，导入会把它的版本文件换掉；要导入这个整合包，得先把继承它的版本移走或删掉";
        let rename = "，再给它改个名字";

        assert_eq!(
            message(&["甲", "乙", "丙"], true),
            format!("{head}它被「甲」、「乙」、「丙」{tail}{rename}")
        );
        assert_eq!(
            message(&["甲", "乙", "丙", "丁"], true),
            format!("{head}它被「甲」、「乙」、「丙」等 4 个版本{tail}{rename}")
        );
        let many: Vec<String> = (1..=12).map(|n| format!("叠加{n}")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        assert_eq!(
            message(&many, true),
            format!("{head}它被「叠加1」、「叠加2」、「叠加3」等 12 个版本{tail}{rename}")
        );
        assert_eq!(
            message(&["甲", "乙", "丙", "丁"], false),
            format!("{head}它被「甲」、「乙」、「丙」等 4 个版本{tail}")
        );
        assert_eq!(
            message(&[], true),
            format!("{head}导入会把它的版本文件换掉，先给它改个名字再导入")
        );
    }

    /// 加载器的位置同理：整合包要的加载器版本目录名下认得出是别的东西时拒收，装着另一种加载器、或除了要的那种
    /// 还多装了别的都算。一个加载器都探不出来的版本 JSON 说不清是什么，照常重装。那个目录要是落樱自己上次导入时
    /// 落下的、玩家没拿它当实例用，装着什么都照常重装：导入出的实例各有一份版本 JSON 的副本，换掉它不影响它们。
    /// 有别的版本直接继承着它的、玩家在里面玩过的，照样拒收。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_loader_version_directory_holding_something_else_is_refused_unless_the_launcher_put_it_there()
     {
        let server = MockServer::start().await;
        mount_vanilla(&server).await;
        mount_fabric(&server).await;
        let tmp = tempfile::tempdir().unwrap();
        let pack = fabric_pack(tmp.path());
        let petalfall = petalfall(tmp.path(), &server);
        let theirs = petalfall
            .game_dir()
            .join("versions")
            .join(NATURAL_FABRIC)
            .join(format!("{NATURAL_FABRIC}.json"));
        let main_class = || -> String {
            let landed: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&theirs).unwrap()).unwrap();
            landed["mainClass"].as_str().unwrap().to_owned()
        };
        const KNOT: &str = "net.fabricmc.loader.impl.launch.knot.KnotClient";
        let quilt = format!(
            r#"{{"id":"{NATURAL_FABRIC}","inheritsFrom":"test","mainClass":"org.quiltmc.loader.impl.launch.knot.KnotClient",
                "libraries":[{{"name":"org.quiltmc:quilt-loader:0.26.0"}}]}}"#
        );
        let fabric_with_optifine = format!(
            r#"{{"id":"{NATURAL_FABRIC}","inheritsFrom":"test","mainClass":"{KNOT}",
                "libraries":[{{"name":"net.fabricmc:fabric-loader:0.15.0"}},{{"name":"optifine:OptiFine:1.20.1_HD_U_I6"}}]}}"#
        );

        for (json, found) in [
            (&quilt, "装着 Quilt"),
            (&fabric_with_optifine, "除了 Fabric 还装着 OptiFine"),
        ] {
            write_game_version(&petalfall, NATURAL_FABRIC, json);
            let err = import_as(&petalfall, pack.clone(), "周末包")
                .await
                .unwrap_err();
            assert_eq!(
                err.to_string(),
                format!(
                    "游戏目录里已有一个叫「{NATURAL_FABRIC}」的版本，它{found}，不是这个整合包要用的加载器 Fabric 0.15.0。导入会把它的版本文件换掉，先给它改个名字再导入"
                )
            );
            assert_eq!(&std::fs::read_to_string(&theirs).unwrap(), json);
            // 拒收在装任何东西之前：原版没落下，实例目录也没建。
            assert_eq!(listed_dirs(&petalfall), [NATURAL_FABRIC]);
        }

        write_game_version(
            &petalfall,
            NATURAL_FABRIC,
            &format!(r#"{{"id":"{NATURAL_FABRIC}","inheritsFrom":"test","mainClass":"M"}}"#),
        );
        import_as(&petalfall, pack.clone(), "认不出的照装")
            .await
            .unwrap();
        assert_eq!(main_class(), KNOT);

        write_game_version(&petalfall, NATURAL_FABRIC, &quilt);
        petalfall
            .record_base_versions(&[NATURAL_FABRIC.to_owned()])
            .await
            .unwrap();
        import_as(&petalfall, pack.clone(), "落樱落下的照装")
            .await
            .unwrap();
        assert_eq!(main_class(), KNOT);

        // 记录里有它、没人在里面玩过，却有别的版本直接继承它（别的启动器建的叠加版本，或移动实例时连基底一起
        // 带过来的）：换掉它就换了那个版本的底，照样拒收。落樱自己导入的实例拿的是副本，不继承它。
        write_game_version(&petalfall, NATURAL_FABRIC, &quilt);
        write_game_version(
            &petalfall,
            "叠在上面的",
            &format!(r#"{{"id":"叠在上面的","inheritsFrom":"{NATURAL_FABRIC}","mainClass":"M"}}"#),
        );
        let err = import_as(&petalfall, pack.clone(), "有人继承的不动")
            .await
            .unwrap_err();
        assert!(
            matches!(&err, CoreError::BaseVersionOccupied { id, found, inherited_by, claimed, .. }
                if id == NATURAL_FABRIC && found == "装着 Quilt" && inherited_by == &["叠在上面的"] && !claimed),
            "{err:?}"
        );
        // 它是落樱落下的，不列在实例库里，玩家改不了它的名：出路只说到把继承它的版本移走或删掉。
        assert_eq!(
            err.to_string(),
            format!(
                "游戏目录里已有一个叫「{NATURAL_FABRIC}」的版本，它装着 Quilt，不是这个整合包要用的加载器 Fabric 0.15.0。它被「叠在上面的」继承着，导入会把它的版本文件换掉；要导入这个整合包，得先把继承它的版本移走或删掉"
            )
        );
        assert_eq!(std::fs::read_to_string(&theirs).unwrap(), quilt);
        // 照报文做完再导入就成：没人继承了，落樱落下的照常重装。
        let stacked = petalfall.game_dir().join("versions").join("叠在上面的");
        std::fs::remove_dir_all(&stacked).unwrap();
        import_as(&petalfall, pack.clone(), "继承者走了照装")
            .await
            .unwrap();
        assert_eq!(main_class(), KNOT);

        // 记录里有它，可玩家拿它当实例玩过（里面有存档）：按玩家的算，照样拒收。
        write_game_version(&petalfall, NATURAL_FABRIC, &quilt);
        let save = theirs.parent().unwrap().join("saves").join("世界");
        std::fs::create_dir_all(&save).unwrap();
        std::fs::write(save.join("level.dat"), b"level").unwrap();
        let err = import_as(&petalfall, pack.clone(), "玩过的不动")
            .await
            .unwrap_err();
        assert!(
            matches!(&err, CoreError::BaseVersionOccupied { id, found, inherited_by, claimed, .. }
                if id == NATURAL_FABRIC && found == "装着 Quilt" && inherited_by.is_empty() && *claimed),
            "{err:?}"
        );
        assert_eq!(std::fs::read_to_string(&theirs).unwrap(), quilt);

        // 玩家的目录又被别的版本继承着：要两步，报文一起说。
        write_game_version(
            &petalfall,
            "叠在上面的",
            &format!(r#"{{"id":"叠在上面的","inheritsFrom":"{NATURAL_FABRIC}","mainClass":"M"}}"#),
        );
        let err = import_as(&petalfall, pack, "玩过又被继承的不动")
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                "游戏目录里已有一个叫「{NATURAL_FABRIC}」的版本，它装着 Quilt，不是这个整合包要用的加载器 Fabric 0.15.0。它被「叠在上面的」继承着，导入会把它的版本文件换掉；要导入这个整合包，得先把继承它的版本移走或删掉，再给它改个名字"
            )
        );
        assert_eq!(std::fs::read_to_string(&theirs).unwrap(), quilt);
    }

    /// 准备阶段查过之后、落盘之前，自然版本目录下冒出了一个别的版本（准备还要解析远程文件，这期间别的命令把
    /// 一个装着加载器的实例改名或移到了这个名字下）：落盘一步建实例目录之前再查一遍，照样拒收，那个版本的
    /// JSON 一个字节不变，实例目录不建，建目录的记号不置位。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_base_version_taken_between_preparing_and_importing_is_still_refused() {
        let server = MockServer::start().await;
        mount_vanilla(&server).await;
        let tmp = tempfile::tempdir().unwrap();
        let petalfall = petalfall(tmp.path(), &server);
        let pack = tmp.path().join("pack.mrpack");
        write_pack(&pack, &index(serde_json::json!({"minecraft": "test"})), &[]);
        let versions = petalfall.game_dir().join("versions");

        let prepared = petalfall
            .prepare_modpack(
                &ModpackSource::File { path: pack },
                Some("周末包"),
                &ImportSelection::default(),
                None,
            )
            .await
            .unwrap();
        let created = prepared.dir_created();
        let json = fabric_instance_json("test");
        write_game_version(&petalfall, "test", &json);

        let err = petalfall
            .import_modpack(prepared, "task-late", None)
            .await
            .unwrap_err();

        assert!(
            matches!(&err, CoreError::BaseVersionOccupied { id, found, .. }
                if id == "test" && found == "装着 Fabric"),
            "{err:?}"
        );
        assert_eq!(
            std::fs::read_to_string(versions.join("test").join("test.json")).unwrap(),
            json
        );
        assert!(!versions.join("周末包").exists());
        assert!(!created.get());
    }

    /// 一个整合包落下基底后，下一个用同样基底的整合包看到的是「目录原本就在」：记录不因此变，基底照样不列出。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn second_pack_on_the_same_bases_keeps_them_hidden() {
        let server = MockServer::start().await;
        mount_vanilla(&server).await;
        mount_fabric(&server).await;
        let tmp = tempfile::tempdir().unwrap();
        let pack = fabric_pack(tmp.path());
        let petalfall = petalfall(tmp.path(), &server);

        import_as(&petalfall, pack.clone(), "第一个").await.unwrap();
        import_as(&petalfall, pack, "第二个").await.unwrap();

        assert_eq!(listed_ids(&petalfall).await, ["第一个", "第二个"]);
    }

    /// 加载器没装成：原版已经落下、目录留着，不能露出来；加载器版本没有落下，不留在记录里，日后别的启动器装出
    /// 这个版本时它是玩家的。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn failed_loader_install_hides_what_landed_and_forgets_what_did_not() {
        let server = MockServer::start().await;
        mount_vanilla(&server).await;
        let tmp = tempfile::tempdir().unwrap();
        let pack = fabric_pack(tmp.path());
        let petalfall = petalfall(tmp.path(), &server);

        import_as(&petalfall, pack, "周末整合包").await.unwrap_err();

        assert!(listed_ids(&petalfall).await.is_empty());
        assert!(
            petalfall
                .game_dir()
                .join("versions")
                .join("test")
                .join("test.json")
                .is_file()
        );

        write_game_version(
            &petalfall,
            NATURAL_FABRIC,
            &fabric_profile("https://example.invalid"),
        );
        assert_eq!(listed_ids(&petalfall).await, [NATURAL_FABRIC]);
    }

    /// 安装器落的目录名和按写法算出的不同（早期 Forge 的 legacy 安装器会这样）：实际落下的记下，算出的那个没落下，
    /// 不留在记录里。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn loader_directory_named_differently_is_recorded_as_it_landed() {
        const LANDED: &str = "test-Forge0.15.0";
        let server = MockServer::start().await;
        mount_vanilla(&server).await;
        Mock::given(method("GET"))
            .and(path("/v2/versions/loader/test/0.15.0/profile/json"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(fabric_profile(&server.uri()).replace(NATURAL_FABRIC, LANDED)),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(
                "/net/fabricmc/fabric-loader/0.15.0/fabric-loader-0.15.0.jar",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"loader".to_vec()))
            .mount(&server)
            .await;
        let tmp = tempfile::tempdir().unwrap();
        let pack = fabric_pack(tmp.path());
        let petalfall = petalfall(tmp.path(), &server);

        import_as(&petalfall, pack, "周末整合包").await.unwrap();

        assert_eq!(listed_ids(&petalfall).await, ["周末整合包"]);
        write_game_version(
            &petalfall,
            NATURAL_FABRIC,
            &fabric_profile("https://example.invalid"),
        );
        assert_eq!(listed_ids(&petalfall).await, [NATURAL_FABRIC, "周末整合包"]);
    }

    /// 原版就没装成：什么都没落下，记录里什么都不留，日后别的启动器装出这个版本时它照常列出。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn failed_vanilla_install_leaves_nothing_in_the_record() {
        let server = MockServer::start().await;
        let base = server.uri();
        Mock::given(method("GET"))
            .and(path("/manifest.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(format!(
                r#"{{"latest":{{"release":"test","snapshot":"test"}},
                    "versions":[{{"id":"test","type":"release","url":"{base}/test.json",
                    "time":"t","releaseTime":"t"}}]}}"#
            )))
            .mount(&server)
            .await;
        let tmp = tempfile::tempdir().unwrap();
        let pack = fabric_pack(tmp.path());
        let petalfall = petalfall(tmp.path(), &server);

        import_as(&petalfall, pack, "周末整合包").await.unwrap_err();

        assert!(listed_ids(&petalfall).await.is_empty());
        write_game_version(
            &petalfall,
            "test",
            r#"{"id":"test","type":"release","mainClass":"net.minecraft.client.main.Main"}"#,
        );
        assert_eq!(listed_ids(&petalfall).await, ["test"]);
    }

    /// 平台整合包：先下载到缓存，导入后缓存删掉，整合包信息记下平台、工程与版本。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn platform_pack_is_downloaded_imported_and_its_cache_removed() {
        let server = MockServer::start().await;
        mount_vanilla(&server).await;
        let tmp = tempfile::tempdir().unwrap();
        let built = tmp.path().join("built.mrpack");
        write_pack(
            &built,
            &index(serde_json::json!({"minecraft": "test"})),
            &[("overrides/options.txt", b"fov:90")],
        );
        let bytes = std::fs::read(&built).unwrap();
        Mock::given(method("GET"))
            .and(path("/version/pv1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "pv1", "project_id": "proj", "name": "1.0.0", "version_number": "1.0.0",
                "version_type": "release", "date_published": "2026-01-01T00:00:00Z",
                "files": [{"hashes": {"sha1": sha1_hex(&bytes)},
                           "url": format!("{}/files/pack.mrpack", server.uri()),
                           "filename": "测试包 1.0.0.mrpack", "primary": true, "size": bytes.len()}]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/files/pack.mrpack"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes))
            .mount(&server)
            .await;
        let petalfall = petalfall(tmp.path(), &server);
        let source = ModpackSource::Platform {
            platform: Platform::Modrinth,
            project_id: "proj".into(),
            version_id: "pv1".into(),
        };

        let outcome = petalfall
            .install_modpack(
                &source,
                Some("从平台装"),
                &ImportSelection::default(),
                "task-p",
                None,
            )
            .await
            .unwrap();
        assert_eq!(outcome.modpack.platform, Some(Platform::Modrinth));
        assert_eq!(outcome.modpack.project_id.as_deref(), Some("proj"));
        assert_eq!(outcome.modpack.version_id.as_deref(), Some("pv1"));
        assert_eq!(
            std::fs::read(outcome.version_dir.join("options.txt")).unwrap(),
            b"fov:90"
        );
        assert_eq!(
            pack_cache_entries(tmp.path()),
            Vec::<String>::new(),
            "下载缓存应在导入后删除"
        );
    }

    /// 准备一步不动游戏目录：名字已定、平台包已下到缓存，但实例目录还没建；放弃时缓存删掉。
    /// 准备失败（名字不合法）时同样不留缓存。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn prepare_leaves_the_game_dir_alone_and_discard_removes_the_cache() {
        let server = MockServer::start().await;
        let tmp = tempfile::tempdir().unwrap();
        let built = tmp.path().join("built.mrpack");
        write_pack(
            &built,
            &index(serde_json::json!({"minecraft": "test"})),
            &[],
        );
        let bytes = std::fs::read(&built).unwrap();
        Mock::given(method("GET"))
            .and(path("/version/pv2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "pv2", "project_id": "proj", "name": "1.0.0", "version_number": "1.0.0",
                "version_type": "release", "date_published": "2026-01-01T00:00:00Z",
                "files": [{"hashes": {"sha1": sha1_hex(&bytes)},
                           "url": format!("{}/files/pack.mrpack", server.uri()),
                           "filename": "pack.mrpack", "primary": true, "size": bytes.len()}]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/files/pack.mrpack"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes))
            .mount(&server)
            .await;
        let petalfall = petalfall(tmp.path(), &server);
        let source = ModpackSource::Platform {
            platform: Platform::Modrinth,
            project_id: "proj".into(),
            version_id: "pv2".into(),
        };

        let prepared = petalfall
            .prepare_modpack(&source, None, &ImportSelection::default(), None)
            .await
            .unwrap();
        assert_eq!(prepared.name(), "测试包");
        let cache = prepared.cache_dir.clone().expect("平台包有下载缓存");
        assert!(
            cache.starts_with(tmp.path().join("data").join("cache").join("modpacks")),
            "{cache:?}"
        );
        assert!(cache.join("pack.mrpack").is_file(), "平台包应已下到缓存");
        assert!(
            !petalfall
                .game_dir()
                .join("versions")
                .join("测试包")
                .exists(),
            "准备一步不建实例目录"
        );
        prepared.discard().await;
        assert_eq!(
            pack_cache_entries(tmp.path()),
            Vec::<String>::new(),
            "放弃导入时删掉下载缓存"
        );

        let err = petalfall
            .prepare_modpack(&source, Some("a/b"), &ImportSelection::default(), None)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            CoreError::InstanceNameInvalid {
                rule: NameViolation::IllegalChar,
                ..
            }
        ));
        assert_eq!(
            pack_cache_entries(tmp.path()),
            Vec::<String>::new(),
            "准备失败也不留下载缓存"
        );
    }

    /// 挂上一个平台版本：`sha1` 是版本信息里声明的包哈希，`bytes` 是下载时真正回的内容。
    async fn mount_platform_pack(
        server: &MockServer,
        version_id: &str,
        sha1: &str,
        bytes: Vec<u8>,
    ) {
        let file = format!("/files/{version_id}.mrpack");
        Mock::given(method("GET"))
            .and(path(format!("/version/{version_id}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": version_id, "project_id": "proj", "name": "1.0.0", "version_number": "1.0.0",
                "version_type": "release", "date_published": "2026-01-01T00:00:00Z",
                "files": [{"hashes": {"sha1": sha1},
                           "url": format!("{}{file}", server.uri()),
                           "filename": "pack.mrpack", "primary": true, "size": bytes.len()}]
            })))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path(file))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes))
            .mount(server)
            .await;
    }

    fn platform_source(version_id: &str) -> ModpackSource {
        ModpackSource::Platform {
            platform: Platform::Modrinth,
            project_id: "proj".into(),
            version_id: version_id.into(),
        }
    }

    /// `<缓存根>/modpacks` 下现有的条目名，排好序；目录还没建出来算没有。
    fn pack_cache_entries(root: &Path) -> Vec<String> {
        let dir = root.join("data").join("cache").join("modpacks");
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// 同一个平台版本同时导入两次：先结束的一路删自己的缓存，另一路手里的包还在，照常导入完。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_imports_of_one_platform_version_keep_their_own_cache() {
        let server = MockServer::start().await;
        mount_vanilla(&server).await;
        let tmp = tempfile::tempdir().unwrap();
        let built = tmp.path().join("built.mrpack");
        write_pack(
            &built,
            &index(serde_json::json!({"minecraft": "test"})),
            &[("overrides/options.txt", b"fov:90")],
        );
        let bytes = std::fs::read(&built).unwrap();
        mount_platform_pack(&server, "pv3", &sha1_hex(&bytes), bytes).await;
        let petalfall = petalfall(tmp.path(), &server);
        let source = platform_source("pv3");
        let none = ImportSelection::default();

        let (first, second) = tokio::join!(
            petalfall.prepare_modpack(&source, Some("第一份"), &none, None),
            petalfall.prepare_modpack(&source, Some("第二份"), &none, None),
        );
        let (first, second) = (first.unwrap(), second.unwrap());
        first.discard().await;
        let outcome = petalfall
            .import_modpack(second, "task-second", None)
            .await
            .unwrap();

        assert_eq!(
            std::fs::read(outcome.version_dir.join("options.txt")).unwrap(),
            b"fov:90"
        );
        assert_eq!(
            pack_cache_entries(tmp.path()),
            Vec::<String>::new(),
            "两路都结束后不留缓存"
        );
    }

    /// 缓存目录只属于这一次导入，下载失败时没有下一次来接手，当场删掉。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn failed_pack_download_leaves_no_cache_behind() {
        let server = MockServer::start().await;
        let tmp = tempfile::tempdir().unwrap();
        // 回来的内容与声明的哈希对不上：下载走到了落盘这一步才失败，目录已经建出来了。
        mount_platform_pack(&server, "pv4", &"0".repeat(40), b"not the pack".to_vec()).await;
        let petalfall = petalfall(tmp.path(), &server);

        let err = petalfall
            .prepare_modpack(
                &platform_source("pv4"),
                None,
                &ImportSelection::default(),
                None,
            )
            .await
            .unwrap_err();

        assert!(matches!(err, CoreError::Download(_)), "{err:?}");
        assert_eq!(pack_cache_entries(tmp.path()), Vec::<String>::new());
    }

    /// 被杀掉的进程留下的目录只有这一步能清掉；新近的目录可能正被别的导入用着，认领暂存位导入后也要留着。
    #[tokio::test]
    async fn sweeping_removes_only_stale_pack_caches() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("data").join("cache").join("modpacks");
        for dir in [
            "claims/curseforge-1",
            "modrinth-v1-0000beef",
            "curseforge-7-1234abcd",
        ] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
            std::fs::write(root.join(dir).join("pack.zip"), b"x").unwrap();
        }

        sweep_stale_pack_caches(&root, SystemTime::now()).await;
        assert_eq!(
            pack_cache_entries(tmp.path()),
            vec!["claims", "curseforge-7-1234abcd", "modrinth-v1-0000beef"],
            "没过期的都留着"
        );

        let later = SystemTime::now() + STALE_PACK_CACHE_AGE + Duration::from_secs(60);
        sweep_stale_pack_caches(&root, later).await;
        assert_eq!(pack_cache_entries(tmp.path()), vec!["claims"]);
        assert!(root.join("claims/curseforge-1/pack.zip").is_file());
    }

    /// 下载平台整合包时顺手清残留：上一次被杀掉的进程留下的目录在这里消失，新近的目录不动。
    #[cfg(windows)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn downloading_a_platform_pack_sweeps_caches_left_by_a_killed_run() {
        use std::os::windows::fs::OpenOptionsExt;

        const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;

        let server = MockServer::start().await;
        let tmp = tempfile::tempdir().unwrap();
        let built = tmp.path().join("built.mrpack");
        write_pack(
            &built,
            &index(serde_json::json!({"minecraft": "test"})),
            &[],
        );
        let bytes = std::fs::read(&built).unwrap();
        mount_platform_pack(&server, "pv5", &sha1_hex(&bytes), bytes).await;
        let root = tmp.path().join("data").join("cache").join("modpacks");
        let stale = root.join("modrinth-old-0000dead");
        let fresh = root.join("modrinth-new-0000beef");
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::create_dir_all(&fresh).unwrap();
        // Windows 上打开目录要带这个标志。
        std::fs::OpenOptions::new()
            .access_mode(FILE_WRITE_ATTRIBUTES)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(&stale)
            .unwrap()
            .set_modified(SystemTime::now() - STALE_PACK_CACHE_AGE - Duration::from_secs(60))
            .unwrap();
        let petalfall = petalfall(tmp.path(), &server);

        let prepared = petalfall
            .prepare_modpack(
                &platform_source("pv5"),
                None,
                &ImportSelection::default(),
                None,
            )
            .await
            .unwrap();

        assert!(!stale.exists(), "残留应被清掉");
        assert!(fresh.is_dir(), "新近的目录可能正被别的导入用着");
        prepared.discard().await;
    }

    /// 还没有缓存目录时什么也不用清。
    #[tokio::test]
    async fn sweeping_a_missing_cache_root_is_a_no_op() {
        let tmp = tempfile::tempdir().unwrap();
        sweep_stale_pack_caches(&tmp.path().join("modpacks"), SystemTime::now()).await;
        assert!(!tmp.path().join("modpacks").exists());
    }

    /// 装游戏失败时整个实例目录删掉；同名（不区分大小写）与和游戏版本同名都在动手前拒绝，已有目录不动。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn failures_leave_no_half_instance_and_names_are_checked_first() {
        let server = MockServer::start().await;
        mount_vanilla(&server).await;
        let tmp = tempfile::tempdir().unwrap();
        let petalfall = petalfall(tmp.path(), &server);
        let versions = petalfall.game_dir().join("versions");

        let missing = tmp.path().join("missing.mrpack");
        write_pack(
            &missing,
            &index(serde_json::json!({"minecraft": "no-such-version"})),
            &[("overrides/a.txt", b"a")],
        );
        let err = petalfall
            .install_modpack(
                &ModpackSource::File { path: missing },
                Some("坏包"),
                &ImportSelection::default(),
                "t1",
                None,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, CoreError::Install(_)), "{err:?}");
        assert!(!versions.join("坏包").exists());

        std::fs::create_dir_all(versions.join("Taken")).unwrap();
        std::fs::write(versions.join("Taken").join("keep.txt"), b"keep").unwrap();
        let pack = tmp.path().join("ok.mrpack");
        write_pack(&pack, &index(serde_json::json!({"minecraft": "test"})), &[]);
        let source = ModpackSource::File { path: pack };
        let err = petalfall
            .install_modpack(
                &source,
                Some("taken"),
                &ImportSelection::default(),
                "t2",
                None,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&err, CoreError::InstanceNameInvalid { name, rule: NameViolation::Duplicate } if name == "taken"),
            "{err:?}"
        );
        assert_eq!(
            std::fs::read(versions.join("Taken").join("keep.txt")).unwrap(),
            b"keep"
        );

        let err = petalfall
            .install_modpack(
                &source,
                Some("TEST"),
                &ImportSelection::default(),
                "t3",
                None,
            )
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            CoreError::InstanceNameInvalid {
                rule: NameViolation::Duplicate,
                ..
            }
        ));
        assert!(
            !versions.join("test").exists(),
            "与游戏版本同名应在安装前拒绝"
        );

        let err = petalfall
            .install_modpack(
                &source,
                Some("a/b"),
                &ImportSelection::default(),
                "t4",
                None,
            )
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            CoreError::InstanceNameInvalid {
                rule: NameViolation::IllegalChar,
                ..
            }
        ));
    }

    const PLANTED_JSON: &[u8] = br#"{"id":"planted","mainClass":"planted.Main"}"#;

    /// 清单里一个落在 `file_path` 上的远程文件，内容是 [`PLANTED_JSON`]。
    fn planted_remote(file_path: &str, optional: bool) -> serde_json::Value {
        let mut entry = serde_json::json!({
            "path": file_path, "hashes": { "sha1": sha1_hex(PLANTED_JSON) },
            "downloads": ["https://cdn.modrinth.com/data/x/planted.bin"],
            "fileSize": PLANTED_JSON.len()
        });
        if optional {
            entry["env"] = serde_json::json!({"client": "optional", "server": "optional"});
        }
        entry
    }

    /// 包里与实例同名的版本 JSON、主 jar、natives 目录不许落进实例：导入先写版本 JSON、后下清单文件并解覆盖
    /// 文件，放行的话实例从此按包里那份 JSON 启动。覆盖文件（两层都算）与清单文件都查，名字不分大小写；撞上时
    /// 报出撞上的那一条与实例名，游戏目录里什么都不建。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pack_files_landing_on_the_instances_own_version_files_are_refused() {
        let server = MockServer::start().await;
        mount_vanilla(&server).await;
        let tmp = tempfile::tempdir().unwrap();
        let petalfall = petalfall(tmp.path(), &server);

        let overrides = [
            "overrides/weekend.JSON",
            "overrides/Weekend.jar",
            "overrides/WEEKEND-natives/lwjgl.dll",
            "client-overrides/Weekend.json",
        ];
        let remotes = ["Weekend.json", "weekend.JAR", "weekend-natives/planted.dll"];
        let cases = overrides
            .iter()
            .map(|entry| (*entry, None))
            .chain(remotes.iter().map(|file| (*file, Some(*file))));
        for (reported, remote) in cases {
            let mut manifest = index(serde_json::json!({"minecraft": "test"}));
            let entries: Vec<(&str, &[u8])> = match remote {
                Some(file) => {
                    manifest["files"] = serde_json::json!([planted_remote(file, false)]);
                    vec![("overrides/config/keep.txt", b"keep".as_slice())]
                }
                None => vec![(reported, PLANTED_JSON)],
            };
            let pack = tmp.path().join("planted.mrpack");
            write_pack(&pack, &manifest, &entries);

            let err = import_as(&petalfall, pack, "Weekend").await.unwrap_err();

            assert!(
                matches!(
                    &err,
                    CoreError::Modpack(petalfall_modpack::Error::InstanceFileCollision { path, instance })
                        if path == reported && instance == "Weekend"
                ),
                "{reported}: {err:?}"
            );
            assert_eq!(
                err.to_string(),
                format!(
                    "整合包里的 {reported} 落在实例「Weekend」自己的版本文件（版本 JSON、主 jar 或 natives 目录）上，换一个实例名再导入"
                )
            );
            assert!(
                !petalfall.game_dir().exists(),
                "{reported}: 拒收发生在动游戏目录之前"
            );
        }
    }

    /// 名字撞没撞上不能比文件系统认得少：它折叠大小写不看上下文，「Σ」与「σ」是同一个名字。整串转小写却会把
    /// 词尾的「Σ」写成「ς」，与包里写的「σ」对不上；这样的落点在磁盘上仍是实例自己的那个文件，一样拒收。「ς」
    /// 与「Σ」、长 s 与「s」是不是同一个名字各个卷不一样，往宽里算，也拒收；字母不同的名字照常导入。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pack_files_matching_the_version_files_up_to_case_folding_are_refused() {
        let server = MockServer::start().await;
        mount_vanilla(&server).await;
        let tmp = tempfile::tempdir().unwrap();
        let petalfall = petalfall(tmp.path(), &server);

        for (name, entry) in [
            ("ΑΣ 1", "overrides/Ασ 1.json"),
            ("ΚΟΣΜΟΣ", "overrides/ΚΟΣΜΟσ-natives/lwjgl.dll"),
            ("ΑΣ 1", "overrides/Ας 1.json"),
            ("Masse", "overrides/Maſſe.jar"),
        ] {
            let pack = tmp.path().join("sigma.mrpack");
            write_pack(
                &pack,
                &index(serde_json::json!({"minecraft": "test"})),
                &[(entry, PLANTED_JSON)],
            );

            let err = import_as(&petalfall, pack, name).await.unwrap_err();

            assert!(
                matches!(
                    &err,
                    CoreError::Modpack(petalfall_modpack::Error::InstanceFileCollision { path, instance })
                        if path == entry && instance == name
                ),
                "{entry}: {err:?}"
            );
            assert!(
                !petalfall.game_dir().exists(),
                "{entry}: 拒收发生在动游戏目录之前"
            );
        }

        let pack = tmp.path().join("other-letter.mrpack");
        write_pack(
            &pack,
            &index(serde_json::json!({"minecraft": "test"})),
            &[("overrides/ΑΤ 1.json", PLANTED_JSON)],
        );
        let outcome = import_as(&petalfall, pack, "ΑΣ 1").await.unwrap();
        assert_eq!(
            std::fs::read(outcome.version_dir.join("ΑΤ 1.json")).unwrap(),
            PLANTED_JSON
        );
        let own: serde_json::Value =
            serde_json::from_slice(&std::fs::read(outcome.version_dir.join("ΑΣ 1.json")).unwrap())
                .unwrap();
        assert_eq!(
            own,
            serde_json::json!({"id": "ΑΣ 1", "inheritsFrom": "test", "type": "release"})
        );
    }

    /// 只有落在实例目录这一层、名字正好是那三个的才拒收。同一个包换个实例名照常导入：那份文件成了普通文件，
    /// 实例的版本 JSON 是启动器写的那份。子目录里的同名文件、名字只是相像的文件照样解出来；选择跳过的可选文件
    /// 不落盘，名字撞上也不拦。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pack_files_that_only_resemble_the_version_files_are_imported_as_ordinary_files() {
        let server = MockServer::start().await;
        mount_vanilla(&server).await;
        let tmp = tempfile::tempdir().unwrap();
        let petalfall = petalfall(tmp.path(), &server);
        let mut manifest = index(serde_json::json!({"minecraft": "test"}));
        manifest["files"] = serde_json::json!([planted_remote("Other.jar", true)]);
        let pack = tmp.path().join("planted.mrpack");
        write_pack(
            &pack,
            &manifest,
            &[
                ("overrides/Weekend.json", PLANTED_JSON),
                ("overrides/config/Other.json", b"nested"),
                ("overrides/Other.json.bak", b"bak"),
                ("overrides/Other-natives.txt", b"txt"),
                ("overrides/Others.json", b"longer"),
            ],
        );

        let outcome = petalfall
            .install_modpack(
                &ModpackSource::File { path: pack },
                Some("Other"),
                &ImportSelection {
                    excluded_optional: vec!["Other.jar".into()],
                },
                "task-resemble",
                None,
            )
            .await
            .unwrap();

        let dir = outcome.version_dir;
        let read = |relative: &str| std::fs::read(dir.join(relative)).unwrap();
        assert_eq!(read("Weekend.json"), PLANTED_JSON);
        assert_eq!(read("config/Other.json"), b"nested");
        assert_eq!(read("Other.json.bak"), b"bak");
        assert_eq!(read("Other-natives.txt"), b"txt");
        assert_eq!(read("Others.json"), b"longer");
        assert!(!dir.join("Other.jar").exists());
        let own: serde_json::Value = serde_json::from_slice(&read("Other.json")).unwrap();
        assert_eq!(
            own,
            serde_json::json!({"id": "Other", "inheritsFrom": "test", "type": "release"})
        );
    }

    /// 已经置位的记号：这次导入建出了实例目录。
    fn dir_was_created() -> InstanceDirCreated {
        let created = InstanceDirCreated::default();
        created.set();
        created
    }

    /// 准备之后、落盘之前，名下冒出了别人的目录（别的程序抢先建的；或名字是另一个实例目录的别名，查重看不出来）：
    /// 导入报重名，记号没置位，凭这个记号去清理一个字节都不删。目录由导入自己建出来、装游戏失败、导入当场把它
    /// 删掉的：记号置位，清理不再看盘，名下后来出现的目录连同存档原样留着。记号停在「建了」、目录还留着的
    /// （导入的 future 被丢弃）：清理删得掉。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn only_a_directory_the_import_created_is_ever_discarded() {
        let server = MockServer::start().await;
        let tmp = tempfile::tempdir().unwrap();
        let petalfall = petalfall(tmp.path(), &server);
        let pack = tmp.path().join("pack.mrpack");
        write_pack(
            &pack,
            &index(serde_json::json!({"minecraft": "no-such-version"})),
            &[],
        );
        let source = ModpackSource::File { path: pack };
        let selection = ImportSelection::default();
        let prepare = || petalfall.prepare_modpack(&source, Some("周末包"), &selection, None);
        let theirs = petalfall.game_dir().join("versions").join("周末包");
        let save = theirs.join("saves").join("世界").join("level.dat");

        let prepared = prepare().await.unwrap();
        let created = prepared.dir_created();
        assert!(!created.get(), "还没落盘");
        std::fs::create_dir_all(save.parent().unwrap()).unwrap();
        std::fs::write(&save, b"level").unwrap();
        let err = petalfall
            .import_modpack(prepared, "task-theirs", None)
            .await
            .unwrap_err();
        assert!(
            matches!(&err, CoreError::InstanceNameInvalid { name, rule: NameViolation::Duplicate } if name == "周末包"),
            "{err:?}"
        );
        assert!(!created.get(), "目录不是这次建的");
        assert_eq!(
            petalfall
                .discard_modpack_import("周末包", &created)
                .await
                .unwrap(),
            DiscardedImport::NotCreated
        );
        assert_eq!(std::fs::read(&save).unwrap(), b"level");
        // 没置位时名字都不校验：不看盘、不报错，调用方拿不到错误，也就不会去改写名下那个目录的标记。
        assert_eq!(
            petalfall
                .discard_modpack_import("..\\邻居", &created)
                .await
                .unwrap(),
            DiscardedImport::NotCreated
        );

        std::fs::remove_dir_all(&theirs).unwrap();
        mount_vanilla(&server).await;
        let prepared = prepare().await.unwrap();
        let created = prepared.dir_created();
        // 清单里没有这个版本：实例目录建好之后装游戏失败，导入自己把目录删了。
        let err = petalfall
            .import_modpack(prepared, "task-ours", None)
            .await
            .unwrap_err();
        assert!(matches!(err, CoreError::Install(_)), "{err:?}");
        assert!(created.get(), "目录是这次建的");
        assert!(!theirs.exists());
        // 导入自己删掉之后，名下又出现的目录不是这次的（别的命令把一个实例改名到了这个名字的另一种写法下）：
        // 清理不看盘，原样返回，那个目录连同存档留着。
        std::fs::create_dir_all(save.parent().unwrap()).unwrap();
        std::fs::write(&save, b"level").unwrap();
        assert_eq!(
            petalfall
                .discard_modpack_import("周末包", &created)
                .await
                .unwrap(),
            DiscardedImport::AlreadyGone
        );
        assert_eq!(std::fs::read(&save).unwrap(), b"level");
        // 导入的 future 被丢弃时记号停在「建了」、目录留着：调用方凭它删掉。
        assert_eq!(
            petalfall
                .discard_modpack_import("周末包", &dir_was_created())
                .await
                .unwrap(),
            DiscardedImport::Removed
        );
        assert!(!theirs.exists());
    }

    /// 按名字删半成品：标记读不出来、还没写下标记的都删得掉；名下没有目录时什么都不做；
    /// 别的实例原样留着；带路径成分的名字直接拒绝。
    #[tokio::test]
    async fn discard_by_name_removes_the_half_import_whatever_its_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let server = MockServer::start().await;
        let petalfall = petalfall(tmp.path(), &server);
        let versions = petalfall.game_dir().join("versions");
        let corrupt = MarkerStore::for_version_dir(&versions.join("标记坏了"));
        std::fs::create_dir_all(corrupt.path().parent().unwrap()).unwrap();
        std::fs::write(corrupt.path(), b"{").unwrap();
        std::fs::create_dir_all(versions.join("还没写标记")).unwrap();
        let ours = versions.join("正常的");
        std::fs::create_dir_all(&ours).unwrap();
        write_marker(&ours, "t1").await.unwrap();
        std::fs::create_dir_all(versions.join("邻居")).unwrap();

        let created = dir_was_created();
        for name in ["标记坏了", "还没写标记", "正常的"] {
            assert_eq!(
                petalfall
                    .discard_modpack_import(name, &created)
                    .await
                    .unwrap(),
                DiscardedImport::Removed,
                "{name}"
            );
            assert!(!versions.join(name).exists(), "{name}");
        }
        assert_eq!(
            petalfall
                .discard_modpack_import("正常的", &created)
                .await
                .unwrap(),
            DiscardedImport::AlreadyGone
        );
        assert!(versions.join("邻居").is_dir());
        assert!(matches!(
            petalfall
                .discard_modpack_import("..\\邻居", &created)
                .await
                .unwrap_err(),
            CoreError::Instance(crate::InstanceError::InvalidId { .. })
        ));
    }

    /// 删不掉的半成品落成结局：保留原来的开始时刻；原标记读不出来时换成一份新的、读得出来的。
    #[tokio::test]
    async fn settling_a_half_import_records_its_outcome() {
        use petalfall_instance::{IncompleteOutcome, MarkerError};

        let tmp = tempfile::tempdir().unwrap();
        let server = MockServer::start().await;
        let petalfall = petalfall(tmp.path(), &server);
        let versions = petalfall.game_dir().join("versions");
        let dir = versions.join("删不掉");
        std::fs::create_dir_all(&dir).unwrap();
        MarkerStore::for_version_dir(&dir)
            .save(&IncompleteMarker::pending(
                IncompleteKind::Modpack {
                    task_id: "mp#1".into(),
                },
                1_700_000_000_000,
            ))
            .await
            .unwrap();
        let failure = MarkerError {
            code: "download_failed".into(),
            message: "下载失败".into(),
        };
        petalfall
            .settle_modpack_import("删不掉", "mp#1", MarkerSettlement::Failed(failure.clone()))
            .await
            .unwrap();
        let marker = MarkerStore::for_version_dir(&dir)
            .load()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(marker.outcome, IncompleteOutcome::Failed);
        assert_eq!(marker.started_at, 1_700_000_000_000);
        assert_eq!(marker.error, Some(failure));
        assert_eq!(
            marker.kind,
            IncompleteKind::Modpack {
                task_id: "mp#1".into()
            }
        );

        let broken = MarkerStore::for_version_dir(&versions.join("标记坏了"));
        std::fs::create_dir_all(broken.path().parent().unwrap()).unwrap();
        std::fs::write(broken.path(), b"{").unwrap();
        petalfall
            .settle_modpack_import("标记坏了", "mp#2", MarkerSettlement::Cancelled)
            .await
            .unwrap();
        let marker = broken.load().await.unwrap().unwrap();
        assert_eq!(marker.outcome, IncompleteOutcome::Cancelled);
        assert!(marker.started_at > 1_700_000_000_000);
    }

    /// 导入写下的标记就是实例列表读的那一种：导入中断后列表认得出它是整合包导入，而不是报标记损坏。
    #[tokio::test]
    async fn import_marker_is_read_by_the_instance_marker_store() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("versions").join("导入中");
        std::fs::create_dir_all(&dir).unwrap();
        write_marker(&dir, "mp#3").await.unwrap();
        let marker = MarkerStore::for_version_dir(&dir)
            .load()
            .await
            .unwrap()
            .expect("应读到标记");
        assert_eq!(
            marker.kind,
            IncompleteKind::Modpack {
                task_id: "mp#3".into()
            }
        );
        assert_eq!(
            marker.outcome,
            petalfall_instance::IncompleteOutcome::Pending
        );
        assert!(marker.started_at > 1_700_000_000_000);
    }

    /// 远程文件下载失败时换备用地址；没有备用地址的失败原样报出。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn remote_download_falls_back_to_the_next_url() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/good.jar"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"jar".to_vec()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/gone.jar"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let tmp = tempfile::tempdir().unwrap();
        let petalfall = petalfall(tmp.path(), &server);
        let dest = tmp.path().join("inst").join("mods").join("a.jar");
        let item = |fallbacks: Vec<String>| RemoteDownload {
            task: DownloadTask::new(format!("{}/gone.jar", server.uri()), &dest)
                .with_sha1(sha1_hex(b"jar"))
                .with_size(3),
            fallbacks,
        };

        petalfall
            .download_remote(vec![item(vec![format!("{}/good.jar", server.uri())])], None)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"jar");

        std::fs::remove_file(&dest).unwrap();
        let err = petalfall
            .download_remote(vec![item(Vec::new())], None)
            .await
            .unwrap_err();
        assert!(matches!(err, CoreError::Download(_)), "{err:?}");
        assert!(!dest.exists());
    }

    fn modrinth_file(dest: &str, urls: &[String], bytes: &[u8]) -> ModrinthFile {
        ModrinthFile {
            path: petalfall_modpack::SafeRelativePath::new(dest).unwrap(),
            urls: urls.to_vec(),
            sha1: sha1_hex(bytes),
            size: bytes.len() as u64,
            optional: false,
        }
    }

    /// 别的启动器导出的 .mrpack 会把 CurseForge 上的 Mod 写成 forgecdn 地址，读包也认这类主机：下载时与
    /// 平台给出的地址一样带密钥。发往 CDN 的每个请求都带，发往别处的不带；CDN 拒绝密钥时报
    /// CredentialRejected（界面据此说明怎么换密钥），不混同于普通的 401。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn mrpack_files_on_the_curseforge_cdn_carry_the_download_key() {
        let cdn = MockServer::start().await;
        let other = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/cdn.jar"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"cdn-jar".to_vec()))
            .mount(&cdn)
            .await;
        Mock::given(method("GET"))
            .and(path("/locked.jar"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&cdn)
            .await;
        Mock::given(method("GET"))
            .and(path("/mr.jar"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"mr-jar".to_vec()))
            .mount(&other)
            .await;
        let tmp = tempfile::tempdir().unwrap();
        let petalfall = petalfall(tmp.path(), &other)
            .with_curseforge_key("k")
            .with_curseforge_cdn_scope(petalfall_download::CredentialScope::new(
                [format!("127.0.0.1:{}", cdn.address().port())],
                false,
            ));
        let inst = tmp.path().join("落樱 实例");
        let files = RemoteFiles::Modrinth(vec![
            modrinth_file(
                "mods/cdn.jar",
                &[format!("{}/cdn.jar", cdn.uri())],
                b"cdn-jar",
            ),
            modrinth_file(
                "mods/mr.jar",
                &[format!("{}/mr.jar", other.uri())],
                b"mr-jar",
            ),
        ]);

        let plan = petalfall
            .remote_downloads(&files, &HashSet::new(), &inst)
            .await
            .unwrap();
        petalfall
            .download_remote(plan.downloads, None)
            .await
            .unwrap();

        assert_eq!(
            std::fs::read(inst.join("mods").join("cdn.jar")).unwrap(),
            b"cdn-jar"
        );
        assert_eq!(
            std::fs::read(inst.join("mods").join("mr.jar")).unwrap(),
            b"mr-jar"
        );
        let cdn_requests = cdn.received_requests().await.unwrap();
        assert!(!cdn_requests.is_empty());
        for request in &cdn_requests {
            assert_eq!(
                request
                    .headers
                    .get("x-api-key")
                    .map(|v| v.to_str().unwrap()),
                Some("k"),
                "{}",
                request.url
            );
        }
        for request in other.received_requests().await.unwrap() {
            assert!(
                request.headers.get("x-api-key").is_none(),
                "{}",
                request.url
            );
        }

        let locked = RemoteFiles::Modrinth(vec![modrinth_file(
            "mods/locked.jar",
            &[format!("{}/locked.jar", cdn.uri())],
            b"x",
        )]);
        let plan = petalfall
            .remote_downloads(&locked, &HashSet::new(), &inst)
            .await
            .unwrap();
        let err = petalfall
            .download_remote(plan.downloads, None)
            .await
            .unwrap_err();
        assert!(
            matches!(
                &err,
                CoreError::Download(petalfall_download::Error::CredentialRejected {
                    status: 401,
                    ..
                })
            ),
            "{err:?}"
        );
    }

    /// 没有 CDN 地址时不要 key；有 CDN 地址时要 key，要不到时看文件还有没有别的路可走。
    #[test]
    fn mrpack_cdn_credential_is_required_only_when_a_file_has_no_other_url() {
        const CDN: &str = "https://edge.forgecdn.net/files/1/2/a.jar";
        const MODRINTH: &str = "https://cdn.modrinth.com/data/a/versions/b/a.jar";
        let scope = CredentialScope::curseforge_cdn();
        let file = |urls: &[&str]| {
            let urls: Vec<String> = urls.iter().map(|url| (*url).to_owned()).collect();
            modrinth_file("mods/a.jar", &urls, b"x")
        };
        let missing = || {
            Err(CoreError::ModPlatform(
                petalfall_modplatform::Error::CurseForgeKeyMissing,
            ))
        };
        let key = || Ok(DownloadCredential::curseforge("k").unwrap());

        let no_cdn = [file(&[MODRINTH])];
        assert!(
            modrinth_cdn_credential(&no_cdn, &scope, || panic!("不该要 key"))
                .unwrap()
                .is_none()
        );

        let cdn_only = [file(&[MODRINTH]), file(&[CDN])];
        assert!(
            modrinth_cdn_credential(&cdn_only, &scope, key)
                .unwrap()
                .is_some()
        );
        assert!(matches!(
            modrinth_cdn_credential(&cdn_only, &scope, missing),
            Err(CoreError::ModPlatform(
                petalfall_modplatform::Error::CurseForgeKeyMissing
            ))
        ));

        let with_fallback = [file(&[CDN, MODRINTH]), file(&[MODRINTH])];
        assert!(
            modrinth_cdn_credential(&with_fallback, &scope, missing)
                .unwrap()
                .is_none()
        );

        // key 配错了不等于没配，有备用地址也要让用户看见。
        let malformed = || {
            Err(CoreError::from(
                DownloadCredential::curseforge(" ").unwrap_err(),
            ))
        };
        assert!(matches!(
            modrinth_cdn_credential(&with_fallback, &scope, malformed),
            Err(CoreError::Download(
                petalfall_download::Error::InvalidCredential { .. }
            ))
        ));
    }

    fn cf_file(id: u32, mod_id: u32, name: &str, url: Option<String>) -> serde_json::Value {
        serde_json::json!({
            "id": id, "modId": mod_id, "displayName": name, "fileName": name,
            "hashes": [{"value": "0f4a890d07402280686a518579fa9fa02309e315", "algo": 1}],
            "fileLength": 10, "downloadUrl": url
        })
    }

    fn cf_mod(id: u32, class_id: u32) -> serde_json::Value {
        serde_json::json!({"id": id, "name": "m", "slug": "m", "summary": "", "classId": class_id})
    }

    /// CurseForge 文件按所属工程的大类落进对应目录，文件对象自带地址的任务都挂 CurseForge 下载凭据；禁止
    /// 分发的文件汇总报错，每项带工程、文件 id、网页链接与落点。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn curseforge_files_land_by_class_and_blocked_files_are_reported_together() {
        let server = MockServer::start().await;
        let uri = server.uri();
        Mock::given(method("POST"))
            .and(path("/v1/mods/files"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": [
                    cf_file(11, 1, "sodium.jar", Some(format!("{uri}/f/11"))),
                    cf_file(22, 2, "faithful.zip", Some(format!("{uri}/f/22"))),
                    cf_file(33, 3, "bsl.zip", Some(format!("{uri}/f/33"))),
                    cf_file(44, 4, "pack.zip", Some(format!("{uri}/f/44"))),
                    cf_file(55, 5, "blocked-a.jar", None),
                    cf_file(66, 6, "blocked-b.jar", None),
                ]})),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/mods"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": [
                    cf_mod(1, 6), cf_mod(2, 12), cf_mod(3, 6552), cf_mod(4, 6945),
                    cf_mod(5, 6), cf_mod(6, 6)
                ]})),
            )
            .mount(&server)
            .await;
        for file in [55u32, 66] {
            Mock::given(method("GET"))
                .and(path(format!(
                    "/v1/mods/{}/files/{file}/download-url",
                    file / 11
                )))
                .respond_with(ResponseTemplate::new(403))
                .mount(&server)
                .await;
        }
        let client = CurseForgeClient::new(petalfall_base::http::build_client().unwrap(), "k")
            .with_base_url(&uri);
        let dir = Path::new("D:/mc/versions/包");
        let refs = |ids: &[(u32, u32)]| -> Vec<CurseForgeFileRef> {
            ids.iter()
                .map(|&(project_id, file_id)| CurseForgeFileRef {
                    project_id,
                    file_id,
                })
                .collect()
        };

        let tmp = tempfile::tempdir().unwrap();
        let petalfall = petalfall(tmp.path(), &server).with_curseforge_key("k");

        let plan = petalfall
            .curseforge_downloads(
                &client,
                &refs(&[(1, 11), (2, 22), (3, 33), (4, 44)]),
                &HashSet::new(),
                dir,
            )
            .await
            .unwrap();
        assert!(plan.claimed.is_empty());
        let downloads = plan.downloads;
        for download in &downloads {
            let credential = download.task.credential.as_ref().expect("应带下载凭据");
            assert_eq!(
                credential.realm(),
                petalfall_download::CredentialRealm::CurseForge
            );
        }
        let dests: Vec<_> = downloads.iter().map(|d| d.task.dest.clone()).collect();
        assert_eq!(
            dests,
            vec![
                dir.join("mods").join("sodium.jar"),
                dir.join("resourcepacks").join("faithful.zip"),
                dir.join("shaderpacks").join("bsl.zip"),
                dir.join("datapacks").join("pack.zip"),
            ]
        );
        assert_eq!(downloads[0].task.url, format!("{uri}/f/11"));
        assert_eq!(downloads[0].task.size, Some(10));
        assert_eq!(
            downloads[0].task.sha1.as_deref(),
            Some("0f4a890d07402280686a518579fa9fa02309e315")
        );

        let err = petalfall
            .curseforge_downloads(
                &client,
                &refs(&[(1, 11), (5, 55), (6, 66)]),
                &HashSet::new(),
                dir,
            )
            .await
            .unwrap_err();
        let CoreError::FileDistributionBlocked { files } = &err else {
            panic!("实际：{err:?}");
        };
        assert_eq!(
            files
                .iter()
                .map(|f| (
                    f.project_id,
                    f.file_id,
                    f.file_name.as_str(),
                    f.web_url.as_str(),
                    f.destination.as_str()
                ))
                .collect::<Vec<_>>(),
            vec![
                (
                    5,
                    55,
                    "blocked-a.jar",
                    "https://www.curseforge.com/minecraft/mc-mods/m/files/55",
                    "mods/blocked-a.jar"
                ),
                (
                    6,
                    66,
                    "blocked-b.jar",
                    "https://www.curseforge.com/minecraft/mc-mods/m/files/66",
                    "mods/blocked-b.jar"
                ),
            ]
        );
        assert_eq!(
            files[0].sha1.as_deref(),
            Some("0f4a890d07402280686a518579fa9fa02309e315")
        );
        assert_eq!((files[0].fingerprint, files[0].size), (0, Some(10)));

        let err = petalfall
            .curseforge_downloads(&client, &refs(&[(9, 99)]), &HashSet::new(), dir)
            .await
            .unwrap_err();
        assert!(
            matches!(err, CoreError::ModVersionNotFound { .. }),
            "{err:?}"
        );
    }

    /// 摘要与预览报的落点就是导入用的落点：同一批引用，各问一次批量文件与批量工程接口，按工程大类落目录、
    /// 大类不认识的按 Mod；大小逐个加总，有一个文件没给大小就不报总量；平台没回的文件与导入一样报不存在。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn curseforge_remote_facts_follow_the_import_landing_rules() {
        use wiremock::matchers::body_json;

        let server = MockServer::start().await;
        let uri = server.uri();
        let sized = |id: u32, mod_id: u32, name: &str, length: Option<u64>| {
            let mut file = cf_file(id, mod_id, name, Some(format!("{uri}/f/{id}")));
            file["fileLength"] = length.map_or(serde_json::Value::Null, Into::into);
            file
        };
        let files = serde_json::json!({"data": [
            sized(11, 1, "sodium.jar", Some(1_000_000)),
            sized(22, 2, "faithful.zip", Some(9_000_000)),
            sized(33, 3, "mystery.jar", Some(0)),
            sized(44, 1, "sodium-extra.jar", Some(u64::from(u32::MAX) + 1)),
        ]});
        Mock::given(method("POST"))
            .and(path("/v1/mods/files"))
            .and(body_json(serde_json::json!({"fileIds": [11, 22, 33, 44]})))
            .respond_with(ResponseTemplate::new(200).set_body_json(files))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/mods"))
            .and(body_json(serde_json::json!({"modIds": [1, 2, 3]})))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": [
                cf_mod(1, 6), cf_mod(2, 12),
                serde_json::json!({"id": 3, "name": "m", "slug": "m", "summary": "", "classId": 17}),
            ]})))
            .expect(1)
            .mount(&server)
            .await;
        let client = CurseForgeClient::new(petalfall_base::http::build_client().unwrap(), "k")
            .with_base_url(&uri);
        let refs: Vec<CurseForgeFileRef> = [(1, 11), (2, 22), (3, 33), (1, 44)]
            .iter()
            .map(|&(project_id, file_id)| CurseForgeFileRef {
                project_id,
                file_id,
            })
            .collect();

        let facts = curseforge_remote_facts(&client, &refs, BlockedProbe::Skip, false)
            .await
            .unwrap();
        assert_eq!(
            facts.destinations().collect::<Vec<_>>(),
            [
                "mods/sodium.jar",
                "resourcepacks/faithful.zip",
                "mods/mystery.jar",
                "mods/sodium-extra.jar"
            ]
        );
        assert_eq!(facts.bytes, Some(10_000_000 + u64::from(u32::MAX) + 1));
        server.verify().await;
        server.reset().await;

        Mock::given(method("POST"))
            .and(path("/v1/mods/files"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": [
                    sized(11, 1, "sodium.jar", Some(1_000_000)),
                    sized(22, 2, "faithful.zip", None),
                ]})),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/mods"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"data": [cf_mod(1, 6), cf_mod(2, 12)]})),
            )
            .mount(&server)
            .await;
        let facts = curseforge_remote_facts(&client, &refs[..2], BlockedProbe::Skip, false)
            .await
            .unwrap();
        assert_eq!(facts.entries.len(), 2);
        assert_eq!(facts.bytes, None, "缺一个文件的大小就不报总量");

        let err = curseforge_remote_facts(&client, &refs, BlockedProbe::Skip, false)
            .await
            .unwrap_err();
        assert!(
            matches!(&err, CoreError::ModVersionNotFound { version_id, .. } if version_id == "33"),
            "{err:?}"
        );
    }

    #[test]
    fn pack_mod_count_takes_remote_files_and_override_jars_directly_in_mods() {
        let none: [&str; 0] = [];
        assert_eq!(count_pack_mods(none, none), 0);
        let remote = [
            "mods/sodium.jar",
            "Mods/Lithium.jar",
            "mods/notes.txt",
            "mods/old/legacy.jar",
            "mods/",
            "resourcepacks/faithful.zip",
            "config/mods/x.jar",
        ];
        assert_eq!(
            count_pack_mods(remote, none),
            3,
            "远程文件落在 mods/ 下就算"
        );
        let overrides = [
            "mods/extra.JAR",
            "mods/README.md",
            "mods/sodium.jar",
            "MODS/lithium.jar",
            "mods/cfg/x.jar",
            "config/extra.jar",
        ];
        assert_eq!(count_pack_mods(none, overrides), 3, "覆盖层只认 .jar");
        assert_eq!(
            count_pack_mods(remote, overrides),
            4,
            "覆盖文件盖掉同名下载文件，只算一次"
        );

        let seed = fastrand::u64(..);
        let mut rng = fastrand::Rng::with_seed(seed);
        let remote_count = rng.usize(0..300);
        let override_count = rng.usize(0..300);
        let remote: Vec<String> = (0..remote_count)
            .map(|i| format!("mods/remote-{i}.jar"))
            .collect();
        let overrides: Vec<String> = (0..override_count)
            .map(|i| format!("mods/override-{i}.jar"))
            .chain(remote.iter().take(rng.usize(0..=remote_count)).cloned())
            .collect();
        assert_eq!(
            count_pack_mods(
                remote.iter().map(String::as_str),
                overrides.iter().map(String::as_str)
            ),
            remote_count + override_count,
            "seed {seed}"
        );
    }

    fn write_curseforge_pack(dest: &Path, manifest: &serde_json::Value, entries: &[(&str, &[u8])]) {
        let mut writer = zip::ZipWriter::new(std::fs::File::create(dest).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        writer.start_file("manifest.json", options).unwrap();
        writer
            .write_all(&serde_json::to_vec(manifest).unwrap())
            .unwrap();
        for (name, bytes) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap();
    }

    fn curseforge_manifest(files: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "minecraft": { "version": "1.20.1", "modLoaders": [{ "id": "forge-47.2.0", "primary": true }] },
            "manifestType": "minecraftModpack", "manifestVersion": 1,
            "name": "本地 CF 包", "files": files, "overrides": "overrides"
        })
    }

    /// 本地 .mrpack 预览：Mod 数是清单里落进 mods/ 的远程文件加上两层覆盖里的 .jar，同一落点只算一次；
    /// 下载量是清单里的大小之和，整个预览不发任何请求。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn local_modrinth_preview_counts_remote_mods_and_override_jars() {
        let server = MockServer::start().await;
        let tmp = tempfile::tempdir().unwrap();
        let file = |path: &str, size: u64| {
            serde_json::json!({
                "path": path, "hashes": { "sha1": "a".repeat(40) },
                "downloads": [format!("https://cdn.modrinth.com/{path}")], "fileSize": size
            })
        };
        let mut manifest =
            index(serde_json::json!({"minecraft": "1.21.1", "fabric-loader": "0.16.10"}));
        manifest["files"] = serde_json::json!([
            file("mods/sodium.jar", 1_000_000),
            file("mods/lithium.jar", 600_000),
            file("resourcepacks/faithful.zip", 9_000_000),
        ]);
        let pack = tmp.path().join("本地.mrpack");
        write_pack(
            &pack,
            &manifest,
            &[
                ("overrides/mods/extra.jar", b"jar"),
                ("overrides/mods/README.md", b"read me"),
                ("overrides/mods/sodium.jar", b"patched"),
                ("overrides/mods/old/legacy.jar", b"old"),
                ("client-overrides/mods/extra.jar", b"client jar"),
                ("client-overrides/mods/zoom.jar", b"zoom"),
                ("overrides/config/sodium.json", b"{}"),
            ],
        );
        let petalfall = petalfall(tmp.path(), &server);

        let preview = petalfall.preview_modpack_file(&pack).await.unwrap();
        assert_eq!(preview.format, PackFormat::Modrinth);
        assert_eq!(preview.remote_files, 3);
        assert_eq!(preview.mod_files, 4, "sodium、lithium、extra、zoom");
        assert_eq!(preview.remote_bytes, Some(10_600_000));
        assert_eq!(preview.override_files, 6, "两层里的 extra.jar 只留一份");
        assert_eq!(
            preview
                .mods
                .iter()
                .map(|m| (m.path.as_str(), m.origin, m.availability))
                .collect::<Vec<_>>(),
            vec![
                (
                    "mods/extra.jar",
                    PackFileOrigin::Override,
                    PackFileAvailability::Bundled
                ),
                (
                    "mods/lithium.jar",
                    PackFileOrigin::Remote,
                    PackFileAvailability::Download
                ),
                (
                    "mods/sodium.jar",
                    PackFileOrigin::Override,
                    PackFileAvailability::Bundled
                ),
                (
                    "mods/zoom.jar",
                    PackFileOrigin::Override,
                    PackFileAvailability::Bundled
                ),
            ]
        );
        assert_eq!(preview.mods.len(), preview.mod_files);
        assert!(preview.optional_extras.is_empty(), "资源包不是可选文件");
        assert!(preview.blocked_files.is_empty());
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    /// 本地 CurseForge 包：没有远程文件时不问平台（也就不需要 API key），Mod 数只来自覆盖层；有远程文件时
    /// 按批量接口回的工程大类认，与覆盖层同名的 .jar 只算一次，下载量是平台给的大小之和。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn local_curseforge_preview_counts_mods_by_class_and_override_jars() {
        let server = MockServer::start().await;
        let tmp = tempfile::tempdir().unwrap();
        let overrides: &[(&str, &[u8])] = &[
            ("overrides/mods/kubejs-extra.jar", b"jar"),
            ("overrides/mods/create-0.5.1.jar", b"patched"),
            ("overrides/mods/notes.txt", b"notes"),
            ("overrides/config/create.toml", b"x=1"),
        ];
        let offline = tmp.path().join("offline.zip");
        write_curseforge_pack(
            &offline,
            &curseforge_manifest(serde_json::json!([])),
            overrides,
        );
        let preview = petalfall(tmp.path(), &server)
            .preview_modpack_file(&offline)
            .await
            .unwrap();
        assert_eq!(preview.format, PackFormat::CurseForge);
        assert_eq!((preview.remote_files, preview.mod_files), (0, 2));
        assert_eq!(preview.remote_bytes, Some(0));
        assert!(server.received_requests().await.unwrap().is_empty());

        Mock::given(method("POST"))
            .and(path("/v1/mods/files"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": [
                    cf_file(11, 1, "create-0.5.1.jar", None),
                    cf_file(22, 2, "jei-15.2.jar", None),
                    cf_file(33, 3, "create-textures.zip", None),
                ]})),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/mods"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": [
                    cf_mod(1, 6), cf_mod(2, 6), cf_mod(3, 12)
                ]})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let online = tmp.path().join("online.zip");
        write_curseforge_pack(
            &online,
            &curseforge_manifest(serde_json::json!([
                { "projectID": 1, "fileID": 11, "required": true },
                { "projectID": 2, "fileID": 22, "required": true },
                { "projectID": 3, "fileID": 33, "required": true }
            ])),
            overrides,
        );
        let archive = read_pack_blocking(online).await.unwrap();
        let RemoteFiles::CurseForge(refs) = &archive.manifest.remote else {
            panic!("应读出 CurseForge 清单");
        };
        let client = CurseForgeClient::new(petalfall_base::http::build_client().unwrap(), "k")
            .with_base_url(server.uri());
        let remote = curseforge_remote_facts(&client, refs, BlockedProbe::Skip, false)
            .await
            .unwrap();
        let preview = preview_of(&archive, remote, &HashSet::new(), Vec::new());
        assert_eq!(preview.remote_files, 3);
        assert_eq!(
            preview.mod_files, 3,
            "create 与 jei 是 Mod、材质包不是；覆盖层的 create 盖掉下载的那份，再加 kubejs-extra"
        );
        assert_eq!(preview.remote_bytes, Some(30), "每个文件 10 字节");
        assert_eq!(preview.override_files, 4);
        assert_eq!(
            preview
                .mods
                .iter()
                .map(|m| (m.file_name.as_str(), m.origin, m.project_id, m.file_id))
                .collect::<Vec<_>>(),
            vec![
                ("create-0.5.1.jar", PackFileOrigin::Override, None, None),
                ("jei-15.2.jar", PackFileOrigin::Remote, Some(2), Some(22)),
                ("kubejs-extra.jar", PackFileOrigin::Override, None, None),
            ]
        );
        assert_eq!(preview.mods.len(), preview.mod_files);
        server.verify().await;
    }

    fn cf_file_with(
        id: u32,
        mod_id: u32,
        name: &str,
        url: Option<String>,
        bytes: &[u8],
    ) -> serde_json::Value {
        let mut file = cf_file(id, mod_id, name, url);
        file["hashes"] = serde_json::json!([{"value": sha1_hex(bytes), "algo": 1}]);
        file["fileLength"] = serde_json::json!(bytes.len());
        file
    }

    fn curseforge_vanilla_manifest(files: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "minecraft": { "version": "test", "modLoaders": [] },
            "manifestType": "minecraftModpack", "manifestVersion": 1,
            "name": "受限包", "files": files, "overrides": "overrides"
        })
    }

    /// 本地 CurseForge 包在预览阶段就列出受限文件：没给下载地址的逐个问 download-url，回 403 的两个进
    /// blocked_files（工程、文件 id、网页链接与落点精确匹配，有 links 的用 websiteUrl，没有的按 slug 拼），
    /// 暂存位里已有校验通过副本的那个记为已认领、不进清单；给了下载地址的不问 download-url。预览本身不失败。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn local_curseforge_preview_lists_blocked_files_before_import() {
        let server = MockServer::start().await;
        let uri = server.uri();
        let claimed_bytes = b"claimed jar";
        Mock::given(method("POST"))
            .and(path("/v1/mods/files"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": [
                    cf_file(11, 1, "jei.jar", Some(format!("{uri}/f/11"))),
                    cf_file(55, 5, "blocked-a.jar", None),
                    cf_file(66, 6, "blocked-b.jar", None),
                    cf_file_with(77, 7, "claimed.jar", None, claimed_bytes),
                ]})),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/mods"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": [
                cf_mod(1, 6),
                {"id": 5, "name": "A", "slug": "a-slug", "summary": "", "classId": 6,
                 "links": {"websiteUrl": "https://www.curseforge.com/minecraft/mc-mods/blocked-a/"}},
                cf_mod(6, 6),
                cf_mod(7, 6),
            ]})))
            .expect(1)
            .mount(&server)
            .await;
        for (project, file) in [(5u32, 55u32), (6, 66), (7, 77)] {
            Mock::given(method("GET"))
                .and(path(format!(
                    "/v1/mods/{project}/files/{file}/download-url"
                )))
                .respond_with(ResponseTemplate::new(403))
                .expect(1)
                .mount(&server)
                .await;
        }
        Mock::given(method("GET"))
            .and(path("/v1/mods/1/files/11/download-url"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        let tmp = tempfile::tempdir().unwrap();
        let pack = tmp.path().join("local.zip");
        write_curseforge_pack(
            &pack,
            &curseforge_vanilla_manifest(serde_json::json!([
                { "projectID": 1, "fileID": 11, "required": true },
                { "projectID": 5, "fileID": 55, "required": true },
                { "projectID": 6, "fileID": 66, "required": true },
                { "projectID": 7, "fileID": 77, "required": true }
            ])),
            &[("overrides/mods/extra.jar", b"x")],
        );
        let petalfall = petalfall(tmp.path(), &server).with_curseforge_key("k");
        let slot = petalfall
            .blocked_claims_dir()
            .join("curseforge-77")
            .join("claimed.jar");
        std::fs::create_dir_all(slot.parent().unwrap()).unwrap();
        std::fs::write(&slot, claimed_bytes).unwrap();

        let preview = petalfall.preview_modpack_file(&pack).await.unwrap();
        let hash = Some("0f4a890d07402280686a518579fa9fa02309e315".to_owned());
        assert_eq!(
            preview.blocked_files,
            vec![
                BlockedFile {
                    project_id: 5,
                    file_id: 55,
                    file_name: "blocked-a.jar".into(),
                    web_url: "https://www.curseforge.com/minecraft/mc-mods/blocked-a/files/55"
                        .into(),
                    destination: "mods/blocked-a.jar".into(),
                    sha1: hash.clone(),
                    fingerprint: 0,
                    size: Some(10),
                },
                BlockedFile {
                    project_id: 6,
                    file_id: 66,
                    file_name: "blocked-b.jar".into(),
                    web_url: "https://www.curseforge.com/minecraft/mc-mods/m/files/66".into(),
                    destination: "mods/blocked-b.jar".into(),
                    sha1: hash,
                    fingerprint: 0,
                    size: Some(10),
                },
            ]
        );
        assert_eq!(
            preview
                .mods
                .iter()
                .map(|m| (m.file_name.as_str(), m.availability, m.file_id))
                .collect::<Vec<_>>(),
            vec![
                ("blocked-a.jar", PackFileAvailability::Blocked, Some(55)),
                ("blocked-b.jar", PackFileAvailability::Blocked, Some(66)),
                ("claimed.jar", PackFileAvailability::Claimed, Some(77)),
                ("extra.jar", PackFileAvailability::Bundled, None),
                ("jei.jar", PackFileAvailability::Download, Some(11)),
            ]
        );
        assert_eq!(preview.mod_files, 5);
        assert_eq!(preview.mods.len(), preview.mod_files);
        server.verify().await;
    }

    /// mrpack 里取消勾选的可选文件不进下载计划：准备好的任务里没有它们，导入时 CDN 也只收到必需文件的请求
    /// （清单只认 https 地址，准备好之后把任务地址换到本地 mock 再导入）；
    /// 预览里可选 Mod 在 Mod 清单里标为可选，资源包列在其他可选文件里；实例记下的远程文件数按剔除后的清单算。
    /// 不是可选文件的路径报 NotOptional。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn skipped_optional_files_are_left_out_of_the_download_plan() {
        let server = MockServer::start().await;
        mount_vanilla(&server).await;
        let uri = server.uri();
        let files: [(&str, &[u8], bool); 3] = [
            ("mods/core.jar", b"core", false),
            ("mods/optional-helper.jar", b"helper", true),
            ("resourcepacks/optional-pack.zip", b"pack!", true),
        ];
        let mut manifest = index(serde_json::json!({"minecraft": "test"}));
        manifest["files"] = files
            .iter()
            .map(|(file_path, bytes, optional)| {
                let name = file_path.rsplit('/').next().unwrap();
                let mut entry = serde_json::json!({
                    "path": file_path, "hashes": { "sha1": sha1_hex(bytes) },
                    "downloads": [format!("https://cdn.modrinth.com/data/x/{name}")],
                    "fileSize": bytes.len()
                });
                if *optional {
                    entry["env"] = serde_json::json!({"client": "optional", "server": "optional"});
                }
                entry
            })
            .collect();
        for (file_path, bytes, _) in &files {
            let name = file_path.rsplit('/').next().unwrap();
            Mock::given(method("GET"))
                .and(path(format!("/cdn/{name}")))
                .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes.to_vec()))
                .mount(&server)
                .await;
        }
        let tmp = tempfile::tempdir().unwrap();
        let pack = tmp.path().join("可选.mrpack");
        write_pack(&pack, &manifest, &[]);
        let petalfall = petalfall(tmp.path(), &server);
        let source = ModpackSource::File { path: pack.clone() };

        let preview = petalfall.preview_modpack_file(&pack).await.unwrap();
        assert_eq!(
            preview
                .mods
                .iter()
                .map(|m| (m.path.as_str(), m.optional))
                .collect::<Vec<_>>(),
            vec![("mods/core.jar", false), ("mods/optional-helper.jar", true)]
        );
        assert_eq!(
            preview
                .optional_extras
                .iter()
                .map(|m| (m.path.as_str(), m.optional, m.origin))
                .collect::<Vec<_>>(),
            vec![(
                "resourcepacks/optional-pack.zip",
                true,
                PackFileOrigin::Remote
            )]
        );

        let selection = ImportSelection {
            excluded_optional: vec![
                "mods/optional-helper.jar".into(),
                "resourcepacks/optional-pack.zip".into(),
            ],
        };
        let dir = petalfall.game_dir().join("versions").join("可选");
        let mut prepared = petalfall
            .prepare_modpack(&source, Some("可选"), &selection, None)
            .await
            .unwrap();
        assert_eq!(
            prepared
                .remote
                .iter()
                .map(|d| (d.task.dest.clone(), d.task.url.clone()))
                .collect::<Vec<_>>(),
            vec![(
                dir.join("mods").join("core.jar"),
                "https://cdn.modrinth.com/data/x/core.jar".to_owned()
            )]
        );
        assert!(
            prepared.remote.iter().all(|d| d.task.credential.is_none()),
            "Modrinth 文件不挂下载凭据"
        );
        for download in &mut prepared.remote {
            let name = download.task.url.rsplit('/').next().unwrap().to_owned();
            download.task.url = format!("{uri}/cdn/{name}");
        }

        let outcome = petalfall
            .import_modpack(prepared, "task-o", None)
            .await
            .unwrap();
        let cdn: Vec<String> = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .map(|r| r.url.path().to_owned())
            .filter(|p| p.starts_with("/cdn/"))
            .collect();
        assert_eq!(cdn, vec!["/cdn/core.jar"]);
        assert_eq!(
            std::fs::read(dir.join("mods").join("core.jar")).unwrap(),
            b"core"
        );
        assert!(!dir.join("mods").join("optional-helper.jar").exists());
        assert!(!dir.join("resourcepacks").join("optional-pack.zip").exists());
        assert_eq!(outcome.modpack.remote_files, 1);

        let err = petalfall
            .prepare_modpack(
                &source,
                Some("必需"),
                &ImportSelection {
                    excluded_optional: vec!["mods/core.jar".into()],
                },
                None,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&err, CoreError::Modpack(petalfall_modpack::Error::NotOptional { path }) if path == "mods/core.jar"),
            "{err:?}"
        );
    }

    /// 受限文件的落点被覆盖层盖住时不用手动下载：覆盖文件在下载之后解出，手动下载的那份反正会被盖掉。
    /// 预览不列它、准备不报受限，导入后落点是包里自带的内容；落点比较不分大小写。别的受限文件照常报。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn blocked_files_covered_by_an_override_need_no_manual_download() {
        let server = MockServer::start().await;
        mount_vanilla(&server).await;
        Mock::given(method("POST"))
            .and(path("/v1/mods/files"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": [
                    cf_file(55, 5, "Covered.jar", None),
                    cf_file(66, 6, "still-blocked.jar", None),
                ]})),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/mods"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"data": [cf_mod(5, 6), cf_mod(6, 6)]})),
            )
            .mount(&server)
            .await;
        for (project, file) in [(5u32, 55u32), (6, 66)] {
            Mock::given(method("GET"))
                .and(path(format!(
                    "/v1/mods/{project}/files/{file}/download-url"
                )))
                .respond_with(ResponseTemplate::new(403))
                .mount(&server)
                .await;
        }
        let tmp = tempfile::tempdir().unwrap();
        let petalfall = petalfall(tmp.path(), &server).with_curseforge_key("k");
        let covered = serde_json::json!({ "projectID": 5, "fileID": 55, "required": true });
        let blocked = serde_json::json!({ "projectID": 6, "fileID": 66, "required": true });
        let bundled: &[(&str, &[u8])] = &[("overrides/mods/covered.jar", b"bundled jar")];
        let none = ImportSelection::default();

        // 只有被盖住的那个：预览不列、导入不拦。
        let pack = tmp.path().join("自带.zip");
        write_curseforge_pack(
            &pack,
            &curseforge_vanilla_manifest(serde_json::json!([covered])),
            bundled,
        );
        let preview = petalfall.preview_modpack_file(&pack).await.unwrap();
        assert_eq!(preview.blocked_files, Vec::new());
        assert_eq!(
            preview
                .mods
                .iter()
                .map(|m| (m.file_name.as_str(), m.availability))
                .collect::<Vec<_>>(),
            vec![("covered.jar", PackFileAvailability::Bundled)]
        );
        let outcome = petalfall
            .install_modpack(
                &ModpackSource::File { path: pack },
                Some("自带"),
                &none,
                "task-covered",
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            std::fs::read(outcome.version_dir.join("mods").join("covered.jar")).unwrap(),
            b"bundled jar"
        );

        // 另一个没被盖住的文件照常报，清单里只有它。
        let pack = tmp.path().join("混合.zip");
        write_curseforge_pack(
            &pack,
            &curseforge_vanilla_manifest(serde_json::json!([covered, blocked])),
            bundled,
        );
        let preview = petalfall.preview_modpack_file(&pack).await.unwrap();
        assert_eq!(
            preview
                .blocked_files
                .iter()
                .map(|f| f.file_id)
                .collect::<Vec<_>>(),
            vec![66]
        );
        let err = petalfall
            .prepare_modpack(&ModpackSource::File { path: pack }, None, &none, None)
            .await
            .unwrap_err();
        let CoreError::FileDistributionBlocked { files } = err else {
            panic!("应报受限文件");
        };
        assert_eq!(
            files.iter().map(|f| f.file_id).collect::<Vec<_>>(),
            vec![66]
        );
    }

    /// 受限文件的完整流程（设置关着）：准备时报受限，只列没认领的那一个；从下载文件夹认领（浏览器改过名）后
    /// 再准备不再报受限，它不生成下载任务；导入后它落在 mods/ 下、内容与认领的文件相同，阶段里有「放入 1 个
    /// 手动下载的文件」，暂存副本与下载文件夹里的原文件都还在。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn claimed_blocked_files_are_copied_in_on_import() {
        let server = MockServer::start().await;
        mount_vanilla(&server).await;
        let uri = server.uri();
        let blocked_bytes = b"blocked jar bytes";
        Mock::given(method("POST"))
            .and(path("/v1/mods/files"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": [
                    cf_file_with(11, 1, "jei.jar", Some(format!("{uri}/f/11")), b"jei"),
                    cf_file_with(55, 5, "blocked-a.jar", None, blocked_bytes),
                ]})),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/mods"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"data": [cf_mod(1, 6), cf_mod(5, 6)]})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/mods/5/files/55/download-url"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/f/11"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"jei".to_vec()))
            .mount(&server)
            .await;
        let tmp = tempfile::tempdir().unwrap();
        let pack = tmp.path().join("受限包.zip");
        write_curseforge_pack(
            &pack,
            &curseforge_vanilla_manifest(serde_json::json!([
                { "projectID": 1, "fileID": 11, "required": true },
                { "projectID": 5, "fileID": 55, "required": true }
            ])),
            &[],
        );
        let petalfall = petalfall(tmp.path(), &server).with_curseforge_key("k");
        let source = ModpackSource::File { path: pack };
        let none = ImportSelection::default();

        let err = petalfall
            .prepare_modpack(&source, None, &none, None)
            .await
            .unwrap_err();
        let CoreError::FileDistributionBlocked { files } = err else {
            panic!("应报受限文件");
        };
        assert_eq!(
            files.iter().map(|f| f.file_id).collect::<Vec<_>>(),
            vec![55]
        );

        let downloads = tmp.path().join("Downloads");
        std::fs::create_dir_all(&downloads).unwrap();
        let renamed = downloads.join("blocked-a (1).jar");
        std::fs::write(&renamed, blocked_bytes).unwrap();
        let report = petalfall
            .claim_blocked_files(&downloads, &files)
            .await
            .unwrap();
        assert_eq!(
            report.claimed,
            vec![crate::blocked::ClaimedFile {
                file_id: 55,
                source: renamed.clone(),
            }]
        );
        assert!(report.missing.is_empty());

        let dir = petalfall.game_dir().join("versions").join("受限包");
        let prepared = petalfall
            .prepare_modpack(&source, None, &none, None)
            .await
            .unwrap();
        assert_eq!(
            prepared
                .remote
                .iter()
                .map(|d| d.task.dest.clone())
                .collect::<Vec<_>>(),
            vec![dir.join("mods").join("jei.jar")]
        );
        assert_eq!(
            prepared
                .claimed
                .iter()
                .map(|c| c.dest.clone())
                .collect::<Vec<_>>(),
            vec![dir.join("mods").join("blocked-a.jar")]
        );
        prepared.discard().await;

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let outcome = petalfall
            .install_modpack(&source, None, &none, "task-c", Some(&tx))
            .await
            .unwrap();
        assert_eq!(
            std::fs::read(dir.join("mods").join("blocked-a.jar")).unwrap(),
            blocked_bytes
        );
        assert_eq!(
            std::fs::read(dir.join("mods").join("jei.jar")).unwrap(),
            b"jei"
        );
        assert_eq!(outcome.modpack.remote_files, 2);
        drop(tx);
        let mut stages = Vec::new();
        while let Some(event) = rx.recv().await {
            if let CoreEvent::Stage(stage) = event {
                stages.push(stage);
            }
        }
        assert!(
            stages.iter().any(|s| s == "放入 1 个手动下载的文件"),
            "{stages:?}"
        );
        assert!(
            petalfall
                .blocked_claims_dir()
                .join("curseforge-55")
                .join("blocked-a.jar")
                .is_file(),
            "暂存副本导入后留着"
        );
        assert!(renamed.is_file(), "下载文件夹里的原文件不动");
    }

    /// CurseForge 整合包本体就不允许启动器直接下载、设置关着：第一次导入报受限文件，清单里只有这个包；用户
    /// 从网页下载、认领进暂存位后再导入，直接用暂存副本，不再向 CDN 要包，暂存副本导入后留着。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn claimed_curseforge_pack_file_is_imported_without_downloading() {
        let server = MockServer::start().await;
        mount_vanilla(&server).await;
        let tmp = tempfile::tempdir().unwrap();
        let built = tmp.path().join("built.zip");
        let mut manifest = curseforge_vanilla_manifest(serde_json::json!([]));
        manifest["minecraft"]["recommendedRam"] = 8192.into();
        write_curseforge_pack(&built, &manifest, &[("overrides/options.txt", b"fov:70")]);
        let pack_bytes = std::fs::read(&built).unwrap();
        Mock::given(method("GET"))
            .and(path("/v1/mods/700/files/7001"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": cf_file_with(7001, 700, "受限包-1.0.zip", None, &pack_bytes)
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/mods/700/files/7001/download-url"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/mods/700"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"data": cf_mod(700, 4471)})),
            )
            .mount(&server)
            .await;
        let petalfall = petalfall(tmp.path(), &server).with_curseforge_key("k");
        let source = ModpackSource::Platform {
            platform: Platform::CurseForge,
            project_id: "700".into(),
            version_id: "7001".into(),
        };
        let none = ImportSelection::default();

        let err = petalfall
            .install_modpack(&source, Some("网页下的包"), &none, "task-b1", None)
            .await
            .unwrap_err();
        let CoreError::FileDistributionBlocked { files } = err else {
            panic!("应报受限文件：{err:?}");
        };
        assert_eq!(
            files
                .iter()
                .map(|f| (f.file_id, f.file_name.as_str(), f.destination.as_str()))
                .collect::<Vec<_>>(),
            vec![(7001, "受限包-1.0.zip", "受限包-1.0.zip")]
        );

        let downloads = tmp.path().join("下载 文件夹");
        std::fs::create_dir_all(&downloads).unwrap();
        let renamed = downloads.join("受限包-1.0 (1).zip");
        std::fs::write(&renamed, &pack_bytes).unwrap();
        let report = petalfall
            .claim_blocked_files(&downloads, &files)
            .await
            .unwrap();
        assert_eq!(report.missing, Vec::<u32>::new());

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let outcome = petalfall
            .install_modpack(&source, Some("网页下的包"), &none, "task-b2", Some(&tx))
            .await
            .unwrap();
        drop(tx);
        // 用暂存副本也照报下载整合包一步；包里没有远程文件，放文件一步读包之后从表里去掉，不报。
        let phases = drain_phases(&mut rx);
        let prelude = import_prelude_plan(&source);
        let exact = {
            use InstallPhase::*;
            vec![
                ModpackDownload,
                ModpackRead,
                Metadata,
                Libraries,
                Assets,
                Natives,
                ModpackOverrides,
                Template,
            ]
        };
        assert_eq!(phases[..2], [(prelude.clone(), 0), (prelude, 1)]);
        // 读包之后的每一条都带改写后的表本身：放文件一步真的从表里去掉了，不只是没报。
        assert!(
            phases[2..].iter().all(|(plan, _)| *plan == exact),
            "{phases:?}"
        );
        assert_eq!(
            phases
                .iter()
                .map(|(plan, index)| plan[*index])
                .collect::<Vec<_>>(),
            exact
        );
        assert_phase_protocol(&phases);
        assert_eq!(outcome.modpack.platform, Some(Platform::CurseForge));
        assert_eq!(outcome.modpack.version_id.as_deref(), Some("7001"));
        assert_eq!(
            read_instance_modpack(&outcome.version_dir)
                .await
                .unwrap()
                .and_then(|info| info.recommended_ram_mb),
            Some(8192),
            "作者写的推荐内存随整合包记录落盘，自动内存按它估算"
        );
        assert_eq!(
            std::fs::read(outcome.version_dir.join("options.txt")).unwrap(),
            b"fov:70"
        );
        assert!(
            petalfall
                .blocked_claims_dir()
                .join("curseforge-7001")
                .join("受限包-1.0.zip")
                .is_file(),
            "暂存副本导入后留着"
        );
        assert!(renamed.is_file(), "下载文件夹里的原文件不动");
    }

    /// 整合包本体受限、设置关着：认领之前摘要报受限、清单里只有这个包；认领之后摘要从暂存副本读，数字与
    /// 导入时读到的同一份包一致，不向 CDN 要包。暂存副本被改过时不算认领，照旧报受限。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn claimed_curseforge_pack_file_is_summarized_from_the_claim() {
        let server = MockServer::start().await;
        let tmp = tempfile::tempdir().unwrap();
        let built = tmp.path().join("built.zip");
        write_curseforge_pack(
            &built,
            &curseforge_vanilla_manifest(serde_json::json!([
                { "projectID": 1, "fileID": 11, "required": true }
            ])),
            &[
                ("overrides/options.txt", b"fov:70"),
                ("overrides/mods/extra.jar", b"jar"),
                ("overrides/mods/README.md", b"read me"),
            ],
        );
        let pack_bytes = std::fs::read(&built).unwrap();
        Mock::given(method("GET"))
            .and(path("/v1/mods/700/files/7001"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": cf_file_with(7001, 700, "受限包-1.0.zip", None, &pack_bytes)
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/mods/700/files/7001/download-url"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/mods/700"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"data": cf_mod(700, 4471)})),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/mods/files"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": [
                    cf_file(11, 1, "jei.jar", None),
                ]})),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/mods"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"data": [cf_mod(1, 6)]})),
            )
            .mount(&server)
            .await;
        let petalfall = petalfall(tmp.path(), &server).with_curseforge_key("k");

        let err = petalfall
            .summarize_platform_modpack(Platform::CurseForge, "700", "7001")
            .await
            .unwrap_err();
        let CoreError::FileDistributionBlocked { files } = err else {
            panic!("应报受限文件：{err:?}");
        };
        assert_eq!(
            files
                .iter()
                .map(|f| (f.file_id, f.destination.as_str()))
                .collect::<Vec<_>>(),
            vec![(7001, "受限包-1.0.zip")]
        );

        let downloads = tmp.path().join("下载");
        std::fs::create_dir_all(&downloads).unwrap();
        std::fs::write(downloads.join("受限包-1.0 (1).zip"), &pack_bytes).unwrap();
        let report = petalfall
            .claim_blocked_files(&downloads, &files)
            .await
            .unwrap();
        assert_eq!(report.missing, Vec::<u32>::new());

        let summary = petalfall
            .summarize_platform_modpack(Platform::CurseForge, "700", "7001")
            .await
            .unwrap();
        assert_eq!(
            summary,
            crate::modpack_summary::ModpackSummary {
                format: PackFormat::CurseForge,
                name: "受限包".into(),
                version: None,
                minecraft: "test".into(),
                loader: None,
                pack_bytes: pack_bytes.len() as u64,
                remote_files: 1,
                mod_files: 2,
                remote_bytes: Some(10),
                override_files: 3,
                override_bytes: 6 + 3 + 7,
            }
        );

        let staged = petalfall
            .blocked_claims_dir()
            .join("curseforge-7001")
            .join("受限包-1.0.zip");
        std::fs::write(&staged, b"tampered").unwrap();
        let err = petalfall
            .summarize_platform_modpack(Platform::CurseForge, "700", "7001")
            .await
            .unwrap_err();
        assert!(
            matches!(&err, CoreError::FileDistributionBlocked { files } if files[0].file_id == 7001),
            "{err:?}"
        );
    }

    // ---- 设置打开时按拼出的 CDN 地址下载受限文件 ----

    const BLOCKED_A: &[u8] = b"blocked a bytes";

    /// 按文件编号与文件名拼出的 CDN 路径，规律同 `CurseForgeClient::derived_download_url`：编号整除 1000、
    /// 余数、文件名。这里的文件名只有字母、数字、连字符与点，不用转义。
    fn derived_path(file_id: u32, name: &str) -> String {
        format!("/files/{}/{}/{name}", file_id / 1000, file_id % 1000)
    }

    /// 写一个只要原版 test、清单引用 `ids`（都是必需文件）的 CurseForge 包，整合包名是「受限包」。
    fn restricted_pack(
        dir: &Path,
        file: &str,
        ids: &[(u32, u32)],
        overrides: &[(&str, &[u8])],
    ) -> PathBuf {
        let refs: serde_json::Value = ids
            .iter()
            .map(|&(project_id, file_id)| {
                serde_json::json!({ "projectID": project_id, "fileID": file_id, "required": true })
            })
            .collect();
        let pack = dir.join(file);
        write_curseforge_pack(&pack, &curseforge_vanilla_manifest(refs), overrides);
        pack
    }

    /// 两个批量接口回 `files` 与它们的工程（大类都是 Mod）；`restricted` 里每个（工程, 文件）的 download-url
    /// 端点回 403，与平台对作者禁止第三方分发的文件的回应一致。
    async fn mount_restricted_platform(
        server: &MockServer,
        files: Vec<serde_json::Value>,
        restricted: &[(u32, u32)],
    ) {
        let mods: Vec<serde_json::Value> = files
            .iter()
            .map(|file| cf_mod(u32::try_from(file["modId"].as_u64().unwrap()).unwrap(), 6))
            .collect();
        Mock::given(method("POST"))
            .and(path("/v1/mods/files"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "data": files })),
            )
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/mods"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "data": mods })),
            )
            .mount(server)
            .await;
        for (project, file) in restricted {
            Mock::given(method("GET"))
                .and(path(format!(
                    "/v1/mods/{project}/files/{file}/download-url"
                )))
                .respond_with(ResponseTemplate::new(403))
                .mount(server)
                .await;
        }
    }

    /// 拼出的地址上的探测（HEAD）回 `status`，期望被探 `times` 次。
    async fn mount_probe(
        server: &MockServer,
        file_id: u32,
        name: &str,
        status: u16,
        times: impl Into<wiremock::Times>,
    ) {
        Mock::given(method("HEAD"))
            .and(path(derived_path(file_id, name)))
            .respond_with(ResponseTemplate::new(status))
            .expect(times)
            .mount(server)
            .await;
    }

    /// 拼出的地址上的下载（GET）回 `bytes`，期望被下 `times` 次。
    async fn mount_derived_file(
        server: &MockServer,
        file_id: u32,
        name: &str,
        bytes: &[u8],
        times: u64,
    ) {
        Mock::given(method("GET"))
            .and(path(derived_path(file_id, name)))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes.to_vec()))
            .expect(times)
            .mount(server)
            .await;
    }

    /// API、拼地址用的 CDN 根地址与 CDN 凭据作用域都指到这个假服务器。凭据作用域也指过来，任务误挂了凭据时
    /// x-api-key 真的会发到拼出的地址上，测试才看得见。
    fn derive_petalfall(root: &Path, server: &MockServer, derive: bool) -> Petalfall {
        let mut petalfall = petalfall(root, server)
            .with_curseforge_key("k")
            .with_curseforge_cdn_base(server.uri())
            .with_curseforge_cdn_scope(petalfall_download::CredentialScope::new(
                [format!("127.0.0.1:{}", server.address().port())],
                false,
            ));
        petalfall.set_download_restricted_curseforge_files(derive);
        petalfall
    }

    /// 准备导入，期望报受限，返回清单。
    async fn blocked_on_prepare(petalfall: &Petalfall, pack: &Path) -> Vec<BlockedFile> {
        let err = petalfall
            .prepare_modpack(
                &ModpackSource::File {
                    path: pack.to_path_buf(),
                },
                None,
                &ImportSelection::default(),
                None,
            )
            .await
            .unwrap_err();
        let CoreError::FileDistributionBlocked { files } = err else {
            panic!("应报受限文件：{err:?}");
        };
        files
    }

    /// 假服务器在 `at` 上收到的 `verb` 请求各自带的 x-api-key，按到达顺序。
    async fn api_keys_sent(server: &MockServer, verb: &str, at: &str) -> Vec<Option<String>> {
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|request| request.method.as_str() == verb && request.url.path() == at)
            .map(|request| {
                request
                    .headers
                    .get("x-api-key")
                    .map(|value| value.to_str().unwrap().to_owned())
            })
            .collect()
    }

    /// 设置关着：受限文件照旧报受限，拼出的地址一次都不碰。假服务器上那个地址明明能下（HEAD 与 GET 都回 200），
    /// 设置要是没被当回事，文件就会被悄悄下回来、不报受限。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn restricted_files_stay_blocked_on_import_while_the_setting_is_off() {
        let server = MockServer::start().await;
        mount_restricted_platform(
            &server,
            vec![cf_file_with(55012, 5, "blocked-a.jar", None, BLOCKED_A)],
            &[(5, 55012)],
        )
        .await;
        mount_probe(&server, 55012, "blocked-a.jar", 200, 0u64).await;
        mount_derived_file(&server, 55012, "blocked-a.jar", BLOCKED_A, 0).await;
        let tmp = tempfile::tempdir().unwrap();
        let pack = restricted_pack(tmp.path(), "受限包.zip", &[(5, 55012)], &[]);
        let petalfall = derive_petalfall(tmp.path(), &server, false);

        let files = blocked_on_prepare(&petalfall, &pack).await;
        assert_eq!(
            files
                .iter()
                .map(|f| (f.file_id, f.destination.as_str()))
                .collect::<Vec<_>>(),
            vec![(55012, "mods/blocked-a.jar")]
        );
        server.verify().await;
    }

    /// 设置开着、拼出的地址探得到：准备不报受限，任务的地址是拼出的那个，带着文件对象里的大小与 sha1、不挂
    /// CDN 凭据；导入后文件落在 mods/ 下、内容校验通过。凭据作用域也指着这个假服务器：文件对象自带地址的 jei
    /// 发出的 GET 带着 x-api-key，拼出地址的 GET 不带。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn derived_urls_download_restricted_files_on_import_without_the_key() {
        let server = MockServer::start().await;
        mount_vanilla(&server).await;
        let uri = server.uri();
        mount_restricted_platform(
            &server,
            vec![
                cf_file_with(11, 1, "jei.jar", Some(format!("{uri}/f/11")), b"jei"),
                cf_file_with(55012, 5, "blocked-a.jar", None, BLOCKED_A),
            ],
            &[(5, 55012)],
        )
        .await;
        Mock::given(method("GET"))
            .and(path("/f/11"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"jei".to_vec()))
            .mount(&server)
            .await;
        mount_probe(&server, 55012, "blocked-a.jar", 200, 1u64).await;
        mount_derived_file(&server, 55012, "blocked-a.jar", BLOCKED_A, 1).await;
        let tmp = tempfile::tempdir().unwrap();
        let pack = restricted_pack(tmp.path(), "受限包.zip", &[(1, 11), (5, 55012)], &[]);
        let petalfall = derive_petalfall(tmp.path(), &server, true);
        let mods = petalfall
            .game_dir()
            .join("versions")
            .join("受限包")
            .join("mods");
        let derived = derived_path(55012, "blocked-a.jar");

        let prepared = petalfall
            .prepare_modpack(
                &ModpackSource::File { path: pack },
                None,
                &ImportSelection::default(),
                None,
            )
            .await
            .unwrap();
        assert!(prepared.claimed.is_empty());
        assert_eq!(
            prepared
                .remote
                .iter()
                .map(|d| (
                    d.task.dest.clone(),
                    d.task.url.clone(),
                    d.task.size,
                    d.task.sha1.clone(),
                    d.task.credential.is_some()
                ))
                .collect::<Vec<_>>(),
            vec![
                (
                    mods.join("jei.jar"),
                    format!("{uri}/f/11"),
                    Some(3),
                    Some(sha1_hex(b"jei")),
                    true
                ),
                (
                    mods.join("blocked-a.jar"),
                    format!("{uri}{derived}"),
                    Some(BLOCKED_A.len() as u64),
                    Some(sha1_hex(BLOCKED_A)),
                    false
                ),
            ]
        );

        petalfall
            .import_modpack(prepared, "task-derived", None)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read(mods.join("blocked-a.jar")).unwrap(),
            BLOCKED_A
        );
        assert_eq!(std::fs::read(mods.join("jei.jar")).unwrap(), b"jei");
        assert_eq!(api_keys_sent(&server, "GET", &derived).await, vec![None]);
        assert_eq!(
            api_keys_sent(&server, "GET", "/f/11").await,
            vec![Some("k".to_owned())],
            "凭据作用域确实指着这个假服务器"
        );
        server.verify().await;
    }

    /// 设置开着、CDN 也不给（探测回 404、410）：照旧报受限，清单与设置关着时一字不差；每个探过一次，没去下。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn restricted_files_the_cdn_refuses_stay_blocked_on_import() {
        let server = MockServer::start().await;
        mount_restricted_platform(
            &server,
            vec![
                cf_file_with(55012, 5, "blocked-a.jar", None, BLOCKED_A),
                cf_file(66013, 6, "blocked-b.jar", None),
            ],
            &[(5, 55012), (6, 66013)],
        )
        .await;
        mount_probe(&server, 55012, "blocked-a.jar", 404, 1u64).await;
        mount_probe(&server, 66013, "blocked-b.jar", 410, 1u64).await;
        mount_derived_file(&server, 55012, "blocked-a.jar", BLOCKED_A, 0).await;
        mount_derived_file(&server, 66013, "blocked-b.jar", b"b", 0).await;
        let tmp = tempfile::tempdir().unwrap();
        let pack = restricted_pack(tmp.path(), "受限包.zip", &[(5, 55012), (6, 66013)], &[]);
        let mut petalfall = derive_petalfall(tmp.path(), &server, false);
        let off = blocked_on_prepare(&petalfall, &pack).await;
        petalfall.set_download_restricted_curseforge_files(true);

        let on = blocked_on_prepare(&petalfall, &pack).await;
        assert_eq!(on, off);
        assert_eq!(
            on.iter()
                .map(|f| (f.file_id, f.destination.as_str()))
                .collect::<Vec<_>>(),
            vec![(55012, "mods/blocked-a.jar"), (66013, "mods/blocked-b.jar")]
        );
        server.verify().await;
    }

    /// 设置开着、探测回 500：照常报错，不当成「不能下」让用户去手动下载，也没去下。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_failing_probe_fails_the_import_instead_of_asking_for_a_manual_download() {
        let server = MockServer::start().await;
        mount_restricted_platform(
            &server,
            vec![cf_file_with(55012, 5, "blocked-a.jar", None, BLOCKED_A)],
            &[(5, 55012)],
        )
        .await;
        mount_probe(&server, 55012, "blocked-a.jar", 500, 1u64..).await;
        mount_derived_file(&server, 55012, "blocked-a.jar", BLOCKED_A, 0).await;
        let tmp = tempfile::tempdir().unwrap();
        let pack = restricted_pack(tmp.path(), "受限包.zip", &[(5, 55012)], &[]);
        let petalfall = derive_petalfall(tmp.path(), &server, true);

        let err = petalfall
            .prepare_modpack(
                &ModpackSource::File { path: pack },
                None,
                &ImportSelection::default(),
                None,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(
                &err,
                CoreError::ModPlatform(petalfall_modplatform::Error::Status { status: 500, .. })
            ),
            "{err:?}"
        );
        server.verify().await;
    }

    /// 一个包里的受限文件有拼得到的也有拼不到的：设置开着时清单只列拼不到的（CDN 回 404 的、编号小于 1000
    /// 拼不出地址的），拼得到的不列。去掉拼不到的再准备就成了：文件对象自带地址与 download-url 端点给的地址
    /// 照常挂凭据，只有拼出来的不挂；端点给了地址的、拼不出地址的都不探测。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn only_restricted_files_that_cannot_be_derived_are_listed_on_import() {
        let server = MockServer::start().await;
        let uri = server.uri();
        mount_restricted_platform(
            &server,
            vec![
                cf_file(11, 1, "jei.jar", Some(format!("{uri}/f/11"))),
                cf_file(22012, 2, "endpoint.jar", None),
                cf_file_with(55012, 5, "blocked-a.jar", None, BLOCKED_A),
                cf_file(77012, 7, "blocked-c.jar", None),
                cf_file(66, 6, "blocked-small.jar", None),
            ],
            &[(5, 55012), (7, 77012), (6, 66)],
        )
        .await;
        Mock::given(method("GET"))
            .and(path("/v1/mods/2/files/22012/download-url"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "data": format!("{uri}/ep/22012") })),
            )
            .mount(&server)
            .await;
        mount_probe(&server, 55012, "blocked-a.jar", 200, 2u64).await;
        mount_probe(&server, 77012, "blocked-c.jar", 404, 1u64).await;
        let tmp = tempfile::tempdir().unwrap();
        let petalfall = derive_petalfall(tmp.path(), &server, true);
        let all = restricted_pack(
            tmp.path(),
            "全部.zip",
            &[(1, 11), (2, 22012), (5, 55012), (7, 77012), (6, 66)],
            &[],
        );

        let files = blocked_on_prepare(&petalfall, &all).await;
        assert_eq!(
            files
                .iter()
                .map(|f| (f.file_id, f.destination.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (77012, "mods/blocked-c.jar"),
                (66, "mods/blocked-small.jar")
            ]
        );

        let derivable = restricted_pack(
            tmp.path(),
            "拼得到.zip",
            &[(1, 11), (2, 22012), (5, 55012)],
            &[],
        );
        let prepared = petalfall
            .prepare_modpack(
                &ModpackSource::File { path: derivable },
                None,
                &ImportSelection::default(),
                None,
            )
            .await
            .unwrap();
        let derived = derived_path(55012, "blocked-a.jar");
        assert_eq!(
            prepared
                .remote
                .iter()
                .map(|d| (d.task.url.clone(), d.task.credential.is_some()))
                .collect::<Vec<_>>(),
            vec![
                (format!("{uri}/f/11"), true),
                (format!("{uri}/ep/22012"), true),
                (format!("{uri}{derived}"), false),
            ]
        );
        prepared.discard().await;
        let probed: Vec<String> = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|request| request.method.as_str() == "HEAD")
            .map(|request| request.url.path().to_owned())
            .collect();
        assert_eq!(
            probed,
            vec![
                derived.clone(),
                derived_path(77012, "blocked-c.jar"),
                derived
            ]
        );
        server.verify().await;
    }

    /// 落点被覆盖层盖住的受限文件，设置开着时导入也不为它拼地址、探测：它反正会被包里自带的那份盖掉。
    /// download-url 端点照旧要问，端点给了地址的被盖住文件照旧下载（挂凭据），随后被盖掉。预览不知道覆盖层，
    /// 照样探一次、不列它；准备与导入一次都不探，拼出的地址一次都不下，导入成功，落点都是包里自带的内容。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn covered_restricted_files_are_not_probed_on_import_when_the_setting_is_on() {
        let server = MockServer::start().await;
        mount_vanilla(&server).await;
        let uri = server.uri();
        mount_restricted_platform(
            &server,
            vec![
                cf_file_with(55012, 5, "covered.jar", None, b"cdn jar"),
                cf_file_with(22012, 2, "covered-ep.jar", None, b"ep jar"),
            ],
            &[(5, 55012)],
        )
        .await;
        Mock::given(method("GET"))
            .and(path("/v1/mods/2/files/22012/download-url"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "data": format!("{uri}/ep/22012") })),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/ep/22012"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"ep jar".to_vec()))
            .expect(1)
            .mount(&server)
            .await;
        mount_probe(&server, 55012, "covered.jar", 200, 1u64).await;
        mount_derived_file(&server, 55012, "covered.jar", b"cdn jar", 0).await;
        let tmp = tempfile::tempdir().unwrap();
        let pack = restricted_pack(
            tmp.path(),
            "自带.zip",
            &[(5, 55012), (2, 22012)],
            &[
                ("overrides/mods/covered.jar", b"bundled jar"),
                ("overrides/mods/covered-ep.jar", b"bundled ep"),
            ],
        );
        let petalfall = derive_petalfall(tmp.path(), &server, true);
        let derived = derived_path(55012, "covered.jar");

        let preview = petalfall.preview_modpack_file(&pack).await.unwrap();
        assert_eq!(preview.blocked_files, Vec::new());
        assert_eq!(
            preview
                .mods
                .iter()
                .map(|m| (m.file_name.as_str(), m.availability))
                .collect::<Vec<_>>(),
            vec![
                ("covered-ep.jar", PackFileAvailability::Bundled),
                ("covered.jar", PackFileAvailability::Bundled)
            ]
        );
        assert_eq!(api_keys_sent(&server, "HEAD", &derived).await, vec![None]);

        let prepared = petalfall
            .prepare_modpack(
                &ModpackSource::File { path: pack },
                Some("自带"),
                &ImportSelection::default(),
                None,
            )
            .await
            .unwrap();
        assert!(prepared.claimed.is_empty());
        assert_eq!(
            prepared
                .remote
                .iter()
                .map(|d| (d.task.url.clone(), d.task.credential.is_some()))
                .collect::<Vec<_>>(),
            vec![(format!("{uri}/ep/22012"), true)]
        );
        let outcome = petalfall
            .import_modpack(prepared, "task-covered-derived", None)
            .await
            .unwrap();
        let mods = outcome.version_dir.join("mods");
        assert_eq!(
            std::fs::read(mods.join("covered.jar")).unwrap(),
            b"bundled jar"
        );
        assert_eq!(
            std::fs::read(mods.join("covered-ep.jar")).unwrap(),
            b"bundled ep"
        );
        assert_eq!(
            api_keys_sent(&server, "HEAD", &derived).await,
            vec![None],
            "准备与导入没有再探"
        );
        assert_eq!(
            api_keys_sent(&server, "GET", "/v1/mods/5/files/55012/download-url")
                .await
                .len(),
            2,
            "预览与准备都问过 download-url 端点"
        );
        server.verify().await;
    }

    /// 文件对象回 `"downloadUrl": ""`：预览与导入都把空串当成没给地址。设置关着时预览把它列进 blocked_files、
    /// 准备报受限，两边清单一字不差，拼出的地址一次都不碰；设置开着、拼出的地址探得到时两边都不算受限：预览
    /// 标为要下载，准备出的是按拼出地址下载、不挂凭据的任务。探测只在打开后发，预览与准备各一次。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn blank_download_urls_count_as_missing_in_both_preview_and_import() {
        let server = MockServer::start().await;
        mount_restricted_platform(
            &server,
            vec![cf_file_with(
                55012,
                5,
                "blocked-a.jar",
                Some(String::new()),
                BLOCKED_A,
            )],
            &[(5, 55012)],
        )
        .await;
        mount_probe(&server, 55012, "blocked-a.jar", 200, 2u64).await;
        mount_derived_file(&server, 55012, "blocked-a.jar", BLOCKED_A, 0).await;
        let tmp = tempfile::tempdir().unwrap();
        let pack = restricted_pack(tmp.path(), "受限包.zip", &[(5, 55012)], &[]);
        let mut petalfall = derive_petalfall(tmp.path(), &server, false);
        let derived = derived_path(55012, "blocked-a.jar");

        let preview = petalfall.preview_modpack_file(&pack).await.unwrap();
        let on_import = blocked_on_prepare(&petalfall, &pack).await;
        assert_eq!(preview.blocked_files, on_import);
        assert_eq!(
            on_import
                .iter()
                .map(|f| (f.file_id, f.destination.as_str()))
                .collect::<Vec<_>>(),
            vec![(55012, "mods/blocked-a.jar")]
        );
        assert_eq!(
            api_keys_sent(&server, "HEAD", &derived).await,
            Vec::<Option<String>>::new()
        );

        petalfall.set_download_restricted_curseforge_files(true);
        let preview = petalfall.preview_modpack_file(&pack).await.unwrap();
        assert_eq!(preview.blocked_files, Vec::new());
        assert_eq!(
            preview
                .mods
                .iter()
                .map(|m| (m.file_name.as_str(), m.availability))
                .collect::<Vec<_>>(),
            vec![("blocked-a.jar", PackFileAvailability::Download)]
        );
        let prepared = petalfall
            .prepare_modpack(
                &ModpackSource::File { path: pack },
                None,
                &ImportSelection::default(),
                None,
            )
            .await
            .unwrap();
        assert!(prepared.claimed.is_empty());
        assert_eq!(
            prepared
                .remote
                .iter()
                .map(|d| (d.task.url.clone(), d.task.credential.is_some()))
                .collect::<Vec<_>>(),
            vec![(format!("{}{derived}", server.uri()), false)]
        );
        prepared.discard().await;
        assert_eq!(
            api_keys_sent(&server, "HEAD", &derived).await,
            vec![None, None]
        );
        server.verify().await;
    }

    /// 设置开着、拼出的地址能下时不用暂存位里认领过的副本。同一个包设置关着时预览标为已认领、准备出的是复制
    /// 副本（副本确实校验得过）；打开后预览标为要下载，准备出的是按拼出地址下载的任务、没有要复制的副本，
    /// 导入时真的发了下载请求。探测发了两次：打开后的预览与准备各一次。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn derived_downloads_win_over_claimed_copies() {
        let server = MockServer::start().await;
        mount_vanilla(&server).await;
        mount_restricted_platform(
            &server,
            vec![cf_file_with(55012, 5, "blocked-a.jar", None, BLOCKED_A)],
            &[(5, 55012)],
        )
        .await;
        mount_probe(&server, 55012, "blocked-a.jar", 200, 2u64).await;
        mount_derived_file(&server, 55012, "blocked-a.jar", BLOCKED_A, 1).await;
        let tmp = tempfile::tempdir().unwrap();
        let pack = restricted_pack(tmp.path(), "受限包.zip", &[(5, 55012)], &[]);
        let source = ModpackSource::File { path: pack.clone() };
        let availability = |preview: &ModpackPreview| {
            assert_eq!(preview.blocked_files, Vec::new());
            preview
                .mods
                .iter()
                .map(|m| (m.file_name.clone(), m.availability))
                .collect::<Vec<_>>()
        };
        let mut petalfall = derive_petalfall(tmp.path(), &server, false);
        let slot = petalfall
            .blocked_claims_dir()
            .join("curseforge-55012")
            .join("blocked-a.jar");
        std::fs::create_dir_all(slot.parent().unwrap()).unwrap();
        std::fs::write(&slot, BLOCKED_A).unwrap();
        let dest = petalfall
            .game_dir()
            .join("versions")
            .join("受限包")
            .join("mods")
            .join("blocked-a.jar");
        let none = ImportSelection::default();

        let preview = petalfall.preview_modpack_file(&pack).await.unwrap();
        assert_eq!(
            availability(&preview),
            vec![("blocked-a.jar".to_owned(), PackFileAvailability::Claimed)]
        );
        let prepared = petalfall
            .prepare_modpack(&source, None, &none, None)
            .await
            .unwrap();
        assert_eq!(
            prepared.claimed,
            vec![ClaimedCopy {
                staged: slot.clone(),
                dest: dest.clone()
            }]
        );
        assert!(prepared.remote.is_empty());
        prepared.discard().await;

        petalfall.set_download_restricted_curseforge_files(true);
        let preview = petalfall.preview_modpack_file(&pack).await.unwrap();
        assert_eq!(
            availability(&preview),
            vec![("blocked-a.jar".to_owned(), PackFileAvailability::Download)]
        );
        let prepared = petalfall
            .prepare_modpack(&source, None, &none, None)
            .await
            .unwrap();
        assert!(prepared.claimed.is_empty());
        assert_eq!(
            prepared
                .remote
                .iter()
                .map(|d| (d.task.dest.clone(), d.task.url.clone()))
                .collect::<Vec<_>>(),
            vec![(
                dest.clone(),
                format!("{}{}", server.uri(), derived_path(55012, "blocked-a.jar"))
            )]
        );
        petalfall
            .import_modpack(prepared, "task-derived-claim", None)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), BLOCKED_A);
        server.verify().await;
    }

    /// 预览、设置关着：受限文件照旧列进清单，拼出的地址一次都不碰（假服务器上它明明能下）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn preview_does_not_probe_restricted_files_while_the_setting_is_off() {
        let server = MockServer::start().await;
        mount_restricted_platform(
            &server,
            vec![cf_file_with(55012, 5, "blocked-a.jar", None, BLOCKED_A)],
            &[(5, 55012)],
        )
        .await;
        mount_probe(&server, 55012, "blocked-a.jar", 200, 0u64).await;
        mount_derived_file(&server, 55012, "blocked-a.jar", BLOCKED_A, 0).await;
        let tmp = tempfile::tempdir().unwrap();
        let pack = restricted_pack(tmp.path(), "受限包.zip", &[(5, 55012)], &[]);

        let preview = derive_petalfall(tmp.path(), &server, false)
            .preview_modpack_file(&pack)
            .await
            .unwrap();
        assert_eq!(
            preview
                .blocked_files
                .iter()
                .map(|f| (f.file_id, f.destination.as_str()))
                .collect::<Vec<_>>(),
            vec![(55012, "mods/blocked-a.jar")]
        );
        server.verify().await;
    }

    /// 预览与导入同一口径：设置开着时探得到的受限文件不列进清单、在 Mod 清单里标为要下载；CDN 不给的
    /// （探测回 404、403）照旧列，内容与设置关着时一字不差。预览只探不下。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn preview_lists_only_restricted_files_the_cdn_refuses_when_the_setting_is_on() {
        let server = MockServer::start().await;
        let uri = server.uri();
        mount_restricted_platform(
            &server,
            vec![
                cf_file(11, 1, "jei.jar", Some(format!("{uri}/f/11"))),
                cf_file_with(55012, 5, "blocked-a.jar", None, BLOCKED_A),
                cf_file(77012, 7, "blocked-c.jar", None),
                cf_file(88012, 8, "blocked-d.jar", None),
            ],
            &[(5, 55012), (7, 77012), (8, 88012)],
        )
        .await;
        mount_probe(&server, 55012, "blocked-a.jar", 200, 1u64).await;
        mount_probe(&server, 77012, "blocked-c.jar", 404, 1u64).await;
        mount_probe(&server, 88012, "blocked-d.jar", 403, 1u64).await;
        mount_derived_file(&server, 55012, "blocked-a.jar", BLOCKED_A, 0).await;
        let tmp = tempfile::tempdir().unwrap();
        let pack = restricted_pack(
            tmp.path(),
            "受限包.zip",
            &[(1, 11), (5, 55012), (7, 77012), (8, 88012)],
            &[],
        );
        let mut petalfall = derive_petalfall(tmp.path(), &server, false);
        let off = petalfall.preview_modpack_file(&pack).await.unwrap();
        assert_eq!(
            off.blocked_files
                .iter()
                .map(|f| f.file_id)
                .collect::<Vec<_>>(),
            vec![55012, 77012, 88012]
        );
        petalfall.set_download_restricted_curseforge_files(true);

        let on = petalfall.preview_modpack_file(&pack).await.unwrap();
        assert_eq!(on.blocked_files, off.blocked_files[1..].to_vec());
        assert_eq!(
            on.mods
                .iter()
                .map(|m| (m.file_name.as_str(), m.availability))
                .collect::<Vec<_>>(),
            vec![
                ("blocked-a.jar", PackFileAvailability::Download),
                ("blocked-c.jar", PackFileAvailability::Blocked),
                ("blocked-d.jar", PackFileAvailability::Blocked),
                ("jei.jar", PackFileAvailability::Download),
            ]
        );
        server.verify().await;
    }

    /// 预览、设置开着、探测回 500：预览照常报错，不把它当成要手动下载的文件列出来。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn preview_fails_when_probing_a_restricted_file_errors() {
        let server = MockServer::start().await;
        mount_restricted_platform(
            &server,
            vec![cf_file_with(55012, 5, "blocked-a.jar", None, BLOCKED_A)],
            &[(5, 55012)],
        )
        .await;
        mount_probe(&server, 55012, "blocked-a.jar", 500, 1u64..).await;
        let tmp = tempfile::tempdir().unwrap();
        let pack = restricted_pack(tmp.path(), "受限包.zip", &[(5, 55012)], &[]);

        let err = derive_petalfall(tmp.path(), &server, true)
            .preview_modpack_file(&pack)
            .await
            .unwrap_err();
        assert!(
            matches!(
                &err,
                CoreError::ModPlatform(petalfall_modplatform::Error::Status { status: 500, .. })
            ),
            "{err:?}"
        );
        server.verify().await;
    }

    /// 整合包本体受限、设置开着：不用认领，按探测交回的跳转终点直接下到包并导入。假 CDN 照线上的脾气：拼出的
    /// 地址只会跳转、对带 Range 的请求回 404。拼出的地址只收到探测那一个 HEAD，下载打在终点上；CDN 上的请求都
    /// 不带 x-api-key（凭据作用域指着这个假服务器，任务误挂了凭据就会被看见）。导入后不留下载缓存，也不用暂存位。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn restricted_curseforge_pack_file_is_downloaded_without_a_claim_when_the_setting_is_on()
    {
        let server = MockServer::start().await;
        mount_vanilla(&server).await;
        let tmp = tempfile::tempdir().unwrap();
        let built = tmp.path().join("built.zip");
        write_curseforge_pack(
            &built,
            &curseforge_vanilla_manifest(serde_json::json!([])),
            &[("overrides/options.txt", b"fov:70")],
        );
        let pack_bytes = std::fs::read(&built).unwrap();
        Mock::given(method("GET"))
            .and(path("/v1/mods/700/files/7001"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": cf_file_with(7001, 700, "locked-pack-1.0.zip", None, &pack_bytes)
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/mods/700/files/7001/download-url"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        let edge = derived_path(7001, "locked-pack-1.0.zip");
        let media = format!("/mediafilez{edge}");
        crate::modpack_summary::tests::mount_edge_cdn(&server, &edge, &media, pack_bytes).await;
        let petalfall = derive_petalfall(tmp.path(), &server, true);
        let source = ModpackSource::Platform {
            platform: Platform::CurseForge,
            project_id: "700".into(),
            version_id: "7001".into(),
        };

        let outcome = petalfall
            .install_modpack(
                &source,
                Some("拼地址下的包"),
                &ImportSelection::default(),
                "task-derived-pack",
                None,
            )
            .await
            .unwrap();

        assert_eq!(outcome.modpack.platform, Some(Platform::CurseForge));
        assert_eq!(outcome.modpack.version_id.as_deref(), Some("7001"));
        assert_eq!(
            std::fs::read(outcome.version_dir.join("options.txt")).unwrap(),
            b"fov:70"
        );
        let cdn = crate::modpack_summary::tests::cdn_requests(&server, &[&edge, &media]).await;
        assert_eq!(
            cdn,
            vec![
                ("HEAD".to_owned(), edge.clone(), false, false),
                ("HEAD".to_owned(), media.clone(), false, false),
                ("GET".to_owned(), media.clone(), false, false),
            ],
            "拼出的地址只被探测，包从跳转终点下载"
        );
        assert_eq!(
            pack_cache_entries(tmp.path()),
            Vec::<String>::new(),
            "导入后不留下载缓存"
        );
        assert!(
            !petalfall
                .blocked_claims_dir()
                .join("curseforge-7001")
                .exists(),
            "没走认领"
        );
    }

    #[test]
    fn forge_versions_with_minecraft_prefix_are_trimmed() {
        assert_eq!(strip_minecraft_prefix("1.20.1-47.2.0", "1.20.1"), "47.2.0");
        assert_eq!(strip_minecraft_prefix("47.2.0", "1.20.1"), "47.2.0");
        assert_eq!(strip_minecraft_prefix("1.20.10-1", "1.20.1"), "1.20.10-1");
    }

    // ---- 游戏目录的数据格式记号 ----

    use crate::data_format::STAMP_FILE;
    use crate::data_format::testing::{GuardFixture, with_fixture};
    use std::collections::BTreeMap;

    /// 测试里当作这一版的数据格式号：号为 1 时一个记号都不写，看不出写没写。
    fn own() -> GuardFixture {
        GuardFixture { own: 2, look: None }
    }

    fn stamp_file(game_dir: &Path) -> PathBuf {
        game_dir.join(PETALFALL_META_DIR).join(STAMP_FILE)
    }

    /// `root` 下的全部内容：目录记成 `None`，文件记成字节。
    fn tree(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
        fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                let rel = path.strip_prefix(root).unwrap().to_owned();
                if path.is_dir() {
                    out.insert(rel, None);
                    walk(root, &path, out);
                } else {
                    out.insert(rel, Some(std::fs::read(&path).unwrap()));
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(root, root, &mut out);
        out
    }

    /// 当前游戏目录被更新的落樱用过：导入在建实例目录之前就报错，不下载游戏文件，游戏目录一个文件都不多；平台
    /// 整合包的下载缓存照样删掉。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn importing_into_a_newer_game_dir_downloads_and_writes_nothing() {
        let server = MockServer::start().await;
        mount_vanilla(&server).await;
        let tmp = tempfile::tempdir().unwrap();
        let built = tmp.path().join("built.mrpack");
        write_pack(
            &built,
            &index(serde_json::json!({"minecraft": "test"})),
            &[("overrides/options.txt", b"fov:90")],
        );
        let bytes = std::fs::read(&built).unwrap();
        mount_platform_pack(&server, "pv-newer", &sha1_hex(&bytes), bytes).await;
        let petalfall = petalfall(tmp.path(), &server);
        let game_dir = petalfall.game_dir().to_owned();
        std::fs::create_dir_all(game_dir.join(PETALFALL_META_DIR)).unwrap();
        std::fs::write(
            stamp_file(&game_dir),
            r#"{"data_format":3,"written_by":"1.1.0-dev.57"}"#,
        )
        .unwrap();
        let prepared = petalfall
            .prepare_modpack(
                &platform_source("pv-newer"),
                Some("被挡下"),
                &ImportSelection::default(),
                None,
            )
            .await
            .unwrap();
        let before = tree(&game_dir);

        let err = with_fixture(own(), petalfall.import_modpack(prepared, "task-n", None))
            .await
            .unwrap_err();

        match err {
            CoreError::GameDirDataTooNew {
                path,
                found,
                supported,
                written_by,
            } => {
                assert_eq!(path, game_dir);
                assert_eq!((found, supported), (3, 2));
                assert_eq!(written_by.as_deref(), Some("1.1.0-dev.57"));
            }
            other => panic!("应拒绝导入，实际是 {other:?}"),
        }
        assert_eq!(tree(&game_dir), before);
        let fetched: Vec<String> = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .map(|request| request.url.path().to_owned())
            .collect();
        assert_eq!(
            fetched,
            ["/version/pv-newer", "/files/pv-newer.mrpack"],
            "只下了整合包本身"
        );
        assert_eq!(pack_cache_entries(tmp.path()), Vec::<String>::new());
    }

    /// 游戏目录原本不存在：导入把它建出来并补上记号，成功后记号是这一版的号。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_game_dir_created_by_an_import_gets_the_stamp() {
        let server = MockServer::start().await;
        mount_vanilla(&server).await;
        let tmp = tempfile::tempdir().unwrap();
        let pack = tmp.path().join("测试包.mrpack");
        write_pack(
            &pack,
            &index(serde_json::json!({"minecraft": "test"})),
            &[("overrides/options.txt", b"fov:90")],
        );
        let petalfall = petalfall(tmp.path(), &server);
        let game_dir = petalfall.game_dir().to_owned();
        assert!(!game_dir.exists());

        let outcome = with_fixture(
            own(),
            petalfall.install_modpack(
                &ModpackSource::File { path: pack },
                Some("新目录"),
                &ImportSelection::default(),
                "task-s",
                None,
            ),
        )
        .await
        .unwrap();

        assert_eq!(
            outcome.version_dir,
            game_dir.join("versions").join("新目录")
        );
        assert_eq!(
            std::fs::read(outcome.version_dir.join("options.txt")).unwrap(),
            b"fov:90"
        );
        assert_eq!(
            std::fs::read_to_string(stamp_file(&game_dir)).unwrap(),
            r#"{"data_format":2}"#
        );
    }

    use crate::data_format::DirLook;
    use crate::data_format::testing::LookFn;
    use crate::instances::tests::{NEWER_STAMP, NEWER_WRITER, disk_state, write_stamp};
    use petalfall_release::data_format::{StampFile, parse_stamp};
    use std::sync::{Arc, Mutex};

    /// 游戏目录原本不存在、导入在装游戏这一步失败：记号在建好 `versions/` 之后、建实例目录之前就写下了。核对时顺带
    /// 看一眼实例目录：第一次看到游戏目录在的那次核对（补记号的那次）实例目录还不在；失败后实例目录照旧删掉，记号留着。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_game_dir_created_by_an_import_gets_the_stamp_before_any_instance_data() {
        // 只挂版本清单、不挂原版 JSON：装游戏失败。
        let server = MockServer::start().await;
        let base = server.uri();
        Mock::given(method("GET"))
            .and(path("/manifest.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(format!(
                r#"{{"latest":{{"release":"test","snapshot":"test"}},
                    "versions":[{{"id":"test","type":"release","url":"{base}/test.json",
                    "time":"t","releaseTime":"t"}}]}}"#
            )))
            .mount(&server)
            .await;
        let tmp = tempfile::tempdir().unwrap();
        let pack = tmp.path().join("测试包.mrpack");
        write_pack(
            &pack,
            &index(serde_json::json!({"minecraft": "test"})),
            &[("overrides/options.txt", b"fov:90")],
        );
        let petalfall = petalfall(tmp.path(), &server);
        let game_dir = petalfall.game_dir().to_owned();
        assert!(!game_dir.exists());
        let instance = game_dir.join("versions").join("中途失败");
        let seen: Arc<Mutex<Vec<bool>>> = Arc::default();
        let look: LookFn = {
            let (seen, instance) = (seen.clone(), instance.clone());
            Arc::new(move |dir: &Path| {
                let look = if dir.exists() {
                    seen.lock().unwrap().push(instance.exists());
                    DirLook::Stamp(match std::fs::read(stamp_file(dir)) {
                        Ok(bytes) => parse_stamp(StampFile::Contents(&bytes)),
                        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                            parse_stamp(StampFile::Missing)
                        }
                        Err(err) => panic!("读记号失败：{err}"),
                    })
                } else {
                    DirLook::Absent
                };
                Box::pin(async move { look })
            })
        };

        let err = with_fixture(
            GuardFixture {
                own: 2,
                look: Some(look),
            },
            petalfall.install_modpack(
                &ModpackSource::File { path: pack },
                Some("中途失败"),
                &ImportSelection::default(),
                "task-f",
                None,
            ),
        )
        .await
        .unwrap_err();

        assert!(
            !matches!(
                err,
                CoreError::GameDirDataTooNew { .. } | CoreError::DataFormatUnreadable { .. }
            ),
            "应在装游戏时失败：{err:?}"
        );
        assert_eq!(
            seen.lock().unwrap().first(),
            Some(&false),
            "补记号时实例目录还不在"
        );
        assert_eq!(
            std::fs::read_to_string(stamp_file(&game_dir)).unwrap(),
            r#"{"data_format":2}"#
        );
        assert!(!instance.exists(), "失败后实例目录照旧删掉");
    }

    /// 游戏目录原本不存在：导入开头的核对看到目录不存在，建好 `versions/` 之后的第二次核对被拒，一种是别处刚写下了
    /// 更高的号，一种是这一刻够不着这个位置（网络盘断开）。导入报第二次核对的错误，不装游戏、不建实例目录、不写
    /// 记号：游戏目录里只有空的 `versions/`，模拟端点一个请求都没收到。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_refused_check_after_creating_the_game_dir_imports_nothing() {
        for case in ["更高的号", "够不着"] {
            let server = MockServer::start().await;
            mount_vanilla(&server).await;
            let tmp = tempfile::tempdir().unwrap();
            let pack = tmp.path().join("测试包.mrpack");
            write_pack(
                &pack,
                &index(serde_json::json!({"minecraft": "test"})),
                &[("overrides/options.txt", b"fov:90")],
            );
            let petalfall = petalfall(tmp.path(), &server);
            let game_dir = petalfall.game_dir().to_owned();
            let instance = game_dir.join("versions").join("被挡下");
            let prepared = petalfall
                .prepare_modpack(
                    &ModpackSource::File { path: pack },
                    Some("被挡下"),
                    &ImportSelection::default(),
                    None,
                )
                .await
                .unwrap();
            assert!(!game_dir.exists(), "{case}");
            // 游戏目录建出来之后的每次核对都记下那时实例目录在不在。
            let seen: Arc<Mutex<Vec<bool>>> = Arc::default();
            let look: LookFn = {
                let (seen, instance) = (seen.clone(), instance.clone());
                Arc::new(move |dir: &Path| {
                    let look = if !dir.exists() {
                        DirLook::Absent
                    } else {
                        seen.lock().unwrap().push(instance.exists());
                        match case {
                            "更高的号" => DirLook::Stamp(parse_stamp(StampFile::Contents(
                                NEWER_STAMP.as_bytes(),
                            ))),
                            _ => DirLook::Unreachable(std::io::Error::from_raw_os_error(53)),
                        }
                    };
                    Box::pin(async move { look })
                })
            };

            let err = with_fixture(
                GuardFixture {
                    own: 2,
                    look: Some(look),
                },
                petalfall.import_modpack(prepared, "task-r", None),
            )
            .await
            .unwrap_err();

            match (case, err) {
                (
                    "更高的号",
                    CoreError::GameDirDataTooNew {
                        path,
                        found,
                        supported,
                        written_by,
                    },
                ) => {
                    assert_eq!(path, game_dir);
                    assert_eq!((found, supported), (3, 2));
                    assert_eq!(written_by.as_deref(), Some(NEWER_WRITER));
                }
                ("够不着", CoreError::InvalidGameDirectory { path, problem }) => {
                    assert_eq!(path, game_dir);
                    assert_eq!(problem, crate::error::GameDirProblem::Unavailable);
                }
                (case, other) => panic!("{case}：应拒绝导入，实际是 {other:?}"),
            }
            assert_eq!(
                *seen.lock().unwrap(),
                [false],
                "{case}：建好目录之后只核对一次，那时实例目录还不在"
            );
            assert_eq!(
                tree(&game_dir),
                BTreeMap::from([(PathBuf::from("versions"), None)]),
                "{case}"
            );
            assert_eq!(server.received_requests().await.unwrap().len(), 0, "{case}");
        }
    }

    /// 导入没成留下的半成品「半成品」，带这次导入的未完成标记。两个收拾入口真走下去都会改盘：删目录、改写标记。
    async fn half_import(petalfall: &Petalfall) {
        let dir = petalfall.game_dir().join(VERSIONS_DIR).join("半成品");
        std::fs::create_dir_all(&dir).unwrap();
        write_marker(&dir, "task-h").await.unwrap();
    }

    async fn call_cleanup(petalfall: &Petalfall, entry: &str) -> Result<()> {
        match entry {
            "discard_modpack_import" => petalfall
                .discard_modpack_import("半成品", &dir_was_created())
                .await
                .map(drop),
            "settle_modpack_import" => {
                petalfall
                    .settle_modpack_import("半成品", "task-h", MarkerSettlement::Cancelled)
                    .await
            }
            other => panic!("表里没有这个入口：{other}"),
        }
    }

    /// 导入失败后收拾半成品时，游戏目录已被更新的落樱用过：两个入口都报游戏目录太新，数据目录与游戏目录里的文件
    /// 内容、修改时间都不变，没有多出文件。
    #[tokio::test]
    async fn cleanup_after_a_failed_import_refuses_a_newer_game_dir_and_touches_nothing() {
        for entry in ["discard_modpack_import", "settle_modpack_import"] {
            let server = MockServer::start().await;
            let tmp = tempfile::tempdir().unwrap();
            let petalfall = petalfall(tmp.path(), &server);
            half_import(&petalfall).await;
            write_stamp(petalfall.game_dir(), NEWER_STAMP);
            let before = disk_state(tmp.path());

            let err = with_fixture(own(), call_cleanup(&petalfall, entry))
                .await
                .unwrap_err();

            match err {
                CoreError::GameDirDataTooNew {
                    path,
                    found,
                    supported,
                    written_by,
                } => {
                    assert_eq!(path, petalfall.game_dir(), "{entry}");
                    assert_eq!((found, supported), (3, 2), "{entry}");
                    assert_eq!(written_by.as_deref(), Some(NEWER_WRITER), "{entry}");
                }
                other => panic!("{entry} 应报游戏目录太新，实际是 {other:?}"),
            }
            assert_eq!(disk_state(tmp.path()), before, "{entry}");
        }
    }

    /// 不换本版号：记号读不懂与号是多少无关，生产的这条路同样在删任何东西之前拦下。
    #[tokio::test]
    async fn discarding_a_half_import_refuses_an_unreadable_stamp() {
        let server = MockServer::start().await;
        let tmp = tempfile::tempdir().unwrap();
        let petalfall = petalfall(tmp.path(), &server);
        half_import(&petalfall).await;
        write_stamp(petalfall.game_dir(), r#"{"data_format":"3"}"#);
        let before = disk_state(tmp.path());

        let err = petalfall
            .discard_modpack_import("半成品", &dir_was_created())
            .await
            .unwrap_err();

        assert!(
            matches!(&err, CoreError::DataFormatUnreadable { path, .. } if path == petalfall.game_dir()),
            "{err:?}"
        );
        assert_eq!(disk_state(tmp.path()), before);
    }

    /// 一步到位的导入：游戏目录被更新的落樱用过时，连平台整合包都不下载，一个请求都不发，什么都不写。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn installing_a_platform_pack_into_a_newer_game_dir_sends_no_request() {
        let server = MockServer::start().await;
        mount_vanilla(&server).await;
        let tmp = tempfile::tempdir().unwrap();
        let built = tmp.path().join("built.mrpack");
        write_pack(
            &built,
            &index(serde_json::json!({"minecraft": "test"})),
            &[("overrides/options.txt", b"fov:90")],
        );
        let bytes = std::fs::read(&built).unwrap();
        mount_platform_pack(&server, "pv-guard", &sha1_hex(&bytes), bytes).await;
        let petalfall = petalfall(tmp.path(), &server);
        write_stamp(petalfall.game_dir(), NEWER_STAMP);
        let before = disk_state(tmp.path());

        let err = with_fixture(
            own(),
            petalfall.install_modpack(
                &platform_source("pv-guard"),
                Some("被挡下"),
                &ImportSelection::default(),
                "task-g",
                None,
            ),
        )
        .await
        .unwrap_err();

        assert!(
            matches!(&err, CoreError::GameDirDataTooNew { path, found: 3, supported: 2, .. } if path == petalfall.game_dir()),
            "{err:?}"
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 0);
        assert_eq!(disk_state(tmp.path()), before);
    }

    /// 收下通道里已到的步骤事件：(步骤表, 下标)，按发出顺序；别的事件不要。
    fn drain_phases(
        rx: &mut tokio::sync::mpsc::UnboundedReceiver<CoreEvent>,
    ) -> Vec<(Vec<InstallPhase>, usize)> {
        let mut phases = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if let CoreEvent::Phase { plan, index } = event {
                phases.push((plan, index));
            }
        }
        phases
    }

    /// 步骤事件守协议：下标落在表内且严格变大；后一条可以带改写过的表，但前一条走到的那一段（`..=index`）原样不变。
    fn assert_phase_protocol(phases: &[(Vec<InstallPhase>, usize)]) {
        for (plan, index) in phases {
            assert!(*index < plan.len(), "下标越界：{phases:?}");
        }
        for pair in phases.windows(2) {
            let ((before, at), (after, next)) = (&pair[0], &pair[1]);
            assert!(next > at, "下标没有变大：{phases:?}");
            assert_eq!(
                after[..=*at],
                before[..=*at],
                "改写动了走过的步骤：{phases:?}"
            );
        }
    }

    /// 两张表逐项照契约的顺序：预排的表按原版包排、放文件与解覆盖文件都算上；确切的表在原版四步之后接加载器各步，
    /// 没有的步骤不排。
    #[test]
    fn import_plans_list_the_steps_in_contract_order() {
        use InstallPhase::*;
        let local = ModpackSource::File {
            path: PathBuf::from("pack.mrpack"),
        };
        let platform = platform_source("pv");
        assert_eq!(
            import_prelude_plan(&local),
            [
                ModpackRead,
                Metadata,
                Libraries,
                Assets,
                Natives,
                ModpackFiles,
                ModpackOverrides
            ]
        );
        assert_eq!(
            import_prelude_plan(&platform),
            [
                ModpackDownload,
                ModpackRead,
                Metadata,
                Libraries,
                Assets,
                Natives,
                ModpackFiles,
                ModpackOverrides
            ]
        );
        assert_eq!(
            import_plan(&local, None, false, true, false),
            [
                ModpackRead,
                Metadata,
                Libraries,
                Assets,
                Natives,
                ModpackOverrides
            ]
        );
        assert_eq!(
            import_plan(&platform, Some(PackLoaderKind::Fabric), true, true, true),
            [
                ModpackDownload,
                ModpackRead,
                Metadata,
                Libraries,
                Assets,
                Natives,
                LoaderProfile,
                LoaderLibraries,
                ModpackFiles,
                ModpackOverrides,
                Template
            ]
        );
        assert_eq!(
            import_plan(&local, Some(PackLoaderKind::NeoForge), true, false, true),
            [
                ModpackRead,
                Metadata,
                Libraries,
                Assets,
                Natives,
                Java,
                LoaderInstaller,
                LoaderLibraries,
                LoaderProcessors,
                ModpackFiles,
                Template
            ]
        );
        // 没有文件要放、没有覆盖文件、不套默认设置：只剩拿包、读包与装原版。
        assert_eq!(
            import_plan(&platform, None, false, false, false),
            [
                ModpackDownload,
                ModpackRead,
                Metadata,
                Libraries,
                Assets,
                Natives
            ]
        );
    }

    /// 每种来源、加载器与有无文件、覆盖文件、模板的组合：确切的表与预排的表在拿包、读包一段相同（平台 2 步、
    /// 本地 1 步），其后是原版四步、加载器各步与按有无排的收尾几步；读包之后改写得了，改写后整张表能按顺序
    /// 报完（某步重复出现或改动了走过的步骤都会在这里崩）。
    #[test]
    fn import_plans_keep_the_prelude_prefix_for_every_pack_shape() {
        use InstallPhase::*;
        let local = ModpackSource::File {
            path: PathBuf::from("pack.mrpack"),
        };
        let platform = platform_source("pv");
        let loaders = [
            None,
            Some(PackLoaderKind::Fabric),
            Some(PackLoaderKind::Quilt),
            Some(PackLoaderKind::Forge),
            Some(PackLoaderKind::NeoForge),
        ];
        for (source, fetched) in [
            (&local, vec![ModpackRead]),
            (&platform, vec![ModpackDownload, ModpackRead]),
        ] {
            let prelude = import_prelude_plan(source);
            assert_eq!(prelude[..fetched.len()], fetched[..]);
            for loader in loaders {
                for shape in 0..8u8 {
                    let (files, overrides, template) =
                        (shape & 1 != 0, shape & 2 != 0, shape & 4 != 0);
                    let plan = import_plan(source, loader, files, overrides, template);
                    let case = format!(
                        "{source:?} {loader:?} files={files} overrides={overrides} template={template}"
                    );
                    assert_eq!(plan[..fetched.len()], prelude[..fetched.len()], "{case}");
                    let rest = &plan[fetched.len()..];
                    assert_eq!(rest[..4], [Metadata, Libraries, Assets, Natives], "{case}");
                    let loader_steps: &[InstallPhase] = match loader {
                        None => &[],
                        Some(PackLoaderKind::Fabric | PackLoaderKind::Quilt) => {
                            &[LoaderProfile, LoaderLibraries]
                        }
                        Some(PackLoaderKind::Forge | PackLoaderKind::NeoForge) => {
                            &[Java, LoaderInstaller, LoaderLibraries, LoaderProcessors]
                        }
                    };
                    let tail: Vec<InstallPhase> = [
                        (files, ModpackFiles),
                        (overrides, ModpackOverrides),
                        (template, Template),
                    ]
                    .into_iter()
                    .filter_map(|(on, phase)| on.then_some(phase))
                    .collect();
                    assert_eq!(
                        rest[4..],
                        [loader_steps, tail.as_slice()].concat()[..],
                        "{case}"
                    );

                    let announcer = PhaseAnnouncer::new(None, prelude.clone());
                    for phase in &fetched {
                        announcer.enter(*phase);
                    }
                    let announcer = PhaseAnnouncer::resume(None, announcer.revise(plan.clone()));
                    for phase in rest {
                        announcer.enter(*phase);
                    }
                }
            }
        }
    }

    /// 一步到位导入带远程文件与覆盖文件的包：读包一步带预排的表，之后带确切的表一路报到最后一步（套默认设置），
    /// 每一步都报到、下标逐个变大，走过的步骤不被改写。不给事件出口时同一个包照常导入。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn importing_a_pack_reports_each_step_against_the_revised_plan() {
        use InstallPhase::*;
        let server = MockServer::start().await;
        mount_vanilla(&server).await;
        let uri = server.uri();
        Mock::given(method("POST"))
            .and(path("/v1/mods/files"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": [
                    cf_file_with(11, 1, "jei.jar", Some(format!("{uri}/f/11")), b"jei"),
                ]})),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/mods"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"data": [cf_mod(1, 6)]})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/f/11"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"jei".to_vec()))
            .mount(&server)
            .await;
        let tmp = tempfile::tempdir().unwrap();
        let pack = tmp.path().join("步骤包.zip");
        write_curseforge_pack(
            &pack,
            &curseforge_vanilla_manifest(serde_json::json!([
                { "projectID": 1, "fileID": 11, "required": true }
            ])),
            &[("overrides/config/a.txt", b"cfg")],
        );
        let petalfall = petalfall(tmp.path(), &server).with_curseforge_key("k");
        let source = ModpackSource::File { path: pack };
        let none = ImportSelection::default();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        let outcome = petalfall
            .install_modpack(&source, Some("报步骤"), &none, "task-steps", Some(&tx))
            .await
            .unwrap();
        drop(tx);
        let phases = drain_phases(&mut rx);

        let prelude = import_prelude_plan(&source);
        let exact = vec![
            ModpackRead,
            Metadata,
            Libraries,
            Assets,
            Natives,
            ModpackFiles,
            ModpackOverrides,
            Template,
        ];
        assert_ne!(
            prelude, exact,
            "确切的表要与预排的不同，才看得出读包之后改写了"
        );
        assert_eq!(phases[0], (prelude, 0));
        assert!(
            phases[1..].iter().all(|(plan, _)| *plan == exact),
            "{phases:?}"
        );
        assert_eq!(
            phases.iter().map(|(_, index)| *index).collect::<Vec<_>>(),
            (0..exact.len()).collect::<Vec<_>>()
        );
        assert_eq!(
            phases.last().map(|(plan, index)| plan[*index]),
            Some(Template)
        );
        assert_phase_protocol(&phases);
        let dir = outcome.version_dir;
        assert_eq!(
            std::fs::read(dir.join("mods").join("jei.jar")).unwrap(),
            b"jei"
        );
        assert_eq!(
            std::fs::read(dir.join("config").join("a.txt")).unwrap(),
            b"cfg"
        );

        let quiet = petalfall
            .install_modpack(&source, Some("不报步骤"), &none, "task-quiet", None)
            .await
            .unwrap();
        let dir = quiet.version_dir;
        assert_eq!(
            std::fs::read(dir.join("mods").join("jei.jar")).unwrap(),
            b"jei"
        );
        assert_eq!(
            std::fs::read(dir.join("config").join("a.txt")).unwrap(),
            b"cfg"
        );
        assert_eq!(
            (quiet.modpack.remote_files, quiet.modpack.override_files),
            (1, 1)
        );
    }

    /// 准备与落盘分两步调：准备一步只报下载整合包、读包（带预排的表），落盘一步带确切的表从装原版接着报，不从头
    /// 报；Fabric 的两步排在原版四步之后，一直报到套默认设置。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn prepare_then_import_continues_the_steps_instead_of_restarting() {
        use InstallPhase::*;
        let server = MockServer::start().await;
        mount_vanilla(&server).await;
        mount_fabric(&server).await;
        let uri = server.uri();
        let mut manifest =
            index(serde_json::json!({"minecraft": "test", "fabric-loader": "0.15.0"}));
        manifest["files"] = serde_json::json!([{
            "path": "mods/core.jar", "hashes": { "sha1": sha1_hex(b"core") },
            "downloads": ["https://cdn.modrinth.com/data/x/core.jar"], "fileSize": 4
        }]);
        Mock::given(method("GET"))
            .and(path("/cdn/core.jar"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"core".to_vec()))
            .mount(&server)
            .await;
        let tmp = tempfile::tempdir().unwrap();
        let built = tmp.path().join("built.mrpack");
        write_pack(&built, &manifest, &[("overrides/config/a.txt", b"cfg")]);
        let bytes = std::fs::read(&built).unwrap();
        mount_platform_pack(&server, "pv-steps", &sha1_hex(&bytes), bytes).await;
        let petalfall = petalfall(tmp.path(), &server);
        let source = platform_source("pv-steps");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        let mut prepared = petalfall
            .prepare_modpack(
                &source,
                Some("分两步"),
                &ImportSelection::default(),
                Some(&tx),
            )
            .await
            .unwrap();
        let prelude = import_prelude_plan(&source);
        let prepared_phases = drain_phases(&mut rx);
        assert_eq!(prepared_phases, vec![(prelude.clone(), 0), (prelude, 1)]);
        // 清单只认 https 地址，准备好之后把任务地址换到本地 mock 再导入。
        for download in &mut prepared.remote {
            download.task.url = format!("{uri}/cdn/core.jar");
        }
        let outcome = petalfall
            .import_modpack(prepared, "task-two-steps", Some(&tx))
            .await
            .unwrap();
        drop(tx);
        let imported = drain_phases(&mut rx);

        let exact = vec![
            ModpackDownload,
            ModpackRead,
            Metadata,
            Libraries,
            Assets,
            Natives,
            LoaderProfile,
            LoaderLibraries,
            ModpackFiles,
            ModpackOverrides,
            Template,
        ];
        assert!(
            imported.iter().all(|(plan, _)| *plan == exact),
            "{imported:?}"
        );
        assert_eq!(
            imported.iter().map(|(_, index)| *index).collect::<Vec<_>>(),
            (2..exact.len()).collect::<Vec<_>>(),
            "落盘一步从装原版接着报"
        );
        let all: Vec<_> = prepared_phases.into_iter().chain(imported).collect();
        assert_phase_protocol(&all);
        let dir = outcome.version_dir;
        assert_eq!(
            std::fs::read(dir.join("mods").join("core.jar")).unwrap(),
            b"core"
        );
        assert_eq!(
            std::fs::read(dir.join("config").join("a.txt")).unwrap(),
            b"cfg"
        );
        assert_eq!(
            outcome.modpack.loader.map(|loader| loader.kind),
            Some(PackLoaderKind::Fabric)
        );
    }

    /// 准备与落盘用的是同一游戏目录上两个配置不同的门面：报不报套默认设置只看准备时排定的表，套不套照旧看落盘
    /// 时的配置。准备时关、落盘时开：表里没有这一步，一路不报（不崩），默认设置照样补上；准备时开、落盘时关：
    /// 这一步照表报到，默认设置不补。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn template_step_follows_the_prepared_plan_when_the_config_changes_before_import() {
        use InstallPhase::*;
        let server = MockServer::start().await;
        mount_vanilla(&server).await;
        let tmp = tempfile::tempdir().unwrap();
        let pack = tmp.path().join("测试包.mrpack");
        write_pack(
            &pack,
            &index(serde_json::json!({"minecraft": "test"})),
            &[("overrides/options.txt", b"guiScale:3\nlang:en_us\n")],
        );
        let source = ModpackSource::File { path: pack };
        let none = ImportSelection::default();
        let mut off = petalfall(tmp.path(), &server);
        off.set_new_instance_template(false);
        let on = petalfall(tmp.path(), &server);
        assert!(on.config().new_instance_template);
        let servers: Vec<u8> = (0..fastrand::usize(1..2048))
            .map(|_| fastrand::u8(..))
            .collect();
        let template = on.template_dir();
        assert_eq!(template, off.template_dir());
        std::fs::create_dir_all(&template).unwrap();
        std::fs::write(template.join("servers.dat"), &servers).unwrap();
        let without_template = vec![
            ModpackRead,
            Metadata,
            Libraries,
            Assets,
            Natives,
            ModpackOverrides,
        ];
        let with_template = [&without_template[..], &[Template][..]].concat();

        for (prepare_with, import_with, name, exact, applied) in [
            (&off, &on, "准备时关", without_template, true),
            (&on, &off, "准备时开", with_template, false),
        ] {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let prepared = prepare_with
                .prepare_modpack(&source, Some(name), &none, Some(&tx))
                .await
                .unwrap();
            let outcome = import_with
                .import_modpack(prepared, &format!("task-{name}"), Some(&tx))
                .await
                .unwrap();
            drop(tx);
            let phases = drain_phases(&mut rx);

            assert_eq!(phases[0], (import_prelude_plan(&source), 0), "{name}");
            assert!(
                phases[1..].iter().all(|(plan, _)| *plan == exact),
                "{name}：{phases:?}"
            );
            // 报到的步骤与准备时排定的表逐项相同：准备时关的一路里没有 Template，准备时开的一路最后一步是它。
            assert_eq!(
                phases
                    .iter()
                    .map(|(plan, index)| plan[*index])
                    .collect::<Vec<_>>(),
                exact,
                "{name}"
            );
            assert_phase_protocol(&phases);
            let dir = outcome.version_dir;
            // 包里的设置留着，写着游戏默认语言的那一行在导入时去掉了。
            assert_eq!(
                std::fs::read(dir.join("options.txt")).unwrap(),
                b"guiScale:3\n",
                "{name}"
            );
            let copied = dir.join("servers.dat");
            if applied {
                assert_eq!(std::fs::read(&copied).unwrap(), servers, "{name}");
            } else {
                assert!(!copied.exists(), "{name}");
            }
        }
    }
}
