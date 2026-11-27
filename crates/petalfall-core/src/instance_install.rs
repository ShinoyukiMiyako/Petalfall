//! 按实例名新建实例，以及补全一个文件不全的实例。
//!
//! 新建分两步，中间由调用方登记任务：
//! 1. [`Petalfall::create_instance_dir`]：校验名称、原子地建出 `versions/<名>/`（并发重名在这一步暴露），
//!    写下未完成标记；
//! 2. [`Petalfall::install_instance`]：原版 JSON 与加载器 JSON 合并成一份以实例名为 id 的自洽 JSON，
//!    客户端 jar 与 natives 落在实例目录里，库与资源进共享目录。
//!
//! 结局由调用方记下：成功、失败经 [`Petalfall::settle_instance_marker`]（成功删标记，失败记下错误码与原文，
//! 界面据此显示「安装没有完成」），取消则经 [`Petalfall::abandon_instance`] 把整个目录丢掉。
//!
//! 补全先经 [`Petalfall::begin_repair`] 看标记定方案：标记里记着当初的安装内容（安装没完成）就照着重装，
//! 否则按现有版本 JSON 补齐缺失的文件；版本 JSON 被删或写坏时照装好时留下的安装记录重装。加载器自带的库
//! 补不回来，要经 [`Petalfall::begin_loader_reinstall`] 把版本 JSON 里读出的加载器原样重装一遍，走的是同一条
//! 安装链路。

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use petalfall_base::error_with_causes;
use petalfall_install::instance::place_json_for_processors;
use petalfall_install::{
    LoaderFlavor, LoaderInstaller, VanillaInstaller, ensure_complete, forge_installer_url,
    merge_for_instance, neoforge_installer_url, write_instance_json,
};
use petalfall_instance::{
    BrokenReason, BrokenVersion, DiscoveredVersion, IncompleteKind, IncompleteMarker,
    IncompleteOutcome, InstallRecordStore, InstallSpec, MarkerError, MarkerStore, NameViolation,
    PETALFALL_META_DIR, VersionProbe, check_instance_name, discover_versions, list_version_dirs,
    probe_version,
};
use petalfall_version::{LoaderInfo, LoaderKind, VersionJson, identify_mc_version, resolve};

use crate::InstanceError;
use crate::data_format::{GameDirCheck, GuardMode};
use crate::error::{CoreError, Result};
use crate::event::{CoreEvent, EventSink, InstallPhase, emit};
use crate::facade::{Petalfall, make_context};
use crate::install::LoaderChoice;
use crate::instances::{LEGACY_JAVA_MAJOR, broken_reason_text, inheritance_root};
use crate::launch::FailureCode;
use crate::template::TemplateOutcome;

/// 安装过程中原版 JSON 的暂存位置（实例元数据目录里）。合并后的 JSON 写好即删除。
const STAGED_VANILLA_JSON: &str = "vanilla.json";

/// 新实例要装的东西。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceRequest {
    /// 原版版本号（版本清单里的 id）。
    pub mc_version: String,
    pub loader: Option<LoaderChoice>,
    /// 加载器版本；Fabric / Quilt 缺省取推荐版，Forge / NeoForge 必须给出。
    pub loader_version: Option<String>,
    /// 装好后把默认设置补进实例。
    pub use_template: bool,
}

impl InstanceRequest {
    /// 写进未完成标记的形式。
    pub fn spec(&self) -> InstallSpec {
        InstallSpec {
            mc_version: self.mc_version.clone(),
            loader: self.loader.map(|choice| choice.key().to_owned()),
            loader_version: self.loader_version.clone(),
            use_template: self.use_template,
        }
    }

    /// 从未完成标记里的安装内容还原；标记里的加载器键不认识时报不可补全。
    pub fn from_spec(id: &str, spec: &InstallSpec) -> Result<Self> {
        Self::recognize(spec, SpecSource::Marker).map_err(|reason| {
            InstanceError::NotRepairable {
                id: id.to_owned(),
                reason,
            }
            .into()
        })
    }

    /// 同 [`from_spec`](Self::from_spec)，加载器键不认识时只交回原因：补全定方案时要把它连同种类一起交给实例列表。
    /// `source` 是 `spec` 读自哪份文件，原因里照实点出它。
    fn recognize(spec: &InstallSpec, source: SpecSource) -> std::result::Result<Self, String> {
        let loader = match spec.loader.as_deref() {
            None => None,
            Some(key) => Some(
                LoaderChoice::from_key(key)
                    .ok_or_else(|| format!("{}里记的加载器 {key} 不认识", source.file_name()))?,
            ),
        };
        Ok(Self {
            mc_version: spec.mc_version.clone(),
            loader,
            loader_version: spec.loader_version.clone(),
            use_template: spec.use_template,
        })
    }

    /// 不碰网络就能判断的问题：Forge / NeoForge 没有「推荐版」可查，版本号必须给出；版本号来自远端版本
    /// 列表，会原样拼进安装器地址、缓存文件名与版本 id，必须是单个安全的路径段。
    pub fn validate(&self) -> Result<()> {
        if let Some(choice @ (LoaderChoice::Forge | LoaderChoice::NeoForge)) = self.loader
            && self.loader_version.is_none()
        {
            return Err(petalfall_install::Error::LoaderVersionNotFound {
                loader: choice.display_name(),
                game_version: self.mc_version.clone(),
            }
            .into());
        }
        if let (Some(choice), Some(version)) = (self.loader, &self.loader_version) {
            petalfall_version::check_path_segment(version).map_err(|problem| {
                petalfall_install::Error::UnsafePath {
                    entry: format!("{} 版本号", choice.display_name()),
                    path: version.clone(),
                    problem,
                }
            })?;
        }
        Ok(())
    }
}

/// 记着当初安装内容的那份文件。两份都在实例的元数据目录里，加载器认不出来时原因里点出是哪一份，玩家才找得到。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpecSource {
    /// 未完成标记：安装没有走完时留着。
    Marker,
    /// 安装记录：实例装好时留下。
    Record,
}

impl SpecSource {
    fn file_name(self) -> &'static str {
        match self {
            SpecSource::Marker => "未完成标记",
            SpecSource::Record => "安装记录",
        }
    }
}

/// 实例里装上的加载器。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledLoader {
    pub choice: LoaderChoice,
    pub version: String,
}

/// 一次实例安装的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceInstallOutcome {
    pub id: String,
    pub mc_version: String,
    pub loader: Option<InstalledLoader>,
    /// 确保就绪的库文件数。
    pub libraries: usize,
    /// 确保就绪的资源对象数。
    pub assets: usize,
    /// natives 文件数。
    pub natives: u32,
    /// 套默认设置的结果；没请求套用时为 `None`。
    pub template: Option<TemplateOutcome>,
}

/// 补全的方案。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepairPlan {
    /// 重装：当初的安装没有完成时照记下的内容，版本 JSON 丢失或写坏时照安装记录，重新安装加载器时照版本
    /// JSON 读出的内容。
    Reinstall(InstanceRequest),
    /// 按现有版本 JSON 补齐文件。
    Complete,
}

/// 一次补全的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairOutcome {
    /// 是否走了重装。
    pub reinstalled: bool,
    pub libraries: usize,
    pub assets: usize,
    pub natives: u32,
}

/// 长任务的结局，写进未完成标记。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkerSettlement {
    /// 成功：删掉标记。
    Succeeded,
    /// 失败：记下错误码与原文。
    Failed(MarkerError),
    /// 取消。
    Cancelled,
}

impl Petalfall {
    /// 校验名称，原子地建出实例目录并写下未完成标记，返回写下的标记（结局时原样交回
    /// [`Petalfall::settle_instance_marker`]）。目录已经存在（含并发新建的同名实例）报重名。
    ///
    /// 建任何东西之前先以「使用」方式核对当前游戏目录的数据格式记号：目录被更新的落樱用过、记号读不懂时报错，
    /// 什么都不建。
    pub async fn create_instance_dir(
        &self,
        name: &str,
        request: &InstanceRequest,
    ) -> Result<IncompleteMarker> {
        request.validate()?;
        let checked = self.guard_game_dir(GuardMode::Use).await?;
        self.check_new_instance_name(name).await?;
        let dir = self.instance_dir(name)?;
        let versions = self.layout().versions_dir();
        tokio::fs::create_dir_all(&versions)
            .await
            .map_err(|source| petalfall_base::Error::Io {
                path: versions,
                source,
            })?;
        // 游戏目录是上一步才建出来的：开头核对时它还不在，记号没处写，现在补上。放在建实例目录之前，记号写不进去
        // 时还没有任何实例数据落下。
        if checked == GameDirCheck::Absent {
            self.guard_game_dir(GuardMode::Use).await?;
        }
        match tokio::fs::create_dir(&dir).await {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(InstanceError::InvalidName {
                    name: name.to_owned(),
                    violation: NameViolation::Duplicate,
                }
                .into());
            }
            Err(source) => return Err(petalfall_base::Error::Io { path: dir, source }.into()),
        }
        // 名字下还留着的记录属于在启动器之外被删掉的同名旧实例，不清掉的话新实例一建出来就带着它的时长。
        // 放在目录建成之后：撞名时那份记录是现有实例的。清不掉不拦新建，改动已记下待补，在这个实例的
        // 第一局记下之前一定先补上。
        if let Err(err) = self.forget_play_records(name).await {
            tracing::warn!(name, error = %err, "清掉同名旧实例的游玩记录失败，已记下待补");
        }
        let marker = IncompleteMarker::pending(
            IncompleteKind::Install {
                spec: request.spec(),
            },
            now_millis(),
        );
        MarkerStore::for_version_dir(&dir).save(&marker).await?;
        tracing::info!(name, mc = %request.mc_version, loader = ?request.loader, "新建实例：目录已建好");
        Ok(marker)
    }

    /// 在已建好的实例目录 `versions/<id>/` 里安装 `request` 描述的版本。
    ///
    /// 动手之前先以「使用」方式核对当前游戏目录的数据格式记号：目录被更新的落樱用过、记号读不懂时报错，不下载、
    /// 不写任何文件。
    pub async fn install_instance(
        &self,
        id: &str,
        request: &InstanceRequest,
        events: Option<&EventSink>,
    ) -> Result<InstanceInstallOutcome> {
        request.validate()?;
        // 不看是不是「目录不在」：实例目录在，游戏目录就在；不在时下一步就报实例不存在。新建出来的游戏目录由
        // create_instance_dir 补记号。
        self.guard_game_dir(GuardMode::Use).await?;
        let dir = self.existing_instance_dir(id).await?;
        let announcer = PhaseAnnouncer::new(events, install_plan(request));
        let report_phase = |phase| announcer.enter(phase);
        announcer.enter(InstallPhase::Metadata);
        self.prepare_sources().await?;
        let layout = self.layout();
        let pool = self.download_pool_reporting(events);
        let policy = self.retry_policy();
        let http = self.http();
        let cx = make_context(
            &http,
            &pool,
            &layout,
            self.runtime(),
            &policy,
            self.cancel_token(),
        )
        .with_phases(Some(&report_phase));
        let mc = request.mc_version.as_str();

        emit(events, CoreEvent::stage(format!("查找原版 {mc}")));
        let vanilla_installer = VanillaInstaller::new(cx)
            .with_manifest_url(self.manifest_url())
            .with_sources(self.source_plan(self.config().version_list_source));
        let entry = vanilla_installer.find_entry(mc).await?;
        let staged = dir.join(PETALFALL_META_DIR).join(STAGED_VANILLA_JSON);
        let vanilla = vanilla_installer
            .download_version_json(&entry, &staged)
            .await?;

        let (merged, loader, placed) = match request.loader {
            None => (merge_for_instance(id, &vanilla, None)?, None, None),
            Some(choice @ (LoaderChoice::Fabric | LoaderChoice::Quilt)) => {
                let (flavor, base) = match choice {
                    LoaderChoice::Fabric => (LoaderFlavor::Fabric, self.fabric_base()),
                    _ => (LoaderFlavor::Quilt, self.quilt_base()),
                };
                announcer.enter(InstallPhase::LoaderProfile);
                emit(
                    events,
                    CoreEvent::stage(format!("获取 {} 加载器信息", choice.display_name())),
                );
                let (_, profile, version) = LoaderInstaller::with_flavor(cx, flavor)
                    .with_base_url(base)
                    .with_sources(self.source_plan(self.config().version_list_source))
                    .fetch_profile(mc, request.loader_version.as_deref())
                    .await?;
                (
                    merge_for_instance(id, &vanilla, Some(&profile))?,
                    Some(InstalledLoader { choice, version }),
                    None,
                )
            }
            Some(choice) => {
                let version = request
                    .loader_version
                    .clone()
                    .expect("validate 已保证 Forge / NeoForge 带版本号");
                // 处理器要读实例目录里的客户端 jar 与原版库，先把原版补齐。
                emit(events, CoreEvent::stage(format!("补全原版 {mc} 的文件")));
                let base = merge_for_instance(id, &vanilla, None)?;
                ensure_complete(cx, &base, id).await?;
                let major = vanilla
                    .java_version
                    .as_ref()
                    .map_or(LEGACY_JAVA_MAJOR, |java| java.major_version);
                announcer.enter(InstallPhase::Java);
                let (found, java) = self.resolve_java(major, events).await?;
                let url = match choice {
                    LoaderChoice::Forge => forge_installer_url(mc, &version),
                    _ => neoforge_installer_url(&version),
                };
                emit(
                    events,
                    CoreEvent::stage(format!(
                        "下载并执行 {} 安装器：{url}",
                        choice.display_name()
                    )),
                );
                // 有的老安装器处理器要从客户端 jar 旁边的版本 JSON 里读原版的库清单，实例自己的那份要到装完才合并
                // 落盘，在的话也给不了处理器：先换成原版的。守卫活到下面写好合并结果才放下，中途失败、任务被丢弃
                // 或 panic 都会还原。进程被硬杀时留下原版那份，未完成标记里记着这次要装的内容（新建由
                // create_instance_dir、重装由 begin_repair / begin_loader_reinstall 在这之前写下），补全照它重装。
                let placed = place_json_for_processors(&layout, &base)?;
                // 处理器不看下载池的令牌，取消看的是上下文上挂的那个（make_context 已挂上）：
                // 取消时正在跑的处理器要被结束，不能等宽限期满被丢下。
                let java_major = found.first().map(|picked| picked.version.major);
                let (embedded, summary) = self
                    .forge_installer(cx, major, java, java_major, events)
                    .install_for_instance(&url, id)
                    .await?;
                emit(
                    events,
                    CoreEvent::stage(format!(
                        "{} 安装器执行完成：库 {} / 处理器 {}",
                        choice.display_name(),
                        summary.libraries,
                        summary.processors
                    )),
                );
                (
                    merge_for_instance(id, &vanilla, Some(&embedded))?,
                    Some(InstalledLoader { choice, version }),
                    Some(placed),
                )
            }
        };

        write_instance_json(&layout, &merged).await?;
        if let Some(placed) = placed {
            // 合并结果已经盖掉了给处理器换上的那份。
            placed.keep();
        }
        // 记实际装上的版本：Fabric / Quilt 没指定版本时装的是当时的推荐版，日后修复时推荐版可能已经变了。
        // 默认设置不记：重装时实例里已经有玩家自己的设置。
        InstallRecordStore::for_version_dir(&dir)
            .save(&InstallSpec {
                mc_version: request.mc_version.clone(),
                loader: request.loader.map(|choice| choice.key().to_owned()),
                loader_version: loader.as_ref().map(|l| l.version.clone()),
                use_template: false,
            })
            .await?;
        emit(events, CoreEvent::stage("补全库、资源与 natives"));
        // Forge / NeoForge 的原版文件与加载器的库在这之前都已下好，这一遍照合并后的版本 JSON 再核一遍：整遍合成
        // 「校验」一步报，库、资源与 natives 这几步前面已经报过，不再报。
        let report = if matches!(
            request.loader,
            Some(LoaderChoice::Forge | LoaderChoice::NeoForge)
        ) {
            announcer.enter(InstallPhase::Verify);
            ensure_complete(cx.with_phases(None), &merged, id).await?
        } else {
            ensure_complete(cx, &merged, id).await?
        };
        tokio::fs::remove_file(&staged)
            .await
            .map_err(|source| petalfall_base::Error::Io {
                path: staged.clone(),
                source,
            })?;
        // 放在版本 JSON 写好之后：要不要套看实例隔离不隔离，而隔离取决于版本类型（正式版还是快照），
        // 建目录那一刻还读不到。仍然在第一次启动之前，游戏还没来得及写出自己的默认设置。套失败让安装
        // 失败：重试经补全重装时会再套一次，标记里记着这次请求了套用。
        let template = if request.use_template {
            announcer.enter(InstallPhase::Template);
            let outcome = self.apply_template(id).await?;
            emit(events, CoreEvent::stage(template_stage(&outcome)));
            Some(outcome)
        } else {
            None
        };
        emit(
            events,
            CoreEvent::stage(format!(
                "实例 {id} 安装完成：库 {} / 资源 {} / natives {}",
                report.libraries, report.assets, report.natives
            )),
        );
        Ok(InstanceInstallOutcome {
            id: id.to_owned(),
            mc_version: request.mc_version.clone(),
            loader,
            libraries: report.libraries,
            assets: report.assets,
            natives: report.natives,
            template,
        })
    }

    /// 丢掉一个新建到一半的实例目录（新建被取消时）。目录已经不在不算错。
    ///
    /// 动手之前先以「使用」方式核对当前游戏目录的数据格式记号：目录被更新的落樱用过、记号读不懂时报错，什么都不删。
    pub async fn abandon_instance(&self, id: &str) -> Result<()> {
        self.guard_game_dir(GuardMode::Use).await?;
        let dir = self.instance_dir(id)?;
        match tokio::fs::remove_dir_all(&dir).await {
            Ok(()) => {
                tracing::info!(id, "新建被取消，实例目录已删除");
                Ok(())
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(petalfall_base::Error::Io { path: dir, source }.into()),
        }
    }

    /// 把长任务的结局写进未完成标记。`marker` 是开始时写下的那份。
    ///
    /// 动手之前先以「使用」方式核对当前游戏目录的数据格式记号：目录被更新的落樱用过、记号读不懂时报错，标记不动。
    pub async fn settle_instance_marker(
        &self,
        id: &str,
        marker: IncompleteMarker,
        settlement: MarkerSettlement,
    ) -> Result<()> {
        self.guard_game_dir(GuardMode::Use).await?;
        let store = MarkerStore::for_version_dir(&self.instance_dir(id)?);
        let settled = match settlement {
            MarkerSettlement::Succeeded => return Ok(store.clear().await?),
            MarkerSettlement::Failed(error) => IncompleteMarker {
                outcome: IncompleteOutcome::Failed,
                error: Some(error),
                ..marker
            },
            MarkerSettlement::Cancelled => IncompleteMarker {
                outcome: IncompleteOutcome::Cancelled,
                error: None,
                ..marker
            },
        };
        store.save(&settled).await?;
        Ok(())
    }

    /// 定补全方案并写下新的未完成标记，返回方案与标记。
    ///
    /// 标记文件读不出来（写坏了、这一版不认识的格式、读盘出错）时不照它定方案：版本 JSON 缺失或损坏时照安装记录重装，
    /// 连记录也没有才报不可补全；版本 JSON 可用就照它补全，只有版本 JSON 里一个加载器都探不出、安装记录里却记着加载器
    /// 时照记录重装：重装 Forge、NeoForge 途中被打断时盘上是只合并了原版的那份，照它补全会把实例补成原版；版本 JSON
    /// 里探得出加载器的不是那种现场，照旧按它补全。
    /// 版本 JSON 读不出来（被占用、没有权限、那个位置是个目录）时，除了整合包没导入完的照旧报重新导入，不论标记与
    /// 安装记录都报不可补全，标记不动。
    ///
    /// 动手之前先以「使用」方式核对当前游戏目录的数据格式记号：目录被更新的落樱用过、记号读不懂时报错，标记不动。
    /// 任务中心重试补全不经列实例，不能指望调用方已经核对过。
    pub async fn begin_repair(&self, id: &str) -> Result<(RepairPlan, IncompleteMarker)> {
        self.guard_game_dir(GuardMode::Use).await?;
        self.plan_repair(id, false).await
    }

    /// 定「重新安装加载器」的方案并写下未完成标记：游戏版本、加载器与它的版本号照现有版本 JSON 原样装回，
    /// 不请求套默认设置，实例里的 Mod、配置与存档都不碰。标记记成安装：中途失败或取消后，普通补全照着这份
    /// 内容接着重装。
    ///
    /// 标记里记着安装内容的照它重装，整合包没导入完的报不可补全，版本 JSON 丢失或写坏时照安装记录重装，都与
    /// [`Petalfall::begin_repair`] 一样；定不出能原样装回的内容时报不可补全，标记不动。核对数据格式记号也同它。
    pub async fn begin_loader_reinstall(&self, id: &str) -> Result<(RepairPlan, IncompleteMarker)> {
        self.guard_game_dir(GuardMode::Use).await?;
        self.plan_repair(id, true).await
    }

    async fn plan_repair(
        &self,
        id: &str,
        reinstall_loader: bool,
    ) -> Result<(RepairPlan, IncompleteMarker)> {
        let dir = self.existing_instance_dir(id).await?;
        let store = MarkerStore::for_version_dir(&dir);
        let loaded = store.load().await;
        let previous = match &loaded {
            Ok(Some(marker)) => PreviousMarker::Found(marker),
            Ok(None) => PreviousMarker::Absent,
            Err(err) => {
                tracing::warn!(
                    id,
                    error = %err,
                    "未完成标记读不出来，按版本 JSON 与安装记录定方案，定下后覆盖它"
                );
                PreviousMarker::Unreadable
            }
        };
        let plan = decide_repair(self.game_dir(), id, previous, reinstall_loader).await?;
        let kind = match &plan {
            RepairPlan::Reinstall(request) => IncompleteKind::Install {
                spec: request.spec(),
            },
            RepairPlan::Complete => IncompleteKind::Repair,
        };
        let marker = IncompleteMarker::pending(kind, now_millis());
        store.save(&marker).await?;
        tracing::info!(id, plan = ?plan, "开始补全实例");
        Ok((plan, marker))
    }

    /// 按方案补全实例。
    ///
    /// 动手之前先以「使用」方式核对当前游戏目录的数据格式记号：目录被更新的落樱用过、记号读不懂时报错，不下载、
    /// 不写任何文件。重装方案经 [`Petalfall::install_instance`]，那里照样再核对一次。
    pub async fn repair_instance(
        &self,
        id: &str,
        plan: &RepairPlan,
        events: Option<&EventSink>,
    ) -> Result<RepairOutcome> {
        self.guard_game_dir(GuardMode::Use).await?;
        match plan {
            RepairPlan::Reinstall(request) => {
                let outcome = self.install_instance(id, request, events).await?;
                Ok(RepairOutcome {
                    reinstalled: true,
                    libraries: outcome.libraries,
                    assets: outcome.assets,
                    natives: outcome.natives,
                })
            }
            RepairPlan::Complete => {
                let scan = discover_versions(self.game_dir()).await?;
                let target = scan
                    .versions
                    .iter()
                    .find(|v| v.id == id)
                    .ok_or_else(|| CoreError::VersionNotInstalled { id: id.to_owned() })?;
                let provider: HashMap<String, VersionJson> = scan
                    .versions
                    .iter()
                    .map(|v| (v.id.clone(), v.json.clone()))
                    .collect();
                let merged = resolve(&target.json, &provider)?;
                // 传统布局的加载器版本，客户端 jar 与 natives 在继承链根（原版）的目录里。
                let root = inheritance_root(&scan, id);
                let layout = self.layout();
                // 安装器在本机生成的文件补全下不回来：缺了就照「加载器文件缺失」停下，不假装补全成功。命令层
                // 把这个失败码记进未完成标记，下一次补全就改走重新安装加载器（见 decide_repair）。排在测速之前：
                // 核对只看本地文件，注定失败的补全不该先发一轮测速探针、网络不佳时还要等它超时。
                petalfall_install::complete::reject_missing_processor_outputs(&merged, &layout)
                    .await?;
                self.prepare_sources().await?;
                let pool = self.download_pool_reporting(events);
                let policy = self.retry_policy();
                let http = self.http();
                let announcer = PhaseAnnouncer::new(
                    events,
                    vec![
                        InstallPhase::Libraries,
                        InstallPhase::Assets,
                        InstallPhase::Natives,
                    ],
                );
                let report_phase = |phase| announcer.enter(phase);
                let cx = make_context(
                    &http,
                    &pool,
                    &layout,
                    self.runtime(),
                    &policy,
                    self.cancel_token(),
                )
                .with_phases(Some(&report_phase));
                emit(events, CoreEvent::stage(format!("补全实例 {id} 的文件")));
                let report = ensure_complete(cx, &merged, &root).await?;
                emit(
                    events,
                    CoreEvent::stage(format!(
                        "实例 {id} 补全完成：库 {} / 资源 {} / natives {}",
                        report.libraries, report.assets, report.natives
                    )),
                );
                Ok(RepairOutcome {
                    reinstalled: false,
                    libraries: report.libraries,
                    assets: report.assets,
                    natives: report.natives,
                })
            }
        }
    }
}

/// 补全修不好的判定：实例列表提前给出，对它发起补全时报的就是其中的原因。种类与原因由同一段判断一起定下，界面按
/// 种类分支，不去解析原因的文字。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unrepairable {
    pub kind: UnrepairableKind,
    /// 与补全报的 [`InstanceError::NotRepairable`] 里的 `reason` 逐字相同。
    pub reason: String,
}

/// 补全为什么修不好。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnrepairableKind {
    /// 版本 JSON 读盘出错（被占用、没有权限、那个位置是个目录）：文件可能完好，放开占用或修好权限后重新扫描就读得
    /// 出来。不带未完成标记的到这里就恢复了；带着 install 或 repair 标记的扫完还得再修复一次，扫描只重读版本 JSON，
    /// 不动标记。
    Unreadable,
    /// 版本 JSON 缺失或损坏，又没有安装记录可照着重装（记录里的加载器认不出来也算：照它装不回去）。
    NoRecord {
        /// 原因里点出的改名目标：实例目录里恰有一份名字对不上的版本 JSON、实例又改得成那个名字时才有，把实例改成
        /// 它就能认出那份文件。与原因里的提示出自同一处，两者同有同无。
        rename_to: Option<String>,
    },
    /// 整合包没有导入完：缺的 Mod 与配置补不回来，只能删掉重新导入。
    Modpack,
}

impl Unrepairable {
    fn into_error(self, id: &str) -> CoreError {
        InstanceError::NotRepairable {
            id: id.to_owned(),
            reason: self.reason,
        }
        .into()
    }
}

/// 照未完成标记、版本 JSON 与安装记录定补全方案，不写盘。版本 JSON 用不了时的那一段（[`broken_verdict`]）与实例
/// 列表共用：列表上说的修不好的原因就是真去补全时报的那句，两边不各写一套判断，不会一边说修得好、一边又拒绝。
///
/// `previous` 是读未完成标记的结果。读不出来的标记给不出结论：版本 JSON 用不了时与没有标记一样看安装记录；版本 JSON
/// 读得出、里面探不出加载器、本来要照它补全时还要对一眼安装记录（见 [`reinstall_over_unreadable_marker`]），没有
/// 标记时不看。标记是 install 时也要先探一次版本 JSON：读不出来的话，照标记重装最后也要把版本 JSON 写回那个位置（见
/// [`unreadable_verdict`]）。
pub(crate) async fn decide_repair(
    game_dir: &Path,
    id: &str,
    previous: PreviousMarker<'_>,
    reinstall_loader: bool,
) -> Result<RepairPlan> {
    let marker = previous.marker();
    // 上次补全已经查实加载器自带的库只能重装补回：再照版本 JSON 补一遍只会原样失败，实例库、启动胶囊
    // 与任务重试的入口都带不上「重装」这个意思，所以由看标记定方案的这一步替它们换掉。装不回的照旧补全：
    // 玩家可能已经把文件放回去了，这是清掉标记的唯一办法；没放回去时照样报缺失。
    let stuck = marker.is_some_and(lost_loader_library);
    let version = match probe_version(game_dir, id).await {
        Ok(VersionProbe::Found(version)) => version,
        Ok(VersionProbe::Broken(broken)) => {
            return broken_verdict(game_dir, id, marker, &broken)
                .await
                .map(RepairPlan::Reinstall)
                .map_err(|refusal| refusal.into_error(id));
        }
        // 全量扫描把同样的读盘故障记成 `BrokenReason::Unreadable`，原因同样取 `error_with_causes`（见
        // `discover_versions`）：照这个口径拼原因，列表上说的与这里报的才是同一句。
        Err(err) => {
            let detail = error_with_causes(&err);
            tracing::warn!(id, error = %detail, "版本 JSON 读不出来");
            return Err(unreadable_verdict(marker, detail).into_error(id));
        }
    };
    if let Some(verdict) = marker_verdict(marker) {
        return verdict
            .map(RepairPlan::Reinstall)
            .map_err(|refusal| refusal.into_error(id));
    }
    if reinstall_loader || (stuck && reinstallable_loader(&version.loaders).is_ok()) {
        Ok(RepairPlan::Reinstall(
            loader_reinstall_request(game_dir, &version).await?,
        ))
    } else if matches!(previous, PreviousMarker::Unreadable)
        && let Some(request) = reinstall_over_unreadable_marker(id, &version).await
    {
        Ok(RepairPlan::Reinstall(request))
    } else {
        Ok(RepairPlan::Complete)
    }
}

/// 补全定方案时读未完成标记的结果。
#[derive(Debug, Clone, Copy)]
pub(crate) enum PreviousMarker<'a> {
    Found(&'a IncompleteMarker),
    /// 没有标记文件。
    Absent,
    /// 有标记文件却读不出来：写坏了、是这一版不认识的格式，或读盘出错。
    Unreadable,
}

impl<'a> PreviousMarker<'a> {
    /// 读到的标记；读不出来的与没有一样为 `None`。
    fn marker(self) -> Option<&'a IncompleteMarker> {
        match self {
            PreviousMarker::Found(marker) => Some(marker),
            PreviousMarker::Absent | PreviousMarker::Unreadable => None,
        }
    }
}

/// 未完成标记读不出来、版本 JSON 读得出时照安装记录重装的内容：版本 JSON 里一个加载器都探不出、记录里却记着加载器时
/// 才有，其余为 `None`，照旧按版本 JSON 补全。
///
/// 重装 Forge、NeoForge 途中进程被结束时，盘上是只合并了原版的那份版本 JSON 加 install 标记（处理器要读原版那份，
/// 见 `place_json_for_processors`）；标记又读不出来的话，照版本 JSON 补全会把加载器实例静默补成原版，这时说得出
/// 当初装的是什么的只剩安装记录。只认这种现场的样子：原版那份里什么加载器都探不出。探出了任何一种（记录是 Forge
/// 而版本 JSON 是 Fabric，或是只装着 OptiFine、LiteLoader 这类装不回的外部安装器产物）都不是被打断留下的，多半是
/// 玩家自己换的文件，照记录重装会把它盖掉，所以不比种类，探出了就按版本 JSON 补全。读不出的记录当作没有；记录里的
/// 加载器认不出来时照它装不回去，同样按版本 JSON 补全，不因此拒掉一个补得了的实例。
async fn reinstall_over_unreadable_marker(
    id: &str,
    version: &DiscoveredVersion,
) -> Option<InstanceRequest> {
    if !version.loaders.is_empty() {
        return None;
    }
    let spec = match InstallRecordStore::for_version_dir(&version.dir)
        .load()
        .await
    {
        Ok(spec) => spec?,
        Err(err) => {
            tracing::warn!(id, error = %err, "安装记录损坏，当作没有记录");
            return None;
        }
    };
    let request = match recorded_request(&spec, SpecSource::Record) {
        Ok(request) => request,
        Err(refusal) => {
            tracing::warn!(id, reason = %refusal.reason, "照安装记录装不回去，按版本 JSON 补全");
            return None;
        }
    };
    request.loader.is_some().then_some(request)
}

/// 未完成标记自己就定得下的结论：照安装标记里记下的内容重装，或整合包没导入完修不好。标记是 repair 或没有标记时
/// 为 `None`，要看版本 JSON 与安装记录。
fn marker_verdict(
    previous: Option<&IncompleteMarker>,
) -> Option<std::result::Result<InstanceRequest, Unrepairable>> {
    match previous.map(|marker| &marker.kind) {
        Some(IncompleteKind::Install { spec }) => Some(recorded_request(spec, SpecSource::Marker)),
        // 导入写版本 JSON 在前、下载 Mod 与解出覆盖文件在后，中断时版本 JSON 往往已经可用；
        // 照它补全只会补齐库文件，再把标记清掉，缺的 Mod 与配置却补不回来，实例就被当成了完好的。
        Some(IncompleteKind::Modpack { .. }) => Some(Err(modpack_refusal())),
        Some(IncompleteKind::Repair) | None => None,
    }
}

/// 版本 JSON 用不了（`broken`）时补全的结论：照未完成标记或安装记录重装的内容，或修不好的判定。实例列表与
/// [`decide_repair`] 都经这里。
///
/// 不读版本 JSON；会读的只有安装记录（读不出来的不读），没有记录、缺的又是版本 JSON 时再看一眼实例目录顶层，在那里
/// 找到一份名字对不上的版本 JSON 时再列一次 `versions/` 查重。这几样读不出都不报错：读不出的记录当作没有，改名
/// 提示说不准就不提，所以列表拿到的一定是一个结论。
async fn broken_verdict(
    game_dir: &Path,
    id: &str,
    previous: Option<&IncompleteMarker>,
    broken: &BrokenVersion,
) -> std::result::Result<InstanceRequest, Unrepairable> {
    if let BrokenReason::Unreadable(detail) = &broken.reason {
        return Err(unreadable_verdict(previous, detail.clone()));
    }
    if let Some(verdict) = marker_verdict(previous) {
        return verdict;
    }
    let record = match InstallRecordStore::for_version_dir(&broken.dir)
        .load()
        .await
    {
        Ok(record) => record,
        Err(err) => {
            tracing::warn!(id, error = %err, "安装记录损坏，当作没有记录");
            None
        }
    };
    match record {
        Some(spec) => recorded_request(&spec, SpecSource::Record),
        None => Err(no_record_refusal(game_dir, id, broken).await),
    }
}

/// 照未完成标记或安装记录（`source`）里记下的内容重装。加载器认不出来时照它装不回去，与没有记录一样修不好。
fn recorded_request(
    spec: &InstallSpec,
    source: SpecSource,
) -> std::result::Result<InstanceRequest, Unrepairable> {
    InstanceRequest::recognize(spec, source).map_err(|reason| Unrepairable {
        kind: UnrepairableKind::NoRecord { rename_to: None },
        reason,
    })
}

fn modpack_refusal() -> Unrepairable {
    Unrepairable {
        kind: UnrepairableKind::Modpack,
        reason: "整合包没有导入完，缺的 Mod 与配置补不回来，删掉这个实例后重新导入".to_owned(),
    }
}

/// 版本 JSON 读不出来（被占用、没有权限、那个位置是个目录）时补全的拒绝。不看安装记录：照记录重装最后也要把版本
/// JSON 写回这个位置，读不出的位置多半也写不进去，白下一遍文件。
///
/// 原因只由 `detail`（读盘错误连同系统给的原因）拼出，列表与补全都经这里。两边各读一次盘，系统给的原因在两次读取
/// 之间可能不同（这一刻被占用、下一刻没有权限），那时两句只差在冒号后面的那段，照实给出，不为凑成一句丢掉它。
fn unreadable_refusal(detail: String) -> Unrepairable {
    Unrepairable {
        kind: UnrepairableKind::Unreadable,
        reason: broken_reason_text(&BrokenReason::Unreadable(detail)),
    }
}

/// 版本 JSON 读不出来时修不好的判定。整合包没导入完的照旧报重新导入：那件事与版本 JSON 读不读得出无关，放开占用
/// 以后照样修不好。其余不论未完成标记是不是 install、有没有安装记录都报读不出来（见 [`unreadable_refusal`]）：照
/// 标记重装同样要下完整套文件，最后在写回版本 JSON 时失败。
fn unreadable_verdict(previous: Option<&IncompleteMarker>, detail: String) -> Unrepairable {
    match previous.map(|marker| &marker.kind) {
        Some(IncompleteKind::Modpack { .. }) => modpack_refusal(),
        Some(IncompleteKind::Install { .. } | IncompleteKind::Repair) | None => {
            unreadable_refusal(detail)
        }
    }
}

/// 补全开局读继承链时实例自己的版本 JSON 读盘出错（见 [`Petalfall::repair_chain`]）时的拒绝，与列表上给的同一句。
/// `dir` 是实例目录，`detail` 是这一次读盘的错误连同系统给的原因；未完成标记照定补全怎么做时的口径读，读不出的
/// 当作没有。
pub(crate) async fn unreadable_repair_refusal(dir: &Path, id: &str, detail: String) -> CoreError {
    let previous = match MarkerStore::for_version_dir(dir).load().await {
        Ok(previous) => previous,
        Err(err) => {
            tracing::warn!(id, error = %err, "未完成标记损坏，当作没有标记");
            None
        }
    };
    unreadable_verdict(previous.as_ref(), detail).into_error(id)
}

/// 实例列表上一个版本 JSON 用不了的实例补全修不好的判定，原因与对它发起补全时报的逐字相同；修得好时为 `None`。
/// 版本 JSON 读不出来时例外的只有系统给的那段原因：列表用的是扫描那一次读到的，补全时再读一次可能不同（见
/// [`unreadable_refusal`]）。
///
/// `previous` 与 `broken` 是列表已经读到的未完成标记与探测结果，不再读一遍；还会读的见 [`broken_verdict`]。
pub(crate) async fn unrepairable(
    game_dir: &Path,
    id: &str,
    previous: Option<&IncompleteMarker>,
    broken: &BrokenVersion,
) -> Option<Unrepairable> {
    broken_verdict(game_dir, id, previous, broken).await.err()
}

/// 版本 JSON 用不了、也没有安装记录时补全的拒绝。缺的是版本 JSON 时顺带看一眼实例目录：别人打包时把实例目录
/// 改了名，里面的版本 JSON 还叫原来的名字，只说「缺少版本 JSON」的话，玩家想不到文件其实就在目录里。实例改不成那个
/// 名字时不提（见 [`renamable_to`]）。提了的话，改名目标连同原因一起交回。
async fn no_record_refusal(game_dir: &Path, id: &str, broken: &BrokenVersion) -> Unrepairable {
    let reason = format!(
        "{}，也没有记下当初装的是什么",
        broken_reason_text(&broken.reason)
    );
    let unhinted = || Unrepairable {
        kind: UnrepairableKind::NoRecord { rename_to: None },
        reason: reason.clone(),
    };
    if broken.reason != BrokenReason::MissingJson {
        return unhinted();
    }
    // 只是附带的提示：目录没读全就说不准是不是恰好一份、改不改得成，宁可不提，修不好的原因照样报。
    let name = match misnamed_version_json(&broken.dir).await {
        Ok(Some(name)) => name,
        Ok(None) => return unhinted(),
        Err(err) => {
            tracing::warn!(id, dir = %broken.dir.display(), error = %err, "查找名字对不上的版本 JSON 失败");
            return unhinted();
        }
    };
    // 查重的名单与改名时的同一份（`Petalfall::instance_names`）：`versions/` 下全部目录名，含版本 JSON 用不了的。
    let existing = match list_version_dirs(game_dir).await {
        Ok(names) => names.unwrap_or_default(),
        Err(err) => {
            tracing::warn!(id, error = %error_with_causes(&err), "列不出实例名，不提改名");
            return unhinted();
        }
    };
    match renamable_to(id, name, &existing) {
        Some(name) => Unrepairable {
            reason: format!(
                "{reason}；目录里有一份叫 {name}.json 的版本文件，把实例改名成 {name} 就能认出来"
            ),
            kind: UnrepairableKind::NoRecord {
                rename_to: Some(name),
            },
        },
        None => unhinted(),
    }
}

/// `name` 过得了实例 `id` 改名时的校验才交回它：与 [`Petalfall::rename_instance_files`] 同一个
/// [`check_instance_name`]，命名规则加上与 `existing` 不分大小写重名（自己除外）。过不了的名字提给玩家，照做只会被
/// 改名拒绝。
///
/// 首尾带空白的名字也不交回：界面提交改名前照 JavaScript 的 `trim` 去掉首尾空白，照提示输进去的名字到后端已经是
/// 另一个，永远改不成，这些空白在提示文字里也看不出来。后端的命名规则只拒段尾的半角空格，开头的空白与段尾的全角
/// 空格、不换行空格都放行，所以要另看一遍。`trim` 去掉的比 Unicode 空白多一个 U+FEFF，这里一并算上。
fn renamable_to(id: &str, name: String, existing: &[String]) -> Option<String> {
    let submitted = name.trim_matches(|c: char| c.is_whitespace() || c == '\u{FEFF}');
    (submitted == name
        && check_instance_name(&name, existing.iter().map(String::as_str), Some(id)).is_ok())
    .then_some(name)
}

/// 实例目录顶层名字对不上的那份版本 JSON 的名字（不带 `.json`）：内容能解析成版本 JSON，`id` 又正好是自己的
/// 文件名，实例改成这个名字就能按 `<id>/<id>.json` 认出来。不止一份时说不准该改成哪个，与没有一样给 `None`。
async fn misnamed_version_json(dir: &Path) -> std::io::Result<Option<String>> {
    let mut names = Vec::new();
    let mut entries = tokio::fs::read_dir(dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        if !entry.file_type().await?.is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        let bytes = tokio::fs::read(&path).await?;
        let json = std::str::from_utf8(&bytes)
            .ok()
            .and_then(|text| VersionJson::from_json_str(text).ok());
        if json.is_some_and(|json| json.id == stem) {
            names.push(stem.to_owned());
        }
    }
    Ok(if names.len() == 1 { names.pop() } else { None })
}

/// 从现有版本 JSON 还原「把加载器原样装回」要的内容。在写标记之前调：定不出来的实例不留下一份照猜测写的
/// 安装标记，版本号不安全的也在这里拒掉。
async fn loader_reinstall_request(
    game_dir: &Path,
    version: &DiscoveredVersion,
) -> Result<InstanceRequest> {
    let id = version.id.as_str();
    let refuse = |reason: String| -> CoreError {
        InstanceError::NotRepairable {
            id: id.to_owned(),
            reason,
        }
        .into()
    };
    let (loader, loader_version) = reinstallable_loader(&version.loaders).map_err(refuse)?;
    // 看继承链根而不是 inheritsFrom：加载器版本可能叠在另一个加载器版本上，改过名的原版基底 id 是玩家起的
    // 名字，版本号只在 clientVersion 里。基底已经不在时只剩 inheritsFrom 写的那个 id 可用。
    let scan = discover_versions(game_dir).await?;
    let root = inheritance_root(&scan, id);
    let base = scan
        .versions
        .iter()
        .find(|v| v.id == root)
        .map_or(&version.json, |v| &v.json);
    let mc = identify_mc_version(base);
    let mc_version = mc
        .value
        .filter(|_| mc.reliable)
        .ok_or_else(|| refuse("读不出游戏版本号，无法原样装回".to_owned()))?;
    let request = InstanceRequest {
        mc_version,
        loader: Some(loader),
        loader_version: Some(loader_version),
        use_template: false,
    };
    request.validate()?;
    Ok(request)
}

/// 这份标记记着上次败在加载器自带的库上（码与命令报给前端的启动失败码同一个）。
fn lost_loader_library(marker: &IncompleteMarker) -> bool {
    marker
        .error
        .as_ref()
        .is_some_and(|error| error.code == FailureCode::LoaderLibraryMissing.as_str())
}

/// 能原样装回的加载器与它的版本号；装不回时给出写进不可补全原因的说明。
///
/// OptiFine、LiteLoader 是外部安装器写进版本 JSON 的，这里装不了，叠在别的加载器上时重装也会把它们丢掉。
/// 读不出版本号的同样不装：Fabric / Quilt 缺版本号时安装器会改装推荐版，版本号就变了。
fn reinstallable_loader(
    loaders: &[LoaderInfo],
) -> std::result::Result<(LoaderChoice, String), String> {
    if let Some(external) = loaders
        .iter()
        .find(|l| matches!(l.kind, LoaderKind::OptiFine | LoaderKind::LiteLoader))
    {
        return Err(format!(
            "装着 {}，重新安装只能装回 Fabric、Quilt、Forge 或 NeoForge，它会丢失",
            external.kind.as_str()
        ));
    }
    // NeoForge 排在 Forge 前面，与界面取主加载器同序：1.20.1 的 NeoForge 探测时也会命中 Forge。
    let primary = [
        (LoaderKind::NeoForge, LoaderChoice::NeoForge),
        (LoaderKind::Forge, LoaderChoice::Forge),
        (LoaderKind::Fabric, LoaderChoice::Fabric),
        (LoaderKind::Quilt, LoaderChoice::Quilt),
    ]
    .into_iter()
    .find_map(|(kind, choice)| {
        let found = loaders.iter().find(|l| l.kind == kind)?;
        Some((choice, found.version.clone()))
    });
    match primary {
        None => Err("没有装加载器，不需要重新安装".to_owned()),
        Some((choice, None)) => Err(format!(
            "读不出 {} 的版本号，无法原样装回",
            choice.display_name()
        )),
        Some((choice, Some(version))) => Ok((choice, version)),
    }
}

/// 新建要走的各步。Forge / NeoForge 的处理器要读实例目录里的客户端 jar，先把原版补齐；加载器装好后照合并后的
/// 版本 JSON 再补一遍，这一遍合成「校验」一步报。
fn install_plan(request: &InstanceRequest) -> Vec<InstallPhase> {
    use InstallPhase::*;
    let mut plan = vec![Metadata];
    match request.loader {
        None => plan.extend([Libraries, Assets, Natives]),
        Some(LoaderChoice::Fabric | LoaderChoice::Quilt) => {
            plan.extend([LoaderProfile, Libraries, Assets, Natives]);
        }
        Some(LoaderChoice::Forge | LoaderChoice::NeoForge) => plan.extend([
            Libraries,
            Assets,
            Natives,
            Java,
            LoaderInstaller,
            LoaderLibraries,
            LoaderProcessors,
            Verify,
        ]),
    }
    if request.use_template {
        plan.push(Template);
    }
    plan
}

/// 按开工时排定的步骤表报步骤。报来的步骤不在表里、比上一步靠前或报了两遍，说明表排错了：前端拿到这样的
/// 下标只会算出倒退的进度，直接崩。
///
/// 开工时还排不准的任务（导入整合包读到包之前不知道加载器与有没有远程文件）先按已知情况排一份，读清楚后经
/// [`revise`](Self::revise) 改写还没报到的步骤，再经 [`resume`](Self::resume) 接着报。
///
/// 事件出口自己拿一份，不借用：导出整合包与恢复存档有的步骤是在阻塞线程里进入的（写完最后一个文件转校验、
/// 校验通过转解压），播报器要能放进 `Arc` 带过去。
pub(crate) struct PhaseAnnouncer {
    events: Option<EventSink>,
    plan: Vec<InstallPhase>,
    /// 下一步最小的合法下标。
    next: AtomicUsize,
}

/// 报到一半的步骤表与下一步最小的合法下标：改写步骤表之后交给 [`PhaseAnnouncer::resume`] 接着报。
#[derive(Debug, Clone)]
pub(crate) struct PhaseCursor {
    plan: Vec<InstallPhase>,
    next: usize,
}

impl PhaseAnnouncer {
    pub(crate) fn new(events: Option<&EventSink>, plan: Vec<InstallPhase>) -> Self {
        Self::resume(events, PhaseCursor { plan, next: 0 })
    }

    /// 接着 `cursor` 报：已经报过的步骤不能再报，下标照旧只许变大。
    pub(crate) fn resume(events: Option<&EventSink>, cursor: PhaseCursor) -> Self {
        Self {
            events: events.cloned(),
            plan: cursor.plan,
            next: AtomicUsize::new(cursor.next),
        }
    }

    /// 把还没报到的步骤改写成 `plan`。自己不发事件，下一次 [`enter`](Self::enter) 发出的事件带新表。
    ///
    /// 已经报过的步骤（前端据此算过进度、画过「第几步」）必须原样留在新表的同一位置，否则前端拿到的是一份
    /// 前后对不上的表：新表的前 `next` 项与旧表不同就是契约错误，直接崩。
    pub(crate) fn revise(self, plan: Vec<InstallPhase>) -> PhaseCursor {
        let next = self.next.into_inner();
        assert!(
            plan.get(..next) == self.plan.get(..next),
            "改写步骤表动了已经报过的前 {next} 步：旧表 {:?}，新表 {plan:?}",
            self.plan
        );
        PhaseCursor { plan, next }
    }

    /// 这次的表里有没有 `phase`。可有可无的步骤报不报照这个判断，不再读一遍排表时读过的状态：那份状态在
    /// 排表之后可能变了，再读会报出表外的步骤。
    pub(crate) fn plans(&self, phase: InstallPhase) -> bool {
        self.plan.contains(&phase)
    }

    pub(crate) fn enter(&self, phase: InstallPhase) {
        let index = self
            .plan
            .iter()
            .position(|p| *p == phase)
            .unwrap_or_else(|| panic!("步骤 {phase:?} 不在这次的步骤表 {:?} 里", self.plan));
        let floor = self.next.swap(index + 1, Ordering::Relaxed);
        // 两种违约分开报：崩溃信息里一眼分得清是把刚报过的一步又报了一遍，还是退回了更早的一步。
        assert!(
            index + 1 != floor,
            "步骤 {phase:?} 报了两遍：上一步报的就是它，步骤表 {:?}",
            self.plan
        );
        assert!(
            index >= floor,
            "步骤 {phase:?} 报在了它后面的步骤之后：步骤表 {:?}",
            self.plan
        );
        emit(
            self.events.as_ref(),
            CoreEvent::Phase {
                plan: self.plan.clone(),
                index,
            },
        );
    }
}

/// 套默认设置这一步的进度文案。
pub(crate) fn template_stage(outcome: &TemplateOutcome) -> String {
    match outcome {
        TemplateOutcome::Applied {
            copied,
            kept,
            left_to_mods,
        } => {
            let mut untouched = Vec::new();
            if !kept.is_empty() {
                untouched.push(format!("已有的 {} 保留不动", kept.join("、")));
            }
            if !left_to_mods.is_empty() {
                untouched.push(format!(
                    "{} 留给整合包里模组的默认设置",
                    left_to_mods.join("、")
                ));
            }
            let untouched = untouched.join("；");
            if copied.is_empty() {
                format!("沿用默认设置：{untouched}")
            } else if untouched.is_empty() {
                format!("沿用默认设置：补上 {}", copied.join("、"))
            } else {
                format!("沿用默认设置：补上 {}（{untouched}）", copied.join("、"))
            }
        }
        TemplateOutcome::Skipped(reason) => format!("没有沿用默认设置：{}", reason.describe()),
    }
}

/// 当前 Unix 毫秒。系统时钟早于 1970 年时报错而不是记一个 0：标记的开始时间要拿来展示。
pub(crate) fn now_millis() -> u64 {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("系统时钟早于 1970-01-01");
    u64::try_from(elapsed.as_millis()).expect("毫秒数超出 u64")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instances::tests::{petalfall_at, vanilla_json, write_version};

    fn marker_path(instance_dir: &std::path::Path) -> std::path::PathBuf {
        MarkerStore::for_version_dir(instance_dir)
            .path()
            .to_path_buf()
    }
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn request(mc: &str, loader: Option<LoaderChoice>, version: Option<&str>) -> InstanceRequest {
        InstanceRequest {
            mc_version: mc.to_owned(),
            loader,
            loader_version: version.map(str::to_owned),
            use_template: false,
        }
    }

    /// 清单里一个空壳原版（无下载、无资源）与 Fabric meta。
    async fn mock_meta(server: &MockServer) {
        let base = server.uri();
        Mock::given(method("GET"))
            .and(path("/manifest.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(format!(
                r#"{{"latest":{{"release":"1.21.1","snapshot":"1.21.1"}},
                    "versions":[{{"id":"1.21.1","type":"release","url":"{base}/1.21.1.json",
                    "time":"t","releaseTime":"t"}}]}}"#
            )))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/1.21.1.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(vanilla_json("1.21.1")))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v2/versions/loader/1.21.1/0.16.10/profile/json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(format!(
                r#"{{"id":"fabric-loader-0.16.10-1.21.1","inheritsFrom":"1.21.1","type":"release",
                    "mainClass":"net.fabricmc.loader.impl.launch.knot.KnotClient",
                    "libraries":[{{"name":"net.example:loader:0.16.10","url":"{base}/"}}]}}"#
            )))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/net/example/loader/0.16.10/loader-0.16.10.jar"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"loader".to_vec()))
            .mount(server)
            .await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fabric_instance_is_self_contained_under_its_name() {
        let server = MockServer::start().await;
        mock_meta(&server).await;
        let tmp = tempfile::tempdir().unwrap();
        let petalfall = petalfall_at(tmp.path())
            .with_manifest_url(format!("{}/manifest.json", server.uri()))
            .with_fabric_base(server.uri());
        let req = request("1.21.1", Some(LoaderChoice::Fabric), Some("0.16.10"));

        let marker = petalfall
            .create_instance_dir("周末生存", &req)
            .await
            .unwrap();
        let dir = petalfall.game_dir().join("versions").join("周末生存");
        assert!(marker_path(&dir).is_file(), "安装开始前就写下标记");
        let outcome = petalfall
            .install_instance("周末生存", &req, None)
            .await
            .unwrap();
        petalfall
            .settle_instance_marker("周末生存", marker, MarkerSettlement::Succeeded)
            .await
            .unwrap();

        assert_eq!(outcome.id, "周末生存");
        assert_eq!(
            outcome.loader,
            Some(InstalledLoader {
                choice: LoaderChoice::Fabric,
                version: "0.16.10".into()
            })
        );
        assert_eq!(outcome.libraries, 1);
        assert!(!marker_path(&dir).exists(), "成功后标记删除");
        assert!(
            !dir.join(PETALFALL_META_DIR)
                .join(STAGED_VANILLA_JSON)
                .exists()
        );
        // versions/ 下只有这一个实例，没有多出原版或 fabric-loader-* 目录。
        assert_eq!(
            petalfall.instance_names().await.unwrap(),
            vec!["周末生存".to_owned()]
        );
        let json: VersionJson =
            serde_json::from_slice(&std::fs::read(dir.join("周末生存.json")).unwrap()).unwrap();
        assert_eq!(json.id, "周末生存");
        assert_eq!(json.inherits_from, None);
        assert_eq!(json.client_version.as_deref(), Some("1.21.1"));
        assert_eq!(json.java_version.unwrap().major_version, 21);

        let list = petalfall.list_instances().await.unwrap();
        let crate::instances::InstanceVersion::Ready(ready) = &list[0].version else {
            panic!("新实例应可用");
        };
        assert_eq!(ready.mc_version.as_deref(), Some("1.21.1"));
        assert_eq!(ready.loaders[0].kind, petalfall_version::LoaderKind::Fabric);
        assert!(list[0].incomplete.is_none());
    }

    fn put_template(petalfall: &Petalfall, files: &[(&str, &[u8])]) {
        let dir = petalfall.template_dir();
        std::fs::create_dir_all(&dir).unwrap();
        for (name, bytes) in files {
            std::fs::write(dir.join(name), bytes).unwrap();
        }
    }

    fn stages(mut rx: tokio::sync::mpsc::UnboundedReceiver<CoreEvent>) -> Vec<String> {
        let mut stages = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if let CoreEvent::Stage(stage) = event {
                stages.push(stage);
            }
        }
        stages
    }

    /// 请求了套用：版本 JSON 写好后补上模板文件，标记里记着这次请求；没请求的实例一个文件都不多。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn requested_template_is_applied_once_the_instance_is_installed() {
        let server = MockServer::start().await;
        mock_meta(&server).await;
        let tmp = tempfile::tempdir().unwrap();
        let petalfall = petalfall_at(tmp.path())
            .with_manifest_url(format!("{}/manifest.json", server.uri()))
            .with_fabric_base(server.uri());
        let servers: Vec<u8> = (0..fastrand::usize(1..2048))
            .map(|_| fastrand::u8(..))
            .collect();
        put_template(
            &petalfall,
            &[
                ("options.txt", b"fov:90\nrenderDistance:16"),
                ("servers.dat", &servers),
            ],
        );
        let req = InstanceRequest {
            use_template: true,
            ..request("1.21.1", Some(LoaderChoice::Fabric), Some("0.16.10"))
        };

        petalfall
            .create_instance_dir("周末生存", &req)
            .await
            .unwrap();
        let dir = petalfall.game_dir().join("versions").join("周末生存");
        let stored = MarkerStore::for_version_dir(&dir)
            .load()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.kind, IncompleteKind::Install { spec: req.spec() });
        assert!(req.spec().use_template);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let outcome = petalfall
            .install_instance("周末生存", &req, Some(&tx))
            .await
            .unwrap();
        drop(tx);

        assert_eq!(
            outcome.template,
            Some(TemplateOutcome::Applied {
                copied: vec!["options.txt", "servers.dat"],
                kept: vec![],
                left_to_mods: vec![],
            })
        );
        assert_eq!(
            std::fs::read(dir.join("options.txt")).unwrap(),
            b"fov:90\nrenderDistance:16"
        );
        assert_eq!(std::fs::read(dir.join("servers.dat")).unwrap(), servers);
        let stages = stages(rx);
        let at = stages
            .iter()
            .position(|s| s == "沿用默认设置：补上 options.txt、servers.dat")
            .unwrap_or_else(|| panic!("{stages:?}"));
        assert!(
            stages[at + 1].starts_with("实例 周末生存 安装完成"),
            "{stages:?}"
        );

        let plain = request("1.21.1", Some(LoaderChoice::Fabric), Some("0.16.10"));
        petalfall.create_instance_dir("不套", &plain).await.unwrap();
        let outcome = petalfall
            .install_instance("不套", &plain, None)
            .await
            .unwrap();
        assert_eq!(outcome.template, None);
        let other = petalfall.game_dir().join("versions").join("不套");
        assert!(!other.join("options.txt").exists());
        assert!(!other.join("servers.dat").exists());
    }

    /// 原版正式版在默认档位下不隔离：版本 JSON 写好后才判得出来，判出来就不碰共用的根目录。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn template_is_skipped_for_an_instance_sharing_the_game_dir() {
        let server = MockServer::start().await;
        mock_meta(&server).await;
        let tmp = tempfile::tempdir().unwrap();
        let petalfall =
            petalfall_at(tmp.path()).with_manifest_url(format!("{}/manifest.json", server.uri()));
        put_template(&petalfall, &[("options.txt", b"fov:90")]);
        std::fs::create_dir_all(petalfall.game_dir()).unwrap();
        std::fs::write(petalfall.game_dir().join("options.txt"), b"fov:70").unwrap();
        let req = InstanceRequest {
            use_template: true,
            ..request("1.21.1", None, None)
        };

        petalfall.create_instance_dir("原版", &req).await.unwrap();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let outcome = petalfall
            .install_instance("原版", &req, Some(&tx))
            .await
            .unwrap();
        drop(tx);

        assert_eq!(
            outcome.template,
            Some(TemplateOutcome::Skipped(
                crate::template::TemplateSkip::SharedWorkingDir
            ))
        );
        assert_eq!(
            std::fs::read(petalfall.game_dir().join("options.txt")).unwrap(),
            b"fov:70"
        );
        let dir = petalfall.game_dir().join("versions").join("原版");
        assert!(!dir.join("options.txt").exists());
        assert!(
            stages(rx).iter().any(
                |s| s == "没有沿用默认设置：这个实例与其他未隔离的实例共用 .minecraft 里的设置"
            ),
        );
    }

    fn drain(mut rx: tokio::sync::mpsc::UnboundedReceiver<CoreEvent>) -> Vec<CoreEvent> {
        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        events
    }

    /// 步骤事件按到达先后的 (步骤表, 下标)。
    fn phases_of(events: &[CoreEvent]) -> Vec<(Vec<InstallPhase>, usize)> {
        events
            .iter()
            .filter_map(|event| match event {
                CoreEvent::Phase { plan, index } => Some((plan.clone(), *index)),
                _ => None,
            })
            .collect()
    }

    /// 新建一路报出的步骤：第一条事件就是第一步，表里每一步按先后各报一次、一步不跳，整份表全程不变。
    /// 套默认设置时这一步照报，哪怕实例共用游戏目录、最后没有套。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn install_announces_every_phase_once_in_plan_order() {
        use InstallPhase::*;
        let server = MockServer::start().await;
        mock_meta(&server).await;
        let tmp = tempfile::tempdir().unwrap();
        let petalfall = petalfall_at(tmp.path())
            .with_manifest_url(format!("{}/manifest.json", server.uri()))
            .with_fabric_base(server.uri());
        put_template(&petalfall, &[("options.txt", b"fov:90")]);
        let cases = [
            (
                "织布",
                request("1.21.1", Some(LoaderChoice::Fabric), Some("0.16.10")),
                vec![Metadata, LoaderProfile, Libraries, Assets, Natives],
            ),
            (
                "原版",
                InstanceRequest {
                    use_template: true,
                    ..request("1.21.1", None, None)
                },
                vec![Metadata, Libraries, Assets, Natives, Template],
            ),
        ];

        for (name, req, plan) in cases {
            petalfall.create_instance_dir(name, &req).await.unwrap();
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            petalfall
                .install_instance(name, &req, Some(&tx))
                .await
                .unwrap();
            drop(tx);

            let events = drain(rx);
            assert!(
                matches!(events[0], CoreEvent::Phase { index: 0, .. }),
                "{name}: {events:?}"
            );
            assert_eq!(
                phases_of(&events),
                (0..plan.len())
                    .map(|i| (plan.clone(), i))
                    .collect::<Vec<_>>(),
                "{name}"
            );
        }
    }

    #[test]
    fn plans_list_the_steps_each_loader_goes_through() {
        use InstallPhase::*;
        let plan = |loader, version: Option<&str>, use_template| {
            install_plan(&InstanceRequest {
                use_template,
                ..request("1.20.1", loader, version)
            })
        };
        for use_template in [false, true] {
            let tail: &[InstallPhase] = if use_template { &[Template] } else { &[] };
            let expect = |steps: &[InstallPhase]| [steps, tail].concat();
            assert_eq!(
                plan(None, None, use_template),
                expect(&[Metadata, Libraries, Assets, Natives])
            );
            for choice in [LoaderChoice::Fabric, LoaderChoice::Quilt] {
                assert_eq!(
                    plan(Some(choice), None, use_template),
                    expect(&[Metadata, LoaderProfile, Libraries, Assets, Natives])
                );
            }
            for (choice, version) in [
                (LoaderChoice::Forge, "47.2.0"),
                (LoaderChoice::NeoForge, "21.1.77"),
            ] {
                assert_eq!(
                    plan(Some(choice), Some(version), use_template),
                    expect(&[
                        Metadata,
                        Libraries,
                        Assets,
                        Natives,
                        Java,
                        LoaderInstaller,
                        LoaderLibraries,
                        LoaderProcessors,
                        Verify,
                    ])
                );
            }
        }
    }

    #[test]
    #[should_panic(expected = "报了两遍")]
    fn announcing_a_phase_twice_panics() {
        let announcer =
            PhaseAnnouncer::new(None, vec![InstallPhase::Metadata, InstallPhase::Libraries]);
        announcer.enter(InstallPhase::Libraries);
        announcer.enter(InstallPhase::Libraries);
    }

    #[test]
    #[should_panic(expected = "不在这次的步骤表")]
    fn announcing_a_phase_outside_the_plan_panics() {
        let announcer =
            PhaseAnnouncer::new(None, vec![InstallPhase::Metadata, InstallPhase::Libraries]);
        announcer.enter(InstallPhase::Template);
    }

    /// 导入整合包开工时预排的表（加载器、远程文件与覆盖文件都还不知道）。
    fn prelude_plan() -> Vec<InstallPhase> {
        use InstallPhase::*;
        vec![
            ModpackDownload,
            ModpackRead,
            Metadata,
            Libraries,
            Assets,
            Natives,
            ModpackFiles,
            ModpackOverrides,
        ]
    }

    /// 改写本身不发事件；改写之后的每一条步骤事件都带新表，下标按新表算，跳过的步骤不占事件。
    #[test]
    fn steps_after_a_revision_carry_the_revised_plan() {
        use InstallPhase::*;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let announcer = PhaseAnnouncer::new(Some(&tx), prelude_plan());
        announcer.enter(ModpackDownload);
        announcer.enter(ModpackRead);
        let exact = vec![
            ModpackDownload,
            ModpackRead,
            Metadata,
            Libraries,
            Assets,
            Natives,
            Java,
            LoaderInstaller,
            LoaderLibraries,
            LoaderProcessors,
            ModpackOverrides,
            Template,
        ];
        let cursor = announcer.revise(exact.clone());
        let before_resume = std::iter::from_fn(|| rx.try_recv().ok()).collect::<Vec<_>>();
        assert_eq!(
            phases_of(&before_resume),
            vec![(prelude_plan(), 0), (prelude_plan(), 1)]
        );

        let resumed = PhaseAnnouncer::resume(Some(&tx), cursor);
        resumed.enter(Metadata);
        resumed.enter(LoaderProcessors);
        resumed.enter(Template);
        drop(resumed);
        drop(tx);
        assert_eq!(
            phases_of(&drain(rx)),
            vec![(exact.clone(), 2), (exact.clone(), 9), (exact, 11)]
        );
    }

    /// 只改还没报到的步骤都不算违约，紧挨着已报前缀的那一步也可以换掉；一步都没报时整张表随便改；表里每一步
    /// 都报过之后还可以往后接。随机切一刀：前 `cut` 步报过，后面换成随机挑的另一些步骤，接着报的下标从 `cut` 起。
    #[test]
    fn revising_only_the_unreported_steps_is_allowed() {
        use InstallPhase::*;
        let seed = fastrand::get_seed();

        // 一步都没报：从预排的表改成毫不相干的表，第一步的下标是 0。
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let cursor = PhaseAnnouncer::new(Some(&tx), prelude_plan()).revise(vec![Verify, Template]);
        PhaseAnnouncer::resume(Some(&tx), cursor).enter(Verify);
        drop(tx);
        assert_eq!(phases_of(&drain(rx)), vec![(vec![Verify, Template], 0)]);

        // 紧挨着已报前缀的那一步（ModpackRead）被删掉，Metadata 顶到下标 1。
        let announcer = PhaseAnnouncer::new(None, prelude_plan());
        announcer.enter(ModpackDownload);
        let cursor = announcer.revise(vec![ModpackDownload, Metadata, Libraries]);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        PhaseAnnouncer::resume(Some(&tx), cursor).enter(Metadata);
        drop(tx);
        assert_eq!(
            phases_of(&drain(rx)),
            vec![(vec![ModpackDownload, Metadata, Libraries], 1)]
        );

        // 整张表都报完了：新表原样保留全部旧步骤，再接一步。
        let announcer = PhaseAnnouncer::new(None, vec![ModpackRead, Metadata]);
        announcer.enter(ModpackRead);
        announcer.enter(Metadata);
        let cursor = announcer.revise(vec![ModpackRead, Metadata, Template]);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        PhaseAnnouncer::resume(Some(&tx), cursor).enter(Template);
        drop(tx);
        assert_eq!(
            phases_of(&drain(rx)),
            vec![(vec![ModpackRead, Metadata, Template], 2)]
        );

        let prelude = prelude_plan();
        let cut = fastrand::usize(0..=prelude.len());
        let mut tail = vec![
            Java,
            LoaderProfile,
            LoaderInstaller,
            LoaderLibraries,
            LoaderProcessors,
            Verify,
            Template,
        ];
        fastrand::shuffle(&mut tail);
        tail.truncate(fastrand::usize(1..=tail.len()));
        let revised = [&prelude[..cut], &tail[..]].concat();
        let announcer = PhaseAnnouncer::new(None, prelude.clone());
        for phase in &prelude[..cut] {
            announcer.enter(*phase);
        }
        let cursor = announcer.revise(revised.clone());
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let resumed = PhaseAnnouncer::resume(Some(&tx), cursor);
        for phase in &tail {
            resumed.enter(*phase);
        }
        drop(resumed);
        drop(tx);
        assert_eq!(
            phases_of(&drain(rx)),
            (cut..revised.len())
                .map(|i| (revised.clone(), i))
                .collect::<Vec<_>>(),
            "seed {seed}，切在 {cut}，后接 {tail:?}"
        );
    }

    /// 表里有的步骤答有、没有的答没有，报没报过都不影响；改写之后按新表答：新加的答有，删掉的答没有。
    #[test]
    fn plans_answers_against_the_current_plan() {
        use InstallPhase::*;
        let announcer = PhaseAnnouncer::new(None, prelude_plan());
        for phase in prelude_plan() {
            assert!(announcer.plans(phase), "{phase:?}");
        }
        for phase in [Template, Verify, Java, LoaderProfile, LoaderProcessors] {
            assert!(!announcer.plans(phase), "{phase:?}");
        }
        announcer.enter(ModpackDownload);
        announcer.enter(ModpackRead);
        assert!(announcer.plans(ModpackDownload), "报过的步骤仍在表里");

        let revised = vec![ModpackDownload, ModpackRead, Metadata, Natives, Template];
        let resumed = PhaseAnnouncer::resume(None, announcer.revise(revised.clone()));
        for phase in &revised {
            assert!(resumed.plans(*phase), "{phase:?}");
        }
        for phase in [Libraries, Assets, ModpackFiles, ModpackOverrides, Verify] {
            assert!(!resumed.plans(phase), "{phase:?}");
        }
    }

    /// 接着报时已报过的步骤不能再报：回到更早的一步。
    #[test]
    #[should_panic(expected = "报在了它后面的步骤之后：")]
    fn a_resumed_announcer_refuses_to_go_back() {
        use InstallPhase::*;
        let announcer = PhaseAnnouncer::new(None, prelude_plan());
        announcer.enter(ModpackDownload);
        announcer.enter(ModpackRead);
        let resumed = PhaseAnnouncer::resume(None, announcer.revise(prelude_plan()));
        resumed.enter(ModpackDownload);
    }

    /// 接着报时已报过的步骤不能再报：把改写前最后报的那一步再报一遍。
    #[test]
    #[should_panic(expected = "报了两遍：上一步报的就是它")]
    fn a_resumed_announcer_refuses_the_last_reported_step_again() {
        use InstallPhase::*;
        let announcer = PhaseAnnouncer::new(None, prelude_plan());
        announcer.enter(ModpackRead);
        let resumed = PhaseAnnouncer::resume(None, announcer.revise(prelude_plan()));
        resumed.enter(ModpackRead);
    }

    /// 已报前缀的最后一步（下标 1）被换掉，哪怕后面原样不动也是违约。
    #[test]
    #[should_panic(expected = "改写步骤表动了已经报过的前 2 步")]
    fn revising_the_last_reported_step_panics() {
        use InstallPhase::*;
        let announcer = PhaseAnnouncer::new(None, prelude_plan());
        announcer.enter(ModpackDownload);
        announcer.enter(ModpackRead);
        let mut revised = prelude_plan();
        revised[1] = Metadata;
        announcer.revise(revised);
    }

    /// 新表比已报前缀还短，同样是动了已报过的步骤。
    #[test]
    #[should_panic(expected = "改写步骤表动了已经报过的前 3 步")]
    fn revising_to_a_plan_shorter_than_the_reported_prefix_panics() {
        use InstallPhase::*;
        let announcer = PhaseAnnouncer::new(None, prelude_plan());
        announcer.enter(ModpackDownload);
        announcer.enter(Metadata);
        announcer.revise(vec![ModpackDownload, ModpackRead]);
    }

    #[test]
    fn request_round_trips_through_the_marker_spec() {
        for use_template in [true, false] {
            for (loader, version) in [
                (None, None),
                (Some(LoaderChoice::Fabric), None),
                (Some(LoaderChoice::Quilt), Some("0.27.1")),
                (Some(LoaderChoice::Forge), Some("47.4.20")),
                (Some(LoaderChoice::NeoForge), Some("21.1.77")),
            ] {
                let req = InstanceRequest {
                    use_template,
                    ..request("1.21.1", loader, version)
                };
                let spec = req.spec();
                assert_eq!(spec.use_template, use_template);
                assert_eq!(InstanceRequest::from_spec("x", &spec).unwrap(), req);
            }
        }
    }

    #[test]
    fn template_stage_names_copied_and_kept_files() {
        use crate::template::TemplateSkip;
        assert_eq!(
            template_stage(&TemplateOutcome::Applied {
                copied: vec!["servers.dat"],
                kept: vec!["options.txt", "optionsof.txt"],
                left_to_mods: vec![],
            }),
            "沿用默认设置：补上 servers.dat（已有的 options.txt、optionsof.txt 保留不动）"
        );
        assert_eq!(
            template_stage(&TemplateOutcome::Applied {
                copied: vec![],
                kept: vec!["options.txt"],
                left_to_mods: vec![],
            }),
            "沿用默认设置：已有的 options.txt 保留不动"
        );
        assert_eq!(
            template_stage(&TemplateOutcome::Applied {
                copied: vec!["options.txt", "servers.dat"],
                kept: vec![],
                left_to_mods: vec![],
            }),
            "沿用默认设置：补上 options.txt、servers.dat"
        );
        assert_eq!(
            template_stage(&TemplateOutcome::Applied {
                copied: vec!["optionsof.txt"],
                kept: vec!["optionsshaders.txt"],
                left_to_mods: vec!["options.txt", "servers.dat"],
            }),
            "沿用默认设置：补上 optionsof.txt（已有的 optionsshaders.txt 保留不动；options.txt、servers.dat 留给整合包里模组的默认设置）"
        );
        assert_eq!(
            template_stage(&TemplateOutcome::Applied {
                copied: vec!["servers.dat"],
                kept: vec![],
                left_to_mods: vec!["options.txt"],
            }),
            "沿用默认设置：补上 servers.dat（options.txt 留给整合包里模组的默认设置）"
        );
        assert_eq!(
            template_stage(&TemplateOutcome::Applied {
                copied: vec![],
                kept: vec![],
                left_to_mods: vec!["options.txt"],
            }),
            "沿用默认设置：options.txt 留给整合包里模组的默认设置"
        );
        assert_eq!(
            template_stage(&TemplateOutcome::Skipped(TemplateSkip::Empty)),
            "没有沿用默认设置：还没有默认设置"
        );
        assert_eq!(
            template_stage(&TemplateOutcome::Skipped(TemplateSkip::Disabled)),
            "没有沿用默认设置：新建实例时沿用默认设置已在设置的「游戏」分区关闭"
        );
    }

    #[tokio::test]
    async fn creating_an_existing_name_reports_duplicate_and_leaves_it_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let petalfall = petalfall_at(tmp.path());
        let dir = write_version(petalfall.game_dir(), "Survival", &vanilla_json("1.21.1"));
        let err = petalfall
            .create_instance_dir("survival", &request("1.21.1", None, None))
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            CoreError::Instance(InstanceError::InvalidName {
                violation: NameViolation::Duplicate,
                ..
            })
        ));
        assert!(!marker_path(&dir).exists());

        let err = petalfall
            .create_instance_dir("x", &request("1.21.1", Some(LoaderChoice::Forge), None))
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            CoreError::Install(petalfall_install::Error::LoaderVersionNotFound { .. })
        ));
        assert!(!petalfall.game_dir().join("versions").join("x").exists());
    }

    /// 同名旧实例在启动器之外（资源管理器、共用游戏目录的别的启动器）被删掉后，它的游玩记录还在；新建的
    /// 同名实例从零算起。撞名失败时不动已有实例的记录，别的实例与别的游戏目录的记录也不动。
    #[tokio::test]
    async fn new_instance_does_not_inherit_records_of_a_vanished_namesake() {
        let tmp = tempfile::tempdir().unwrap();
        let petalfall = petalfall_at(tmp.path());
        let other = tmp.path().join("HMCL").join(".minecraft");
        let hour = 3_600_000;
        for (game_dir, id) in [
            (petalfall.game_dir(), "周末生存"),
            (petalfall.game_dir(), "Survival"),
            (petalfall.game_dir(), "别的"),
            (other.as_path(), "周末生存"),
        ] {
            crate::playtime::record_play(tmp.path(), game_dir, id, 0, hour)
                .await
                .unwrap();
        }
        write_version(petalfall.game_dir(), "Survival", &vanilla_json("1.21.1"));

        petalfall
            .create_instance_dir("survival", &request("1.21.1", None, None))
            .await
            .unwrap_err();
        petalfall
            .create_instance_dir("周末生存", &request("1.21.1", None, None))
            .await
            .unwrap();

        let summaries = petalfall.play_summaries().await.unwrap();
        assert_eq!(
            summaries.keys().collect::<Vec<_>>(),
            vec!["Survival", "别的"]
        );
        assert_eq!(summaries["Survival"].playtime_min, 60);
        let elsewhere = Petalfall::for_test(
            crate::config::PetalfallConfig::default(),
            tmp.path().to_path_buf(),
            other,
        );
        assert_eq!(
            elsewhere.play_summaries().await.unwrap()["周末生存"].playtime_min,
            60
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn failed_install_keeps_the_directory_with_a_failed_marker_and_repair_reinstalls() {
        let server = MockServer::start().await;
        mock_meta(&server).await;
        let tmp = tempfile::tempdir().unwrap();
        let petalfall = petalfall_at(tmp.path())
            .with_manifest_url(format!("{}/manifest.json", server.uri()))
            .with_fabric_base(server.uri());
        // 不存在的 loader 版本：meta 404，安装失败。
        let bad = request("1.21.1", Some(LoaderChoice::Fabric), Some("9.9.9"));
        let marker = petalfall.create_instance_dir("半成品", &bad).await.unwrap();
        let err = petalfall
            .install_instance("半成品", &bad, None)
            .await
            .unwrap_err();
        petalfall
            .settle_instance_marker(
                "半成品",
                marker,
                MarkerSettlement::Failed(MarkerError {
                    code: "install_failed".into(),
                    message: err.to_string(),
                }),
            )
            .await
            .unwrap();

        let dir = petalfall.game_dir().join("versions").join("半成品");
        let stored = MarkerStore::for_version_dir(&dir)
            .load()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.outcome, IncompleteOutcome::Failed);
        assert_eq!(stored.error.as_ref().unwrap().code, "install_failed");
        assert_eq!(stored.kind, IncompleteKind::Install { spec: bad.spec() });

        // 修复照标记里的内容重装；把标记里的版本改成存在的那个，模拟「网络恢复后重试」。
        let good = request("1.21.1", Some(LoaderChoice::Fabric), Some("0.16.10"));
        MarkerStore::for_version_dir(&dir)
            .save(&IncompleteMarker {
                kind: IncompleteKind::Install { spec: good.spec() },
                ..stored
            })
            .await
            .unwrap();
        let (plan, marker) = petalfall.begin_repair("半成品").await.unwrap();
        assert_eq!(plan, RepairPlan::Reinstall(good));
        assert_eq!(marker.outcome, IncompleteOutcome::Pending);
        let outcome = petalfall
            .repair_instance("半成品", &plan, None)
            .await
            .unwrap();
        assert!(outcome.reinstalled);
        petalfall
            .settle_instance_marker("半成品", marker, MarkerSettlement::Succeeded)
            .await
            .unwrap();
        assert!(!marker_path(&dir).exists(), "修复成功后标记清掉");
        assert!(dir.join("半成品.json").is_file());
    }

    #[tokio::test]
    async fn repair_of_a_complete_instance_writes_then_clears_a_repair_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let petalfall = petalfall_at(tmp.path());
        let dir = write_version(
            petalfall.game_dir(),
            "1.21.1",
            r#"{"id":"1.21.1","type":"release","mainClass":"M"}"#,
        );
        let (plan, marker) = petalfall.begin_repair("1.21.1").await.unwrap();
        assert_eq!(plan, RepairPlan::Complete);
        let on_disk = MarkerStore::for_version_dir(&dir)
            .load()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(on_disk.kind, IncompleteKind::Repair);
        assert_eq!(on_disk.outcome, IncompleteOutcome::Pending);

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let outcome = petalfall
            .repair_instance("1.21.1", &plan, Some(&tx))
            .await
            .unwrap();
        drop(tx);
        assert!(!outcome.reinstalled);
        assert_eq!(outcome.libraries, 0);
        let steps = vec![
            InstallPhase::Libraries,
            InstallPhase::Assets,
            InstallPhase::Natives,
        ];
        assert_eq!(
            phases_of(&drain(rx)),
            vec![(steps.clone(), 0), (steps.clone(), 1), (steps, 2)]
        );
        petalfall
            .settle_instance_marker("1.21.1", marker.clone(), MarkerSettlement::Succeeded)
            .await
            .unwrap();
        assert!(!marker_path(&dir).exists());

        // 被取消时标记留下并记为 cancelled。
        petalfall
            .settle_instance_marker("1.21.1", marker, MarkerSettlement::Cancelled)
            .await
            .unwrap();
        let cancelled = MarkerStore::for_version_dir(&dir)
            .load()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cancelled.outcome, IncompleteOutcome::Cancelled);
    }

    /// 整合包导入中断的实例即使版本 JSON 已经写好也不能按「补全」处理：报不可补全，标记原样留着。
    /// 装好之后版本 JSON 被手工删掉或写坏：这时已经没有未完成标记，只有安装记录说得出当初装的是什么。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn lost_version_json_is_reinstalled_from_the_install_record() {
        let server = MockServer::start().await;
        mock_meta(&server).await;
        let tmp = tempfile::tempdir().unwrap();
        let petalfall = petalfall_at(tmp.path())
            .with_manifest_url(format!("{}/manifest.json", server.uri()))
            .with_fabric_base(server.uri());
        let id = "周末生存";
        let req = request("1.21.1", Some(LoaderChoice::Fabric), Some("0.16.10"));
        let marker = petalfall.create_instance_dir(id, &req).await.unwrap();
        petalfall.install_instance(id, &req, None).await.unwrap();
        petalfall
            .settle_instance_marker(id, marker, MarkerSettlement::Succeeded)
            .await
            .unwrap();
        let dir = petalfall.game_dir().join("versions").join(id);
        let json = dir.join(format!("{id}.json"));
        let installed = std::fs::read_to_string(&json).unwrap();
        // 玩家的 Mod 不能因为修复而丢。
        std::fs::create_dir_all(dir.join("mods")).unwrap();
        std::fs::write(dir.join("mods").join("sodium.jar"), b"jar").unwrap();

        for damage in [
            Box::new(|p: &std::path::Path| std::fs::remove_file(p).unwrap())
                as Box<dyn Fn(&std::path::Path)>,
            Box::new(|p: &std::path::Path| std::fs::write(p, "{ not json").unwrap()),
        ] {
            damage(&json);
            let (plan, marker) = petalfall.begin_repair(id).await.unwrap();
            assert_eq!(plan, RepairPlan::Reinstall(req.clone()));
            assert!(matches!(marker.kind, IncompleteKind::Install { .. }));
            let outcome = petalfall.repair_instance(id, &plan, None).await.unwrap();
            assert!(outcome.reinstalled);
            petalfall
                .settle_instance_marker(id, marker, MarkerSettlement::Succeeded)
                .await
                .unwrap();
            assert_eq!(std::fs::read_to_string(&json).unwrap(), installed);
        }
        assert!(dir.join("mods").join("sodium.jar").exists());
        assert!(!marker_path(&dir).exists());
    }

    /// 没指定版本时记下的是这次实际装上的，不是「推荐版」：日后修复不该换掉玩家的加载器版本。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn install_record_keeps_the_version_that_was_actually_installed() {
        let server = MockServer::start().await;
        mock_meta(&server).await;
        Mock::given(method("GET"))
            .and(path("/v2/versions/loader/1.21.1"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"[{"loader":{"separator":".","build":10,"maven":"m","version":"0.16.10","stable":true}}]"#,
            ))
            .mount(&server)
            .await;
        let tmp = tempfile::tempdir().unwrap();
        let petalfall = petalfall_at(tmp.path())
            .with_manifest_url(format!("{}/manifest.json", server.uri()))
            .with_fabric_base(server.uri());
        let req = InstanceRequest {
            use_template: true,
            ..request("1.21.1", Some(LoaderChoice::Fabric), None)
        };
        petalfall.create_instance_dir("推荐版", &req).await.unwrap();
        petalfall
            .install_instance("推荐版", &req, None)
            .await
            .unwrap();
        let dir = petalfall.game_dir().join("versions").join("推荐版");
        assert_eq!(
            InstallRecordStore::for_version_dir(&dir)
                .load()
                .await
                .unwrap(),
            Some(InstallSpec {
                mc_version: "1.21.1".into(),
                loader: Some("fabric".into()),
                loader_version: Some("0.16.10".into()),
                use_template: false,
            })
        );
    }

    #[tokio::test]
    async fn interrupted_modpack_import_is_not_repairable_and_keeps_its_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let petalfall = petalfall_at(tmp.path());
        let dir = write_version(petalfall.game_dir(), "整合包", &vanilla_json("1.21.1"));
        let store = MarkerStore::for_version_dir(&dir);
        let marker = IncompleteMarker::pending(
            IncompleteKind::Modpack {
                task_id: "mp#1".into(),
            },
            1_700_000_000_000,
        );
        store.save(&marker).await.unwrap();

        let err = petalfall.begin_repair("整合包").await.unwrap_err();
        match err {
            CoreError::Instance(InstanceError::NotRepairable { id, reason }) => {
                assert_eq!(id, "整合包");
                assert!(reason.contains("重新导入"), "{reason}");
            }
            other => panic!("应报不可补全：{other:?}"),
        }
        assert_eq!(store.load().await.unwrap(), Some(marker));
    }

    #[tokio::test]
    async fn broken_instance_without_install_record_is_not_repairable() {
        let tmp = tempfile::tempdir().unwrap();
        let petalfall = petalfall_at(tmp.path());
        std::fs::create_dir_all(petalfall.game_dir().join("versions").join("空的")).unwrap();
        let err = petalfall.begin_repair("空的").await.unwrap_err();
        assert!(
            matches!(
                err,
                CoreError::Instance(InstanceError::NotRepairable { .. })
            ),
            "实际：{err:?}"
        );
    }

    /// 对一个实例发起补全的结论：修得好时是定下的方案，修不好时是报的原因。
    async fn repair_verdict(
        petalfall: &Petalfall,
        id: &str,
    ) -> std::result::Result<RepairPlan, String> {
        match petalfall.begin_repair(id).await {
            Ok((plan, _)) => Ok(plan),
            Err(CoreError::Instance(InstanceError::NotRepairable { reason, .. })) => Err(reason),
            Err(other) => panic!("{id} 应定出方案或报不可补全：{other:?}"),
        }
    }

    /// 实例列表说的修不好的原因就是补全报的那句：修不好的逐字相同，修得好的列表上为空、补全也不拒。
    #[tokio::test]
    async fn listed_unrepairable_reason_matches_what_repair_reports() {
        let tmp = tempfile::tempdir().unwrap();
        let petalfall = petalfall_at(tmp.path());
        let game = petalfall.game_dir().to_path_buf();
        let versions = game.join("versions");
        let fabric = request("1.21.1", Some(LoaderChoice::Fabric), Some("0.16.10"));
        // 装好之后版本 JSON 被删：只剩安装记录。
        InstallRecordStore::for_version_dir(&versions.join("有记录"))
            .save(&fabric.spec())
            .await
            .unwrap();
        // 新建在写出版本 JSON 之前失败：带安装标记。
        MarkerStore::for_version_dir(&versions.join("装到一半"))
            .save(&IncompleteMarker::pending(
                IncompleteKind::Install {
                    spec: fabric.spec(),
                },
                1_700_000_000_000,
            ))
            .await
            .unwrap();
        // 整合包导入在写出版本 JSON 之前中断：带整合包标记。
        MarkerStore::for_version_dir(&versions.join("导到一半"))
            .save(&IncompleteMarker::pending(
                IncompleteKind::Modpack {
                    task_id: "mp#1".into(),
                },
                1_700_000_000_000,
            ))
            .await
            .unwrap();
        write_version(&game, "完好", &vanilla_json("完好"));
        std::fs::create_dir_all(versions.join("空的")).unwrap();
        write_version(&game, "写坏了", "{ not json");
        // 安装记录里的加载器这一版认不出来：照它装不回去，与没有记录一样修不好。
        InstallRecordStore::for_version_dir(&versions.join("认不出加载器"))
            .save(&InstallSpec {
                loader: Some("rift".into()),
                ..fabric.spec()
            })
            .await
            .unwrap();
        // 安装标记里的加载器认不出来：原因点出的是未完成标记，不是安装记录。
        MarkerStore::for_version_dir(&versions.join("标记里认不出"))
            .save(&IncompleteMarker::pending(
                IncompleteKind::Install {
                    spec: InstallSpec {
                        loader: Some("rift".into()),
                        ..fabric.spec()
                    },
                },
                1_700_000_000_000,
            ))
            .await
            .unwrap();

        let list = petalfall.list_instances().await.unwrap();

        let expected = [
            ("有记录", Ok(RepairPlan::Reinstall(fabric.clone()))),
            ("装到一半", Ok(RepairPlan::Reinstall(fabric.clone()))),
            (
                "导到一半",
                Err(refusal(
                    UnrepairableKind::Modpack,
                    "整合包没有导入完，缺的 Mod 与配置补不回来，删掉这个实例后重新导入",
                )),
            ),
            ("完好", Ok(RepairPlan::Complete)),
            (
                "空的",
                Err(no_record("缺少版本 JSON，也没有记下当初装的是什么")),
            ),
            (
                "写坏了",
                Err(no_record(
                    "版本 JSON 损坏：key must be a string at line 1 column 3，也没有记下当初装的是什么",
                )),
            ),
            (
                "认不出加载器",
                Err(no_record("安装记录里记的加载器 rift 不认识")),
            ),
            (
                "标记里认不出",
                Err(no_record("未完成标记里记的加载器 rift 不认识")),
            ),
        ];
        assert_eq!(list.len(), expected.len());
        for (id, verdict) in expected {
            let listed = list.iter().find(|i| i.id == id).unwrap();
            assert_eq!(listed.unrepairable, verdict.clone().err(), "{id} 在列表上");
            assert_eq!(
                repair_verdict(&petalfall, id).await,
                verdict.map_err(|refusal| refusal.reason),
                "{id} 去补全"
            );
        }
    }

    fn refusal(kind: UnrepairableKind, reason: &str) -> Unrepairable {
        Unrepairable {
            kind,
            reason: reason.to_owned(),
        }
    }

    /// 没有安装记录、原因里也不带改名提示的那一种。
    fn no_record(reason: &str) -> Unrepairable {
        refusal(UnrepairableKind::NoRecord { rename_to: None }, reason)
    }

    /// 版本 JSON 读不出来（这里让 `<id>.json` 是个目录，读文件必报 IO 错）：有没有安装记录都修不好。列表上的原因与
    /// 补全报的是同一句「版本 JSON 读不出来：」后接读盘错误与系统给的原因，不说「也没有记下当初装的是什么」；补全在
    /// 写标记之前就拒绝，不会照安装记录去重装。
    #[tokio::test]
    async fn an_unreadable_version_json_is_not_repairable_with_or_without_an_install_record() {
        let tmp = tempfile::tempdir().unwrap();
        let petalfall = petalfall_at(tmp.path());
        let versions = petalfall.game_dir().join("versions");
        let fabric = request("1.21.1", Some(LoaderChoice::Fabric), Some("0.16.10"));
        InstallRecordStore::for_version_dir(&versions.join("有记录"))
            .save(&fabric.spec())
            .await
            .unwrap();
        let ids = ["有记录", "没记录"];
        for id in ids {
            std::fs::create_dir_all(versions.join(id).join(format!("{id}.json"))).unwrap();
        }

        let list = petalfall.list_instances().await.unwrap();
        assert_eq!(list.len(), ids.len());
        for id in ids {
            let json = versions.join(id).join(format!("{id}.json"));
            let expected = format!(
                "版本 JSON 读不出来：文件 IO 失败: {}：{}",
                json.display(),
                std::fs::read(&json).unwrap_err()
            );
            let listed = list.iter().find(|i| i.id == id).unwrap();
            assert!(
                matches!(
                    &listed.version,
                    crate::instances::InstanceVersion::Broken(BrokenReason::Unreadable(_))
                ),
                "{id}：{:?}",
                listed.version
            );
            assert_eq!(
                listed.unrepairable,
                Some(refusal(UnrepairableKind::Unreadable, &expected)),
                "{id} 在列表上"
            );
            assert_eq!(
                repair_verdict(&petalfall, id).await,
                Err(expected),
                "{id} 去补全"
            );
            assert!(
                !marker_path(&versions.join(id)).exists(),
                "{id} 补全在写标记之前就拒绝"
            );
        }
    }

    /// 版本 JSON 读不出来、未完成标记是 install（新建在写出版本 JSON 之后失败）：照标记重装最后也要把版本 JSON 写回
    /// 那个位置，同样修不好。列表上的原因与补全、重装加载器报的都是读不出来那句，两者在写标记之前就拒绝，原来的标记
    /// 原样留着。整合包没导入完的照旧是重新导入那句。
    #[tokio::test]
    async fn an_unreadable_version_json_is_not_repairable_from_an_install_marker_either() {
        let tmp = tempfile::tempdir().unwrap();
        let petalfall = petalfall_at(tmp.path());
        let versions = petalfall.game_dir().join("versions");
        let fabric = request("1.21.1", Some(LoaderChoice::Fabric), Some("0.16.10"));
        let install = IncompleteMarker {
            error: Some(MarkerError {
                code: "network_failed".into(),
                message: "连不上".into(),
            }),
            outcome: IncompleteOutcome::Failed,
            ..IncompleteMarker::pending(
                IncompleteKind::Install {
                    spec: fabric.spec(),
                },
                1_700_000_000_000,
            )
        };
        let modpack = IncompleteMarker::pending(
            IncompleteKind::Modpack {
                task_id: "mp#1".into(),
            },
            1_700_000_000_000,
        );
        for (id, marker) in [("装到一半", &install), ("导到一半", &modpack)] {
            std::fs::create_dir_all(versions.join(id).join(format!("{id}.json"))).unwrap();
            MarkerStore::for_version_dir(&versions.join(id))
                .save(marker)
                .await
                .unwrap();
        }
        let json = versions.join("装到一半").join("装到一半.json");
        let expected = [
            (
                "装到一半",
                &install,
                refusal(
                    UnrepairableKind::Unreadable,
                    &format!(
                        "版本 JSON 读不出来：文件 IO 失败: {}：{}",
                        json.display(),
                        std::fs::read(&json).unwrap_err()
                    ),
                ),
            ),
            (
                "导到一半",
                &modpack,
                refusal(
                    UnrepairableKind::Modpack,
                    "整合包没有导入完，缺的 Mod 与配置补不回来，删掉这个实例后重新导入",
                ),
            ),
        ];

        let list = petalfall.list_instances().await.unwrap();
        assert_eq!(list.len(), expected.len());
        for (id, marker, unrepairable) in expected {
            let listed = list.iter().find(|i| i.id == id).unwrap();
            assert!(
                matches!(
                    &listed.version,
                    crate::instances::InstanceVersion::Broken(BrokenReason::Unreadable(_))
                ),
                "{id}：{:?}",
                listed.version
            );
            assert_eq!(
                listed.unrepairable.as_ref(),
                Some(&unrepairable),
                "{id} 在列表上"
            );
            assert_eq!(
                repair_verdict(&petalfall, id).await,
                Err(unrepairable.reason.clone()),
                "{id} 去补全"
            );
            match petalfall.begin_loader_reinstall(id).await {
                Err(CoreError::Instance(InstanceError::NotRepairable { reason, .. })) => {
                    assert_eq!(reason, unrepairable.reason, "{id} 重装加载器");
                }
                other => panic!("{id} 重装加载器应报不可补全：{other:?}"),
            }
            assert_eq!(
                MarkerStore::for_version_dir(&versions.join(id))
                    .load()
                    .await
                    .unwrap()
                    .as_ref(),
                Some(marker),
                "{id} 在写标记之前就拒绝"
            );
        }
    }

    /// 别人打包时把实例目录改了名、里面的版本 JSON 还叫原来的名字：恰好一份时列表与补全都点出它的名字，没有或不止
    /// 一份时不提。只看缺版本 JSON 的实例目录顶层：id 对不上自己文件名的、不是版本 JSON 的、子目录里的都不算。
    #[tokio::test]
    async fn a_misnamed_version_json_is_named_only_when_there_is_exactly_one() {
        let tmp = tempfile::tempdir().unwrap();
        let petalfall = petalfall_at(tmp.path());
        let game = petalfall.game_dir().to_path_buf();
        let versions = game.join("versions");
        let put = |dir: &std::path::Path, name: &str, contents: &str| {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join(name), contents).unwrap();
        };
        let one = versions.join("atm10");
        put(
            &one,
            "All the Mods 10.json",
            &vanilla_json("All the Mods 10"),
        );
        put(&one, "改过 id.json", &vanilla_json("别的名字"));
        put(&one, "usercache.json", "[]");
        put(&one.join("config"), "config.json", &vanilla_json("config"));
        put(&versions.join("一份也没有"), "usercache.json", "[]");
        let two = versions.join("两份");
        put(&two, "甲.json", &vanilla_json("甲"));
        put(&two, "乙.json", &vanilla_json("乙"));
        // 版本 JSON 在、只是写坏了，不是名字对不上：旁边那份不提。
        let corrupt = write_version(&game, "写坏了", "{ not json");
        put(&corrupt, "丙.json", &vanilla_json("丙"));

        let missing = "缺少版本 JSON，也没有记下当初装的是什么";
        let expected = [
            (
                "atm10",
                refusal(
                    UnrepairableKind::NoRecord {
                        rename_to: Some("All the Mods 10".to_owned()),
                    },
                    &format!(
                        "{missing}；目录里有一份叫 All the Mods 10.json 的版本文件，把实例改名成 All the Mods 10 就能认出来"
                    ),
                ),
            ),
            ("一份也没有", no_record(missing)),
            ("两份", no_record(missing)),
            (
                "写坏了",
                no_record(
                    "版本 JSON 损坏：key must be a string at line 1 column 3，也没有记下当初装的是什么",
                ),
            ),
        ];
        let list = petalfall.list_instances().await.unwrap();
        assert_eq!(list.len(), expected.len());
        for (id, unrepairable) in expected {
            let listed = list.iter().find(|i| i.id == id).unwrap();
            assert_eq!(
                listed.unrepairable.as_ref(),
                Some(&unrepairable),
                "{id} 在列表上"
            );
            assert_eq!(
                repair_verdict(&petalfall, id).await,
                Err(unrepairable.reason),
                "{id} 去补全"
            );
        }
    }

    /// 名字对不上的那份版本 JSON，只有实例改得成它的名字时才点出来：改名的校验（命名规则加不分大小写查重）过不了的，
    /// 照做只会被改名拒绝，首尾带空白的在界面提交时会被去掉，同样改不成，原因里都不提改名，与零份、多份时一样。
    #[tokio::test]
    async fn a_misnamed_version_json_is_named_only_when_the_instance_can_take_that_name() {
        let tmp = tempfile::tempdir().unwrap();
        let petalfall = petalfall_at(tmp.path());
        let versions = petalfall.game_dir().join("versions");
        let put = |id: &str, name: &str| {
            let dir = versions.join(id);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(format!("{name}.json")), vanilla_json(name)).unwrap();
        };
        put("改得成", "All the Mods 10");
        put("太长", &"樱".repeat(33));
        put("段尾空格", "生存 ");
        put("段尾点", "生存.");
        put("点开头", ".atm");
        put("开头空格", " 生存");
        put("全角空格", "生存\u{3000}");
        put("撞名", "atm10");
        // 已有的实例，与撞名那份只差大小写：Windows 上是同一个文件夹。
        std::fs::create_dir_all(versions.join("ATM10")).unwrap();

        let missing = "缺少版本 JSON，也没有记下当初装的是什么";
        let hinted = refusal(
            UnrepairableKind::NoRecord {
                rename_to: Some("All the Mods 10".to_owned()),
            },
            &format!(
                "{missing}；目录里有一份叫 All the Mods 10.json 的版本文件，把实例改名成 All the Mods 10 就能认出来"
            ),
        );
        let expected = [
            ("改得成", hinted),
            ("太长", no_record(missing)),
            ("段尾空格", no_record(missing)),
            ("段尾点", no_record(missing)),
            ("点开头", no_record(missing)),
            ("开头空格", no_record(missing)),
            ("全角空格", no_record(missing)),
            ("撞名", no_record(missing)),
            ("ATM10", no_record(missing)),
        ];
        let list = petalfall.list_instances().await.unwrap();
        assert_eq!(list.len(), expected.len());
        for (id, unrepairable) in expected {
            let listed = list.iter().find(|i| i.id == id).unwrap();
            assert_eq!(
                listed.unrepairable.as_ref(),
                Some(&unrepairable),
                "{id} 在列表上"
            );
            assert_eq!(
                repair_verdict(&petalfall, id).await,
                Err(unrepairable.reason),
                "{id} 去补全"
            );
        }

        // 命名规则或查重过不了、所以没给提示的，照着名字去改名确实被拒：提示与改名两边的判断由这里绑在一起。
        for (id, name, violation) in [
            ("撞名", "atm10".to_owned(), NameViolation::Duplicate),
            ("太长", "樱".repeat(33), NameViolation::TooLong),
            (
                "段尾空格",
                "生存 ".to_owned(),
                NameViolation::ReservedByWindows,
            ),
            (
                "段尾点",
                "生存.".to_owned(),
                NameViolation::ReservedByWindows,
            ),
            ("点开头", ".atm".to_owned(), NameViolation::LeadingDot),
        ] {
            match petalfall.rename_instance_files(id, &name).await {
                Err(CoreError::Instance(InstanceError::InvalidName { violation: got, .. })) => {
                    assert_eq!(got, violation, "{id}")
                }
                other => panic!("{id} 改名成 {name:?} 应被拒：{other:?}"),
            }
        }

        // 照提示真的改一次名：改完那份版本 JSON 原样认得出来，重新列表时实例完好，不再有修不好的判定。
        let original = std::fs::read(versions.join("改得成").join("All the Mods 10.json")).unwrap();
        petalfall
            .rename_instance_files("改得成", "All the Mods 10")
            .await
            .unwrap();
        let renamed = versions
            .join("All the Mods 10")
            .join("All the Mods 10.json");
        assert_eq!(std::fs::read(&renamed).unwrap(), original);
        match probe_version(petalfall.game_dir(), "All the Mods 10")
            .await
            .unwrap()
        {
            VersionProbe::Found(version) => {
                assert_eq!(version.json.id, "All the Mods 10");
                assert_eq!(version.json_path, renamed);
            }
            VersionProbe::Broken(broken) => panic!("改名后应认得出：{broken:?}"),
        }
        let list = petalfall.list_instances().await.unwrap();
        assert!(list.iter().all(|i| i.id != "改得成"));
        let fixed = list.iter().find(|i| i.id == "All the Mods 10").unwrap();
        assert!(
            matches!(fixed.version, crate::instances::InstanceVersion::Ready(_)),
            "{:?}",
            fixed.version
        );
        assert_eq!(fixed.unrepairable, None);
    }

    /// Windows 的文件名里放不下的名字（保留字符、控制字符、设备名）在盘上造不出那样的版本 JSON，直接拿名字核对：过不了
    /// 改名校验的不交回，过得了的原样交回。
    #[test]
    fn only_a_name_the_instance_can_be_renamed_to_is_suggested() {
        let existing = ["atm10".to_owned(), "RLCraft".to_owned()];
        assert_eq!(
            renamable_to("atm10", "All the Mods 10".to_owned(), &existing).as_deref(),
            Some("All the Mods 10")
        );
        let too_long = "樱".repeat(33);
        for name in [
            "a:b",
            "a?b",
            "a\u{7}b",
            "CON",
            "nul.tar",
            too_long.as_str(),
            "生存 ",
            "生存.",
            ".atm",
            "rlcraft",
            // 命名规则放行、界面提交时却会被去掉的首尾空白：开头的半角空格，段尾的全角空格、不换行空格与 U+FEFF。
            " ATM10",
            "生存\u{3000}",
            "生存\u{a0}",
            "\u{feff}生存",
        ] {
            assert_eq!(
                renamable_to("atm10", name.to_owned(), &existing),
                None,
                "{name:?}"
            );
        }
        // 中间的空白不受影响。
        assert_eq!(
            renamable_to("atm10", "All\u{3000}the Mods".to_owned(), &existing).as_deref(),
            Some("All\u{3000}the Mods")
        );
    }

    /// 实例目录里的 Fabric 版本 JSON：`clientVersion` 记着原版版本号，改过名也认得出。
    fn fabric_instance_json(id: &str, loader: &str) -> String {
        format!(
            r#"{{"id":"{id}","type":"release","clientVersion":"1.21.1",
                "mainClass":"net.fabricmc.loader.impl.launch.knot.KnotClient",
                "libraries":[{{"name":"net.fabricmc:fabric-loader:{loader}"}}]}}"#
        )
    }

    /// 重新安装加载器照原样把版本 JSON 与加载器装回：Mod、配置、存档一个字节不动，默认设置也不套，
    /// 否则玩家的 options.txt 之外的文件会被模板补进来。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reinstalling_the_loader_rewrites_it_and_leaves_player_files_alone() {
        let server = MockServer::start().await;
        mock_meta(&server).await;
        let tmp = tempfile::tempdir().unwrap();
        let petalfall = petalfall_at(tmp.path())
            .with_manifest_url(format!("{}/manifest.json", server.uri()))
            .with_fabric_base(server.uri());
        put_template(&petalfall, &[("servers.dat", b"template")]);
        let dir = write_version(
            petalfall.game_dir(),
            "周末生存",
            &fabric_instance_json("周末生存", "0.16.10"),
        );
        let world: Vec<u8> = (0..fastrand::usize(1..2048))
            .map(|_| fastrand::u8(..))
            .collect();
        let kept: [(&str, &[u8]); 4] = [
            ("mods/sodium.jar", b"mod-bytes"),
            ("config/sodium.toml", b"[sodium]\nfog = false\n"),
            ("saves/新世界/level.dat", &world),
            ("options.txt", b"fov:90"),
        ];
        for (path, bytes) in kept {
            let file = dir.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, bytes).unwrap();
        }

        let (plan, marker) = petalfall.begin_loader_reinstall("周末生存").await.unwrap();
        let expected = request("1.21.1", Some(LoaderChoice::Fabric), Some("0.16.10"));
        assert_eq!(plan, RepairPlan::Reinstall(expected.clone()));
        let on_disk = MarkerStore::for_version_dir(&dir)
            .load()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            on_disk.kind,
            IncompleteKind::Install {
                spec: expected.spec()
            },
            "中途失败时补全要照着这份内容接着重装"
        );
        assert_eq!(on_disk.outcome, IncompleteOutcome::Pending);

        let outcome = petalfall
            .repair_instance("周末生存", &plan, None)
            .await
            .unwrap();
        petalfall
            .settle_instance_marker("周末生存", marker, MarkerSettlement::Succeeded)
            .await
            .unwrap();

        assert_eq!(
            outcome,
            RepairOutcome {
                reinstalled: true,
                libraries: 1,
                assets: 0,
                natives: 0
            }
        );
        assert!(!marker_path(&dir).exists(), "重装成功后标记清掉");
        let json: VersionJson =
            serde_json::from_slice(&std::fs::read(dir.join("周末生存.json")).unwrap()).unwrap();
        assert_eq!(json.client_version.as_deref(), Some("1.21.1"));
        assert_eq!(
            json.libraries
                .iter()
                .map(|l| l.name.as_str())
                .collect::<Vec<_>>(),
            vec!["net.example:loader:0.16.10"],
            "版本 JSON 换成了加载器服务给的那一份"
        );
        for (path, bytes) in kept {
            assert_eq!(std::fs::read(dir.join(path)).unwrap(), bytes, "{path}");
        }
        assert!(!dir.join("servers.dat").exists(), "重装不套默认设置");
    }

    /// 版本号照现有版本 JSON 读：改过名的基底只在 clientVersion 里记着版本号，1.20.1 的 NeoForge 探测时
    /// 同时命中 Forge，都不能读错，否则重装的就是另一个游戏版本或另一个加载器。
    #[tokio::test]
    async fn the_loader_to_reinstall_is_read_from_the_version_json() {
        let tmp = tempfile::tempdir().unwrap();
        let petalfall = petalfall_at(tmp.path());
        let game = petalfall.game_dir().to_path_buf();
        write_version(
            &game,
            "我的原版",
            r#"{"id":"我的原版","type":"release","clientVersion":"1.20.1","mainClass":"M"}"#,
        );
        for (id, json, mc, loader, version) in [
            (
                "Forge",
                r#"{"id":"Forge","type":"release","clientVersion":"1.20.1","mainClass":"M",
                    "libraries":[{"name":"net.minecraftforge:forge:1.20.1-47.2.0:client"}]}"#,
                "1.20.1",
                LoaderChoice::Forge,
                "47.2.0",
            ),
            (
                "NeoForge",
                r#"{"id":"NeoForge","type":"release","clientVersion":"1.21.1","mainClass":"M",
                    "libraries":[{"name":"net.neoforged:neoforge:21.1.77-beta:universal"}]}"#,
                "1.21.1",
                LoaderChoice::NeoForge,
                "21.1.77-beta",
            ),
            (
                "NeoForge 旧版",
                r#"{"id":"NeoForge 旧版","type":"release","clientVersion":"1.20.1","mainClass":"M",
                    "arguments":{"game":["--fml.forgeVersion","47.1.106"],"jvm":[]},
                    "libraries":[{"name":"net.neoforged:forge:1.20.1-47.1.106:universal"}]}"#,
                "1.20.1",
                LoaderChoice::NeoForge,
                "47.1.106",
            ),
            (
                "Quilt",
                r#"{"id":"Quilt","type":"release","clientVersion":"1.20.1","mainClass":"M",
                    "libraries":[{"name":"org.quiltmc:quilt-loader:0.26.0-beta.1"}]}"#,
                "1.20.1",
                LoaderChoice::Quilt,
                "0.26.0-beta.1",
            ),
            (
                "传统布局",
                r#"{"id":"传统布局","inheritsFrom":"我的原版","type":"release","mainClass":"M",
                    "libraries":[{"name":"net.fabricmc:fabric-loader:0.15.11"}]}"#,
                "1.20.1",
                LoaderChoice::Fabric,
                "0.15.11",
            ),
            (
                "基底不在了",
                r#"{"id":"基底不在了","inheritsFrom":"1.19.4","type":"release","mainClass":"M",
                    "libraries":[{"name":"net.fabricmc:fabric-loader:0.15.11"}]}"#,
                "1.19.4",
                LoaderChoice::Fabric,
                "0.15.11",
            ),
        ] {
            write_version(&game, id, json);
            let (plan, _) = petalfall.begin_loader_reinstall(id).await.unwrap();
            assert_eq!(
                plan,
                RepairPlan::Reinstall(request(mc, Some(loader), Some(version))),
                "{id}"
            );
        }
    }

    /// 定不出能原样装回的内容时报不可补全，动手之前就停下，原有的标记不被覆盖：实例不会因为点了一下就多出
    /// 一份照着猜测内容写的安装标记。
    #[tokio::test]
    async fn loaders_that_cannot_be_put_back_as_they_were_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let petalfall = petalfall_at(tmp.path());
        let game = petalfall.game_dir().to_path_buf();
        for (id, json, reason) in [
            (
                "原版",
                r#"{"id":"原版","type":"release","clientVersion":"1.21.1","mainClass":"M"}"#,
                "没有装加载器",
            ),
            (
                "光影",
                r#"{"id":"光影","type":"release","clientVersion":"1.21.1","mainClass":"M",
                    "libraries":[{"name":"optifine:OptiFine:1.21.1_HD_U_J1"}]}"#,
                "OptiFine",
            ),
            (
                "叠层",
                r#"{"id":"叠层","type":"release","clientVersion":"1.20.1","mainClass":"M",
                    "libraries":[{"name":"net.minecraftforge:forge:1.20.1-47.2.0"},
                                 {"name":"optifine:OptiFine:1.20.1_HD_U_I6"}]}"#,
                "OptiFine",
            ),
            (
                "老旧",
                r#"{"id":"老旧","type":"release","clientVersion":"1.7.10","mainClass":"M",
                    "minecraftArguments":"--tweakClass com.mumfrey.liteloader.launch.LiteLoaderTweaker",
                    "libraries":[{"name":"com.mumfrey:liteloader:1.7.10"}]}"#,
                "LiteLoader",
            ),
            (
                "无版本号",
                r#"{"id":"无版本号","type":"release","clientVersion":"1.21.1",
                    "mainClass":"net.fabricmc.loader.impl.launch.knot.KnotClient"}"#,
                "读不出 Fabric 的版本号",
            ),
            (
                "不知版本",
                r#"{"id":"不知版本","type":"release","mainClass":"M",
                    "libraries":[{"name":"net.fabricmc:fabric-loader:0.15.11"}]}"#,
                "读不出游戏版本号",
            ),
            // 版本号是从目录名里抠出来的：猜错了就成了另一个游戏版本。
            (
                "魔改1.20",
                r#"{"id":"魔改1.20","type":"release","mainClass":"M",
                    "libraries":[{"name":"net.fabricmc:fabric-loader:0.15.11"}]}"#,
                "读不出游戏版本号",
            ),
        ] {
            let dir = write_version(&game, id, json);
            let store = MarkerStore::for_version_dir(&dir);
            let earlier = IncompleteMarker {
                error: Some(MarkerError {
                    code: "network_failed".into(),
                    message: "连不上".into(),
                }),
                outcome: IncompleteOutcome::Failed,
                ..IncompleteMarker::pending(IncompleteKind::Repair, 1_700_000_000_000)
            };
            store.save(&earlier).await.unwrap();

            match petalfall.begin_loader_reinstall(id).await.unwrap_err() {
                CoreError::Instance(InstanceError::NotRepairable {
                    id: got,
                    reason: why,
                }) => {
                    assert_eq!(got, id);
                    assert!(why.contains(reason), "{id}：{why}");
                }
                other => panic!("{id} 应报不可补全：{other:?}"),
            }
            assert_eq!(store.load().await.unwrap(), Some(earlier), "{id}");
        }
    }

    /// 版本号来自本地版本 JSON，同样会拼进安装器地址：不是单个安全路径段时在写下标记之前就拒掉。
    #[tokio::test]
    async fn an_unsafe_loader_version_in_the_version_json_is_refused_before_any_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let petalfall = petalfall_at(tmp.path());
        let dir = write_version(
            petalfall.game_dir(),
            "手改过",
            &fabric_instance_json("手改过", ".."),
        );
        let err = petalfall
            .begin_loader_reinstall("手改过")
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                CoreError::Install(petalfall_install::Error::UnsafePath { .. })
            ),
            "实际：{err:?}"
        );
        assert!(!marker_path(&dir).exists());
    }

    /// 安装没完成的实例照标记里记下的内容重装（标记里可能记着开局要套默认设置），不去读可能还没写出来的版本 JSON；
    /// 整合包没导入完的缺的是整合包内容，重装加载器补不回来。
    #[tokio::test]
    async fn loader_reinstall_defers_to_what_the_marker_recorded() {
        let tmp = tempfile::tempdir().unwrap();
        let petalfall = petalfall_at(tmp.path());
        let wanted = InstanceRequest {
            use_template: true,
            ..request("1.21.1", Some(LoaderChoice::Fabric), Some("0.16.10"))
        };
        petalfall
            .create_instance_dir("半成品", &wanted)
            .await
            .unwrap();
        let (plan, _) = petalfall.begin_loader_reinstall("半成品").await.unwrap();
        assert_eq!(plan, RepairPlan::Reinstall(wanted));

        let dir = write_version(
            petalfall.game_dir(),
            "整合包",
            &fabric_instance_json("整合包", "0.16.10"),
        );
        let store = MarkerStore::for_version_dir(&dir);
        let marker = IncompleteMarker::pending(
            IncompleteKind::Modpack {
                task_id: "mp#1".into(),
            },
            1_700_000_000_000,
        );
        store.save(&marker).await.unwrap();
        match petalfall
            .begin_loader_reinstall("整合包")
            .await
            .unwrap_err()
        {
            CoreError::Instance(InstanceError::NotRepairable { reason, .. }) => {
                assert!(reason.contains("重新导入"), "{reason}");
            }
            other => panic!("应报不可补全：{other:?}"),
        }
        assert_eq!(store.load().await.unwrap(), Some(marker));
    }

    /// 上次补全败在加载器自带的库上时，普通补全直接换成重装：「修复并启动」「重试」「全部修复」都走这里，
    /// 否则每一个都原样失败，玩家在实例库里点不出去。别的失败照旧按版本 JSON 补全。
    #[tokio::test]
    async fn repair_after_a_lost_loader_library_reinstalls_the_loader() {
        let tmp = tempfile::tempdir().unwrap();
        let petalfall = petalfall_at(tmp.path());
        let dir = write_version(
            petalfall.game_dir(),
            "周末生存",
            &fabric_instance_json("周末生存", "0.16.10"),
        );
        let store = MarkerStore::for_version_dir(&dir);
        let failed_with = |code: &str| IncompleteMarker {
            error: Some(MarkerError {
                code: code.to_owned(),
                message: "原文".into(),
            }),
            outcome: IncompleteOutcome::Failed,
            ..IncompleteMarker::pending(IncompleteKind::Repair, 1_700_000_000_000)
        };

        store.save(&failed_with("network_failed")).await.unwrap();
        let (plan, _) = petalfall.begin_repair("周末生存").await.unwrap();
        assert_eq!(plan, RepairPlan::Complete);

        store
            .save(&failed_with("loader_library_missing"))
            .await
            .unwrap();
        let (plan, _) = petalfall.begin_repair("周末生存").await.unwrap();
        assert_eq!(
            plan,
            RepairPlan::Reinstall(request(
                "1.21.1",
                Some(LoaderChoice::Fabric),
                Some("0.16.10")
            ))
        );

        // 装不回的加载器照旧补全：玩家把文件放回去之后，这是清掉标记的唯一办法。
        let optifine = write_version(
            petalfall.game_dir(),
            "光影",
            r#"{"id":"光影","type":"release","clientVersion":"1.21.1","mainClass":"M",
                "libraries":[{"name":"optifine:OptiFine:1.21.1_HD_U_J1"}]}"#,
        );
        MarkerStore::for_version_dir(&optifine)
            .save(&failed_with("loader_library_missing"))
            .await
            .unwrap();
        let (plan, _) = petalfall.begin_repair("光影").await.unwrap();
        assert_eq!(plan, RepairPlan::Complete);
    }

    /// 重装 Forge 途中被打断，盘上是只合并了原版的那份版本 JSON，未完成标记又读不出来（写坏了，或是这一版不认识的
    /// 格式）：安装记录里记着 Forge、版本 JSON 里一个加载器都探不出，照安装记录重装，不照原版那份补全把实例补成原版。
    /// 标记读不出来时版本 JSON 里探得出加载器（记录里那一种、玩家换上的 Fabric、只装着的 OptiFine）、没有记录、记录读不出
    /// 或记录里的加载器认不出来，都照旧补全；没有标记时不看安装记录，记录与版本 JSON 对不上也照旧补全。
    #[tokio::test]
    async fn an_unreadable_marker_reinstalls_from_the_record_only_when_the_json_has_no_loader() {
        let tmp = tempfile::tempdir().unwrap();
        let petalfall = petalfall_at(tmp.path());
        let game = petalfall.game_dir().to_path_buf();
        let forge_version = format!("47.2.{}", fastrand::u16(..));
        let recorded = request(
            "1.20.1",
            Some(LoaderChoice::Forge),
            Some(forge_version.as_str()),
        );
        let vanilla_only = |id: &str| {
            format!(r#"{{"id":"{id}","type":"release","clientVersion":"1.20.1","mainClass":"M"}}"#)
        };
        let with_forge = |id: &str| {
            format!(
                r#"{{"id":"{id}","type":"release","clientVersion":"1.20.1","mainClass":"M",
                    "libraries":[{{"name":"net.minecraftforge:forge:1.20.1-{forge_version}:client"}}]}}"#
            )
        };
        let put_marker = |dir: &Path, bytes: &[u8]| {
            let path = marker_path(dir);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        };
        let unreadable: [&[u8]; 2] = [
            b"{ not json",
            br#"{"kind":"migrate","outcome":"pending","started_at":1700000000000,"error":null}"#,
        ];

        let dir = write_version(&game, "重装到一半", &vanilla_only("重装到一半"));
        InstallRecordStore::for_version_dir(&dir)
            .save(&recorded.spec())
            .await
            .unwrap();
        for bytes in unreadable {
            put_marker(&dir, bytes);
            let (plan, marker) = petalfall.begin_repair("重装到一半").await.unwrap();
            assert_eq!(plan, RepairPlan::Reinstall(recorded.clone()));
            assert_eq!(
                marker.kind,
                IncompleteKind::Install {
                    spec: recorded.spec()
                }
            );
            assert_eq!(
                MarkerStore::for_version_dir(&dir).load().await.unwrap(),
                Some(marker)
            );
        }

        let forge_kept = write_version(&game, "Forge 还在", &with_forge("Forge 还在"));
        put_marker(&forge_kept, unreadable[0]);
        InstallRecordStore::for_version_dir(&forge_kept)
            .save(&recorded.spec())
            .await
            .unwrap();
        let fabric_swapped = write_version(
            &game,
            "换成了 Fabric",
            &fabric_instance_json("换成了 Fabric", "0.16.10"),
        );
        put_marker(&fabric_swapped, unreadable[0]);
        InstallRecordStore::for_version_dir(&fabric_swapped)
            .save(&recorded.spec())
            .await
            .unwrap();
        // 只装着 OptiFine 也算探出了加载器：被打断的现场探不出它，照记录重装会把玩家装的它盖掉。
        let optifine_only = write_version(
            &game,
            "只装着 OptiFine",
            r#"{"id":"只装着 OptiFine","type":"release","clientVersion":"1.20.1","mainClass":"M",
                "libraries":[{"name":"optifine:OptiFine:1.20.1_HD_U_I6"}]}"#,
        );
        put_marker(&optifine_only, unreadable[0]);
        InstallRecordStore::for_version_dir(&optifine_only)
            .save(&recorded.spec())
            .await
            .unwrap();
        let no_record = write_version(&game, "没有记录", &vanilla_only("没有记录"));
        put_marker(&no_record, unreadable[0]);
        let bad_record = write_version(&game, "记录写坏了", &vanilla_only("记录写坏了"));
        put_marker(&bad_record, unreadable[0]);
        std::fs::write(
            bad_record
                .join(PETALFALL_META_DIR)
                .join("install-record.json"),
            b"{ not json",
        )
        .unwrap();
        assert!(
            InstallRecordStore::for_version_dir(&bad_record)
                .load()
                .await
                .is_err(),
            "夹具里的安装记录要读不出来"
        );
        let unknown = write_version(&game, "记录认不出", &vanilla_only("记录认不出"));
        put_marker(&unknown, unreadable[0]);
        InstallRecordStore::for_version_dir(&unknown)
            .save(&InstallSpec {
                loader: Some("rift".into()),
                ..recorded.spec()
            })
            .await
            .unwrap();
        let no_marker = write_version(&game, "没有标记", &vanilla_only("没有标记"));
        InstallRecordStore::for_version_dir(&no_marker)
            .save(&recorded.spec())
            .await
            .unwrap();

        for (id, dir) in [
            ("Forge 还在", forge_kept),
            ("换成了 Fabric", fabric_swapped),
            ("只装着 OptiFine", optifine_only),
            ("没有记录", no_record),
            ("记录写坏了", bad_record),
            ("记录认不出", unknown),
            ("没有标记", no_marker),
        ] {
            let (plan, marker) = petalfall.begin_repair(id).await.unwrap();
            assert_eq!(plan, RepairPlan::Complete, "{id}");
            assert_eq!(marker.kind, IncompleteKind::Repair, "{id}");
            assert_eq!(
                MarkerStore::for_version_dir(&dir).load().await.unwrap(),
                Some(marker),
                "{id}"
            );
        }
    }

    /// 安装器在本机生成的文件（这里是 NeoForge 补丁后的客户端 jar）补全下不回来：缺了它补全就照加载器文件缺失
    /// 停下，不报成功，也不进任何下载步骤，连测速探针都不发；文件放回去后同一个方案照常补全，这时才测速。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn repair_stops_when_a_processor_output_of_the_loader_is_missing() {
        use crate::sources::tests::probe_count;
        // 测速的两个源钉到两台本地 mock 上，探针发没发出去看得见。
        let official = MockServer::start().await;
        let mirror = MockServer::start().await;
        let tmp = tempfile::tempdir().unwrap();
        let petalfall = petalfall_at(tmp.path()).with_source_routes(
            std::sync::Arc::new(crate::sources::tests::Routes {
                official: official.uri(),
                mirror: mirror.uri(),
            }),
            std::time::Duration::from_secs(600),
            std::time::Duration::from_secs(30),
        );
        write_version(
            petalfall.game_dir(),
            "整合包",
            r#"{"id":"整合包","type":"release","mainClass":"cpw.mods.bootstraplauncher.BootstrapLauncher",
                "arguments":{"jvm":[],"game":["--fml.neoForgeVersion","21.1.250","--fml.fmlVersion","4.0.44",
                "--fml.mcVersion","1.21.1","--fml.neoFormVersion","20240808.144430","--launchTarget","forgeclient"]}}"#,
        );
        let layout = petalfall.layout();
        let patched =
            layout.library_path("net/neoforged/neoforge/21.1.250/neoforge-21.1.250-client.jar");
        let srg = layout.library_path(
            "net/minecraft/client/1.21.1-20240808.144430/client-1.21.1-20240808.144430-srg.jar",
        );
        std::fs::create_dir_all(srg.parent().unwrap()).unwrap();
        std::fs::write(&srg, b"srg").unwrap();
        let (plan, _) = petalfall.begin_repair("整合包").await.unwrap();
        assert_eq!(plan, RepairPlan::Complete);

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let err = petalfall
            .repair_instance("整合包", &plan, Some(&tx))
            .await
            .unwrap_err();
        drop(tx);
        assert!(
            matches!(
                &err,
                CoreError::Install(petalfall_install::Error::LoaderLibraryMissing { path })
                    if *path == patched
            ),
            "{err:?}"
        );
        assert!(phases_of(&drain(rx)).is_empty(), "不该进到下载步骤");
        for server in [&official, &mirror] {
            assert_eq!(
                server.received_requests().await.unwrap().len(),
                0,
                "核对没过时一个请求都不发"
            );
        }

        std::fs::create_dir_all(patched.parent().unwrap()).unwrap();
        std::fs::write(&patched, b"patched").unwrap();
        let outcome = petalfall
            .repair_instance("整合包", &plan, None)
            .await
            .unwrap();
        assert!(!outcome.reinstalled);
        // 核对过了才测速：两个源各收到一个探针，上面的 0 不是因为测速根本没接到这两台 mock 上。
        assert_eq!(
            (probe_count(&official).await, probe_count(&mirror).await),
            (1, 1)
        );
    }

    #[tokio::test]
    async fn abandon_removes_the_directory_and_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let petalfall = petalfall_at(tmp.path());
        let dir = write_version(petalfall.game_dir(), "临时", &vanilla_json("1.21.1"));
        petalfall.abandon_instance("临时").await.unwrap();
        assert!(!dir.exists());
        petalfall.abandon_instance("临时").await.unwrap();
    }

    #[test]
    fn unknown_loader_in_a_marker_is_not_repairable() {
        let spec = InstallSpec {
            mc_version: "1.21.1".into(),
            loader: Some("rift".into()),
            loader_version: None,
            use_template: false,
        };
        let err = InstanceRequest::from_spec("x", &spec).unwrap_err();
        assert!(
            matches!(
                &err,
                CoreError::Instance(InstanceError::NotRepairable { id, reason })
                    if id == "x" && reason == "未完成标记里记的加载器 rift 不认识"
            ),
            "{err:?}"
        );
    }

    /// 加载器认不出来时原因照 spec 读自哪份文件说：未完成标记里的说未完成标记，安装记录里的说安装记录，种类都是
    /// 照它装不回去的 no_record。
    #[test]
    fn an_unknown_loader_is_blamed_on_the_file_it_was_read_from() {
        let key = format!("未来的加载器-{}", fastrand::u32(..));
        let spec = InstallSpec {
            mc_version: "1.21.1".into(),
            loader: Some(key.clone()),
            loader_version: Some("1.0".into()),
            use_template: fastrand::bool(),
        };
        assert_eq!(
            recorded_request(&spec, SpecSource::Marker),
            Err(no_record(&format!("未完成标记里记的加载器 {key} 不认识")))
        );
        assert_eq!(
            recorded_request(&spec, SpecSource::Record),
            Err(no_record(&format!("安装记录里记的加载器 {key} 不认识")))
        );
    }

    /// 加载器版本号来自远端版本列表，会拼进安装器地址、缓存文件名与版本 id：不是单个安全路径段时在动手之前
    /// 报路径越界，不建目录、不发请求；正常的版本号（含预发布后缀）照常放行。
    #[test]
    fn loader_versions_that_are_not_a_single_path_segment_fail_validation() {
        for (choice, version, problem) in [
            (
                LoaderChoice::Forge,
                r"47.2.0\..\..\x",
                petalfall_version::UnsafePath::Backslash,
            ),
            (
                LoaderChoice::NeoForge,
                "21.1.42/../x",
                petalfall_version::UnsafePath::Separator,
            ),
            (
                LoaderChoice::Fabric,
                "..",
                petalfall_version::UnsafePath::DotSegment,
            ),
        ] {
            let request = InstanceRequest {
                mc_version: "1.20.1".into(),
                loader: Some(choice),
                loader_version: Some(version.into()),
                use_template: false,
            };
            match request.validate() {
                Err(CoreError::Install(petalfall_install::Error::UnsafePath {
                    entry,
                    path,
                    problem: got,
                })) => {
                    assert_eq!(entry, format!("{} 版本号", choice.display_name()));
                    assert_eq!(path, version);
                    assert_eq!(got, problem);
                }
                other => panic!("{version} 应报路径越界，实际：{other:?}"),
            }
        }
        for (choice, version) in [
            (LoaderChoice::Forge, "47.2.0"),
            (LoaderChoice::Quilt, "0.26.0-beta.1"),
        ] {
            InstanceRequest {
                mc_version: "1.20.1".into(),
                loader: Some(choice),
                loader_version: Some(version.into()),
                use_template: false,
            }
            .validate()
            .unwrap();
        }
    }

    // ---- 游戏目录的数据格式记号 ----

    use crate::data_format::STAMP_FILE;
    use crate::data_format::testing::{GuardFixture, with_fixture};
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    /// 测试里当作这一版的数据格式号：号为 1 时一个记号都不写，看不出写没写。
    const OWN: u32 = 2;
    /// 比 [`OWN`] 新的记号。
    const NEWER_STAMP: &str = r#"{"data_format":3,"written_by":"1.1.0-dev.57"}"#;

    fn own() -> GuardFixture {
        GuardFixture {
            own: OWN,
            look: None,
        }
    }

    fn stamp_file(game_dir: &Path) -> PathBuf {
        game_dir.join(PETALFALL_META_DIR).join(STAMP_FILE)
    }

    fn put(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
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

    /// 当前游戏目录被更新的落樱用过、或记号读不懂：新建在建任何东西之前就报错，数据目录与游戏目录一个文件都不多。
    #[tokio::test]
    async fn creating_in_a_refused_game_dir_writes_nothing() {
        let unreadable = r#"{"data_format":"3"}"#;
        for stamp in [NEWER_STAMP, unreadable] {
            let tmp = tempfile::tempdir().unwrap();
            let petalfall = petalfall_at(tmp.path());
            write_version(petalfall.game_dir(), "旧实例", &vanilla_json("1.21.1"));
            put(&stamp_file(petalfall.game_dir()), stamp);
            let before = tree(tmp.path());

            let err = with_fixture(
                own(),
                petalfall.create_instance_dir("新实例", &request("1.21.1", None, None)),
            )
            .await
            .unwrap_err();

            match err {
                CoreError::GameDirDataTooNew {
                    path,
                    found,
                    supported,
                    written_by,
                } if stamp == NEWER_STAMP => {
                    assert_eq!(path, petalfall.game_dir());
                    assert_eq!((found, supported), (3, OWN));
                    assert_eq!(written_by.as_deref(), Some("1.1.0-dev.57"));
                }
                CoreError::DataFormatUnreadable { path, .. } if stamp == unreadable => {
                    assert_eq!(path, petalfall.game_dir());
                }
                other => panic!("记号 {stamp} 应拒绝新建，实际是 {other:?}"),
            }
            assert_eq!(tree(tmp.path()), before, "记号 {stamp}");
        }
    }

    /// 实例目录建好之后、安装开工之前，游戏目录被更新的落樱用过：安装报错，一个请求都不发，一个文件都不写。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn installing_into_a_newer_game_dir_downloads_and_writes_nothing() {
        let server = MockServer::start().await;
        mock_meta(&server).await;
        let tmp = tempfile::tempdir().unwrap();
        let petalfall = petalfall_at(tmp.path())
            .with_manifest_url(format!("{}/manifest.json", server.uri()))
            .with_fabric_base(server.uri());
        let req = request("1.21.1", Some(LoaderChoice::Fabric), Some("0.16.10"));
        petalfall
            .create_instance_dir("周末生存", &req)
            .await
            .unwrap();
        put(&stamp_file(petalfall.game_dir()), NEWER_STAMP);
        let before = tree(tmp.path());

        let err = with_fixture(own(), petalfall.install_instance("周末生存", &req, None))
            .await
            .unwrap_err();

        match err {
            CoreError::GameDirDataTooNew {
                path,
                found,
                supported,
                ..
            } => {
                assert_eq!(path, petalfall.game_dir());
                assert_eq!((found, supported), (3, OWN));
            }
            other => panic!("应拒绝安装，实际是 {other:?}"),
        }
        assert_eq!(server.received_requests().await.unwrap().len(), 0);
        assert_eq!(tree(tmp.path()), before);
    }

    /// 游戏目录原本不存在：新建把它建出来，在安装开工之前就补上记号，成功后记号是这一版的号。记号已经等于这一版
    /// 时再新建，记号原样不动。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_game_dir_created_by_a_new_instance_gets_the_stamp() {
        let server = MockServer::start().await;
        mock_meta(&server).await;
        let tmp = tempfile::tempdir().unwrap();
        let petalfall = petalfall_at(tmp.path())
            .with_manifest_url(format!("{}/manifest.json", server.uri()))
            .with_fabric_base(server.uri());
        let game_dir = petalfall.game_dir().to_owned();
        assert!(!game_dir.exists());
        let req = request("1.21.1", Some(LoaderChoice::Fabric), Some("0.16.10"));

        with_fixture(own(), async {
            petalfall
                .create_instance_dir("周末生存", &req)
                .await
                .unwrap();
            assert_eq!(
                std::fs::read_to_string(stamp_file(&game_dir)).unwrap(),
                r#"{"data_format":2}"#,
                "建出游戏目录时就该补上记号"
            );
            petalfall
                .install_instance("周末生存", &req, None)
                .await
                .unwrap();
        })
        .await;

        assert_eq!(
            std::fs::read_to_string(stamp_file(&game_dir)).unwrap(),
            r#"{"data_format":2}"#
        );
        assert!(
            game_dir
                .join("versions")
                .join("周末生存")
                .join("周末生存.json")
                .is_file()
        );

        let equal = r#"{"data_format":2,"written_by":"1.0.0"}"#;
        put(&stamp_file(&game_dir), equal);
        with_fixture(
            own(),
            petalfall.create_instance_dir("第二个", &request("1.21.1", None, None)),
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(stamp_file(&game_dir)).unwrap(),
            equal
        );
    }

    use crate::data_format::DirLook;
    use crate::data_format::testing::LookFn;
    use crate::error::GameDirProblem;
    use petalfall_release::data_format::{StampFile, parse_stamp};
    use std::sync::{Arc, Mutex};

    /// 游戏目录原本不存在：开头的核对看到目录不存在，建好 `versions/` 之后的第二次核对被拒，一种是别处刚写下了更高的
    /// 号，一种是这一刻够不着这个位置（网络盘断开）。新建报第二次核对的错误，实例目录没建、没有未完成标记、没写记号：
    /// 盘上只多出空的 `versions/`。
    #[tokio::test]
    async fn a_refused_check_after_creating_the_game_dir_creates_no_instance() {
        for case in ["更高的号", "够不着"] {
            let tmp = tempfile::tempdir().unwrap();
            let petalfall = petalfall_at(tmp.path());
            let game_dir = petalfall.game_dir().to_owned();
            let instance = game_dir.join("versions").join("新实例");
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
                    own: OWN,
                    look: Some(look),
                },
                petalfall.create_instance_dir("新实例", &request("1.21.1", None, None)),
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
                    assert_eq!((found, supported), (3, OWN));
                    assert_eq!(written_by.as_deref(), Some("1.1.0-dev.57"));
                }
                ("够不着", CoreError::InvalidGameDirectory { path, problem }) => {
                    assert_eq!(path, game_dir);
                    assert_eq!(problem, GameDirProblem::Unavailable);
                }
                (case, other) => panic!("{case}：应拒绝新建，实际是 {other:?}"),
            }
            assert_eq!(
                *seen.lock().unwrap(),
                [false],
                "{case}：建好目录之后只核对一次，那时实例目录还不在"
            );
            assert_eq!(
                tree(tmp.path()),
                BTreeMap::from([
                    (PathBuf::from(".minecraft"), None),
                    (PathBuf::from(".minecraft").join("versions"), None),
                ]),
                "{case}"
            );
        }
    }

    use crate::instances::tests::disk_state;

    /// 补全、定方案、记结局、丢弃这几个入口在表里的名字；补全按两种方案各调一次。
    const WRITERS: [&str; 6] = [
        "abandon_instance",
        "settle_instance_marker",
        "begin_repair",
        "begin_loader_reinstall",
        "repair_instance 补齐",
        "repair_instance 重装",
    ];

    /// 等着补全的实例「待补全」：版本 JSON 只有一个从模拟端点下载的库，未完成标记记着当初的安装内容。各入口真走
    /// 下去都会改盘或发请求：删目录、清标记、写新标记、下载库与版本清单。返回写下的标记。
    async fn repairable_instance(petalfall: &Petalfall, base: &str) -> IncompleteMarker {
        let dir = write_version(
            petalfall.game_dir(),
            "待补全",
            &format!(
                r#"{{"id":"待补全","type":"release","mainClass":"m",
                    "libraries":[{{"name":"net.example:loader:0.16.10","url":"{base}/"}}]}}"#
            ),
        );
        let marker = IncompleteMarker::pending(
            IncompleteKind::Install {
                spec: request("1.21.1", None, None).spec(),
            },
            1_700_000_000_000,
        );
        MarkerStore::for_version_dir(&dir)
            .save(&marker)
            .await
            .unwrap();
        marker
    }

    async fn call_writer(
        petalfall: &Petalfall,
        entry: &str,
        marker: &IncompleteMarker,
    ) -> Result<()> {
        let id = "待补全";
        match entry {
            "abandon_instance" => petalfall.abandon_instance(id).await,
            "settle_instance_marker" => {
                petalfall
                    .settle_instance_marker(id, marker.clone(), MarkerSettlement::Succeeded)
                    .await
            }
            "begin_repair" => petalfall.begin_repair(id).await.map(drop),
            "begin_loader_reinstall" => petalfall.begin_loader_reinstall(id).await.map(drop),
            "repair_instance 补齐" => petalfall
                .repair_instance(id, &RepairPlan::Complete, None)
                .await
                .map(drop),
            "repair_instance 重装" => petalfall
                .repair_instance(
                    id,
                    &RepairPlan::Reinstall(request("1.21.1", None, None)),
                    None,
                )
                .await
                .map(drop),
            other => panic!("表里没有这个入口：{other}"),
        }
    }

    fn repair_petalfall(root: &Path, server: &MockServer) -> Petalfall {
        petalfall_at(root)
            .with_manifest_url(format!("{}/manifest.json", server.uri()))
            .with_fabric_base(server.uri())
    }

    /// 任务中心重试补全、旧列表上点补全时，游戏目录已被更新的落樱用过：每个入口都报游戏目录太新，一个请求都不发，
    /// 数据目录与游戏目录里的文件内容、修改时间都不变，没有多出文件。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn repair_entries_refuse_a_newer_game_dir_and_touch_nothing() {
        for entry in WRITERS {
            let server = MockServer::start().await;
            mock_meta(&server).await;
            let tmp = tempfile::tempdir().unwrap();
            let petalfall = repair_petalfall(tmp.path(), &server);
            let marker = repairable_instance(&petalfall, &server.uri()).await;
            put(&stamp_file(petalfall.game_dir()), NEWER_STAMP);
            let before = disk_state(tmp.path());

            let err = with_fixture(own(), call_writer(&petalfall, entry, &marker))
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
                    assert_eq!((found, supported), (3, OWN), "{entry}");
                    assert_eq!(written_by.as_deref(), Some("1.1.0-dev.57"), "{entry}");
                }
                other => panic!("{entry} 应报游戏目录太新，实际是 {other:?}"),
            }
            assert_eq!(disk_state(tmp.path()), before, "{entry}");
            assert_eq!(
                server.received_requests().await.unwrap().len(),
                0,
                "{entry}"
            );
        }
    }

    /// 不换本版号：记号读不懂与号是多少无关，生产的这条路同样在碰任何文件、发任何请求之前拦下。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn repair_entries_refuse_an_unreadable_stamp_and_touch_nothing() {
        for entry in ["begin_repair", "repair_instance 补齐"] {
            let server = MockServer::start().await;
            mock_meta(&server).await;
            let tmp = tempfile::tempdir().unwrap();
            let petalfall = repair_petalfall(tmp.path(), &server);
            let marker = repairable_instance(&petalfall, &server.uri()).await;
            put(&stamp_file(petalfall.game_dir()), r#"{"data_format":"3"}"#);
            let before = disk_state(tmp.path());

            let err = call_writer(&petalfall, entry, &marker).await.unwrap_err();

            assert!(
                matches!(&err, CoreError::DataFormatUnreadable { path, .. } if path == petalfall.game_dir()),
                "{entry}：{err:?}"
            );
            assert_eq!(disk_state(tmp.path()), before, "{entry}");
            assert_eq!(
                server.received_requests().await.unwrap().len(),
                0,
                "{entry}"
            );
        }
    }

    /// 装 Forge 系加载器时处理器开跑前换上原版版本 JSON 的接线：假的 Forge 安装器加假 java 批处理，走真正的
    /// create_instance_dir 与 install_instance。
    ///
    /// 假处理器只有一个参数 `{MINECRAFT_JAR}`，按它找客户端 jar 旁边的版本 JSON（官方启动器布局里那个位置的原版
    /// 版本 JSON），找不到就非零退出；找到了照抄一份到 java 旁边的 seen.json，用例据此看它读到的是哪一份。java 旁边
    /// 放着 fail.flag 时抄完再非零退出，当作处理器失败。批处理里只写相对脚本所在目录的路径（%~dp0、%~dpn4），临时
    /// 目录在中文用户名下时脚本文字不会读成乱码。
    #[cfg(windows)]
    mod processor_json {
        use super::*;
        use crate::config::PetalfallConfig;
        use crate::java::JavaEnv;
        use std::io::Write;
        use std::path::PathBuf;

        /// 这个版本的安装器内嵌的版本 JSON 继承原版，合并得成。
        const MERGEABLE: &str = "1.0";
        /// 这个版本的安装器内嵌的版本 JSON 继承的是别的游戏版本：处理器跑完之后合并失败。
        const UNMERGEABLE: &str = "2.0";
        const FORGE_MAIN: &str = "net.minecraftforge.bootstrap.ForgeBootstrap";

        struct Fixture {
            _tmp: tempfile::TempDir,
            _server: MockServer,
            petalfall: Petalfall,
            bin: PathBuf,
            vanilla: VersionJson,
        }

        impl Fixture {
            fn request(&self, forge: &str) -> InstanceRequest {
                request("test", Some(LoaderChoice::Forge), Some(forge))
            }

            fn json_path(&self, id: &str) -> PathBuf {
                self.petalfall
                    .game_dir()
                    .join("versions")
                    .join(id)
                    .join(format!("{id}.json"))
            }

            /// 实例 `id` 装加载器之前、只合并了原版的那份。
            fn base(&self, id: &str) -> VersionJson {
                merge_for_instance(id, &self.vanilla, None).unwrap()
            }

            fn fail_processors(&self, fail: bool) {
                let flag = self.bin.join("fail.flag");
                if fail {
                    std::fs::write(flag, b"").unwrap();
                } else if flag.exists() {
                    std::fs::remove_file(flag).unwrap();
                }
            }

            /// 跑一次安装，交回结果与处理器这一次读到的版本 JSON（没跑到处理器或读不到时为 `None`）。
            async fn install(
                &self,
                id: &str,
                forge: &str,
            ) -> (Result<InstanceInstallOutcome>, Option<VersionJson>) {
                let seen = self.bin.join("seen.json");
                if seen.exists() {
                    std::fs::remove_file(&seen).unwrap();
                }
                let outcome = self
                    .petalfall
                    .install_instance(id, &self.request(forge), None)
                    .await;
                let read = seen.exists().then(|| {
                    VersionJson::from_json_str(&std::fs::read_to_string(&seen).unwrap()).unwrap()
                });
                (outcome, read)
            }
        }

        async fn fixture() -> Fixture {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path().to_path_buf();
            let game = root.join("game");
            let stored = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);

            // 处理器 jar 只要 MANIFEST.MF 里有 Main-Class，真正执行的是假 java。
            let processor = game.join(r"libraries\test\proc\1\proc-1.jar");
            std::fs::create_dir_all(processor.parent().unwrap()).unwrap();
            let mut jar = zip::ZipWriter::new(std::fs::File::create(&processor).unwrap());
            jar.start_file("META-INF/MANIFEST.MF", stored).unwrap();
            jar.write_all(b"Manifest-Version: 1.0\r\nMain-Class: test.Processor\r\n\r\n")
                .unwrap();
            jar.finish().unwrap();

            let home = root.join("jdk-17");
            for file in [home.join("release"), home.join(r"lib\modules")] {
                std::fs::create_dir_all(file.parent().unwrap()).unwrap();
                std::fs::write(file, b"stub").unwrap();
            }
            let bin = home.join("bin");
            std::fs::create_dir_all(&bin).unwrap();
            let java = bin.join("java-17.cmd");
            // 参数依次是 -cp、classpath、Main-Class、处理器自己的参数，所以客户端 jar 是 %4。
            std::fs::write(
                &java,
                "@echo off\r\n\
                 if not exist \"%~dpn4.json\" exit /b 2\r\n\
                 copy /y \"%~dpn4.json\" \"%~dp0seen.json\" >nul\r\n\
                 if exist \"%~dp0fail.flag\" exit /b 1\r\n\
                 exit /b 0\r\n",
            )
            .unwrap();

            let libs = [
                format!("com.mojang:realms:1.{}", fastrand::u16(..)),
                format!("org.lwjgl:lwjgl:3.{}", fastrand::u16(..)),
            ];
            let vanilla = serde_json::json!({
                "id": "test", "type": "release", "mainClass": "net.minecraft.client.main.Main",
                "javaVersion": { "component": "java-runtime-gamma", "majorVersion": 17 },
                "libraries": libs.iter().map(|name| serde_json::json!({
                    "name": name, "url": "https://libraries.minecraft.net/"
                })).collect::<Vec<_>>(),
            })
            .to_string();

            // 官方地址用解析不了的域名，经源计划落到 mock：接线断了也连不到真实网络。
            let server = MockServer::start().await;
            let base = "https://meta.invalid";
            let mut routes = vec![
                (
                    "/manifest.json".to_owned(),
                    format!(
                        r#"{{"latest":{{"release":"test","snapshot":"test"}},
                            "versions":[{{"id":"test","type":"release","url":"{base}/test.json",
                            "time":"t","releaseTime":"t"}}]}}"#
                    )
                    .into_bytes(),
                ),
                ("/test.json".to_owned(), vanilla.clone().into_bytes()),
            ];
            for name in &libs {
                let parts: Vec<&str> = name.split(':').collect();
                let [group, artifact, version] = parts[..] else {
                    unreachable!("坐标是三段")
                };
                routes.push((
                    format!(
                        "/{}/{artifact}/{version}/{artifact}-{version}.jar",
                        group.replace('.', "/")
                    ),
                    format!("{name} 的字节").into_bytes(),
                ));
            }
            for (forge, inherits) in [(MERGEABLE, "test"), (UNMERGEABLE, "其他版本")] {
                let profile = serde_json::json!({
                    "version": format!("test-forge-{forge}"),
                    "minecraft": "test",
                    "json": "/version.json",
                    "processors": [{ "jar": "test:proc:1", "args": ["{MINECRAFT_JAR}"] }],
                });
                let embedded = serde_json::json!({
                    "id": format!("test-forge-{forge}"), "inheritsFrom": inherits,
                    "mainClass": FORGE_MAIN, "libraries": []
                });
                let mut installer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
                for (name, body) in [
                    ("install_profile.json", profile),
                    ("version.json", embedded),
                ] {
                    installer.start_file(name, stored).unwrap();
                    installer.write_all(body.to_string().as_bytes()).unwrap();
                }
                routes.push((
                    format!(
                        "/net/minecraftforge/forge/test-{forge}/forge-test-{forge}-installer.jar"
                    ),
                    installer.finish().unwrap().into_inner(),
                ));
            }
            for (route, body) in routes {
                Mock::given(method("GET"))
                    .and(path(route))
                    .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
                    .mount(&server)
                    .await;
            }

            let config = PetalfallConfig {
                default_java: Some(java.clone()),
                extra_java_paths: vec![java],
                auto_download_java: false,
                ..PetalfallConfig::default()
            };
            let petalfall = Petalfall::for_test(config, root.clone(), game)
                .with_manifest_url(format!("{base}/manifest.json"))
                .with_source_routes(
                    std::sync::Arc::new(crate::sources::tests::Routes {
                        official: server.uri(),
                        mirror: server.uri(),
                    }),
                    std::time::Duration::from_secs(600),
                    std::time::Duration::from_secs(2),
                );
            // 识别结果预先记进识别缓存：安装取 Java 时命中缓存，不拉起假 java，也不扫测试机上真装着的 Java。
            let seeded = petalfall
                .detect_java_with(&JavaEnv::for_test(
                    |exe: &Path| {
                        let home = exe.parent().and_then(Path::parent).and_then(Path::file_name);
                        assert_eq!(home, Some(std::ffi::OsStr::new("jdk-17")), "夹具里没有 {}", exe.display());
                        Ok("openjdk version \"17.0.8\" 2023-07-18\nOpenJDK 64-Bit Server VM (build 17.0.8+7-LTS, mixed mode)\n".to_owned())
                    },
                    String::new(),
                ))
                .await
                .unwrap();
            assert_eq!(
                seeded
                    .iter()
                    .map(|java| (java.version.major, java.is_complete()))
                    .collect::<Vec<_>>(),
                vec![(17, true)]
            );

            Fixture {
                _tmp: tmp,
                _server: server,
                petalfall,
                bin,
                vanilla: VersionJson::from_json_str(&vanilla).unwrap(),
            }
        }

        fn library_names(json: &VersionJson) -> Vec<&str> {
            json.libraries.iter().map(|lib| lib.name.as_str()).collect()
        }

        /// 新建：处理器从客户端 jar 旁边读到的是只合并了原版的那份（原版的库清单），装成后那里是合并结果。
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn a_new_forge_instance_hands_processors_the_vanilla_json_and_keeps_the_merged_one() {
            let f = fixture().await;
            let id = "旧版铁砧";
            f.petalfall
                .create_instance_dir(id, &f.request(MERGEABLE))
                .await
                .unwrap();

            let (outcome, seen) = f.install(id, MERGEABLE).await;

            outcome.unwrap();
            let seen = seen.expect("处理器应读到客户端 jar 旁边的版本 JSON");
            assert_eq!(seen, f.base(id));
            assert_eq!(library_names(&seen), library_names(&f.vanilla));
            let installed =
                VersionJson::from_json_str(&std::fs::read_to_string(f.json_path(id)).unwrap())
                    .unwrap();
            assert_eq!(installed.id, id);
            assert_eq!(installed.main_class.as_deref(), Some(FORGE_MAIN));
            assert_eq!(installed.inherits_from, None);
        }

        /// 新建没装成（处理器失败，或处理器跑完了合并不成）：实例目录里不留那份原版的版本 JSON。
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn a_failed_new_forge_instance_leaves_no_version_json() {
            let f = fixture().await;

            let id = "处理器失败";
            f.petalfall
                .create_instance_dir(id, &f.request(MERGEABLE))
                .await
                .unwrap();
            f.fail_processors(true);
            let (outcome, seen) = f.install(id, MERGEABLE).await;
            let err = outcome.unwrap_err();
            assert!(
                matches!(
                    &err,
                    CoreError::Install(petalfall_install::Error::ProcessorFailed {
                        status: Some(1),
                        ..
                    })
                ),
                "{err:?}"
            );
            assert_eq!(seen, Some(f.base(id)), "处理器跑的时候读得到原版那份");
            assert!(!f.json_path(id).exists());

            let id = "合并不成";
            f.petalfall
                .create_instance_dir(id, &f.request(UNMERGEABLE))
                .await
                .unwrap();
            f.fail_processors(false);
            let (outcome, seen) = f.install(id, UNMERGEABLE).await;
            let err = outcome.unwrap_err();
            assert!(
                matches!(
                    &err,
                    CoreError::Install(petalfall_install::Error::UnsupportedInstallProfile { .. })
                ),
                "{err:?}"
            );
            assert_eq!(seen, Some(f.base(id)), "处理器已经跑完");
            assert!(!f.json_path(id).exists());
        }

        /// 重新安装加载器、实例里是装好的合并结果（Forge 自己的库下载地址是空串，处理器读它会抛错）：处理器读到的是
        /// 原版那份；装不成时合并结果逐字节回来，再装一次装成了就换成新的合并结果。
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn reinstalling_over_a_merged_json_hands_processors_the_vanilla_one() {
            let f = fixture().await;
            let id = "老整合包";
            f.petalfall
                .create_instance_dir(id, &f.request(MERGEABLE))
                .await
                .unwrap();
            let forge = VersionJson::from_json_str(&format!(
                r#"{{"id":"test-forge-{MERGEABLE}","inheritsFrom":"test","mainClass":"{FORGE_MAIN}",
                    "libraries":[{{"name":"net.minecraftforge:forge:test-{MERGEABLE}:client",
                    "downloads":{{"artifact":{{"path":"net/minecraftforge/forge/test-{MERGEABLE}/forge-test-{MERGEABLE}-client.jar",
                    "url":"","sha1":"{}","size":{}}}}}}}]}}"#,
                "0".repeat(40),
                fastrand::u32(1..),
            ))
            .unwrap();
            let merged = merge_for_instance(id, &f.vanilla, Some(&forge)).unwrap();
            let before = serde_json::to_vec_pretty(&merged).unwrap();
            assert!(
                String::from_utf8_lossy(&before).contains(r#""url": """#),
                "夹具要带着下载地址为空串的库"
            );
            std::fs::write(f.json_path(id), &before).unwrap();

            f.fail_processors(true);
            let (outcome, seen) = f.install(id, MERGEABLE).await;
            assert!(outcome.is_err());
            let seen = seen.expect("处理器应读到客户端 jar 旁边的版本 JSON");
            assert_eq!(seen, f.base(id), "读到的不是实例原来的合并结果");
            assert_eq!(library_names(&seen), library_names(&f.vanilla));
            assert_eq!(std::fs::read(f.json_path(id)).unwrap(), before);

            f.fail_processors(false);
            let (outcome, seen) = f.install(id, MERGEABLE).await;
            outcome.unwrap();
            assert_eq!(seen, Some(f.base(id)));
            let installed =
                VersionJson::from_json_str(&std::fs::read_to_string(f.json_path(id)).unwrap())
                    .unwrap();
            assert_eq!(installed.main_class.as_deref(), Some(FORGE_MAIN));
            assert_eq!(library_names(&installed), library_names(&f.vanilla));
        }

        /// 实例的版本 JSON 写坏了、照记下的内容重装：处理器读到的是原版那份，装不成时写坏的原字节逐字节回来。
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn reinstalling_over_a_corrupt_json_restores_its_bytes_on_failure() {
            let f = fixture().await;
            let id = "写坏了";
            f.petalfall
                .create_instance_dir(id, &f.request(MERGEABLE))
                .await
                .unwrap();
            let corrupt: Vec<u8> = (0..fastrand::usize(1..4096))
                .map(|_| fastrand::u8(..))
                .collect();
            std::fs::write(f.json_path(id), &corrupt).unwrap();

            f.fail_processors(true);
            let (outcome, seen) = f.install(id, MERGEABLE).await;

            assert!(outcome.is_err());
            assert_eq!(seen, Some(f.base(id)));
            assert_eq!(std::fs::read(f.json_path(id)).unwrap(), corrupt);
        }
    }
}
