//! 版本安装编排：原版补全 + 可选 Mod 加载器（Fabric / Quilt / Forge / NeoForge）。
//!
//! 门面把 petalfall-install 的原版安装器与加载器安装器串起来，按当前配置的下载源策略与并发跑批量下载，
//! 并在关键节点发出阶段事件。Fabric / Quilt 的 meta 服务直接合成版本 JSON，无需本地安装器；
//! Forge / NeoForge 则先读该版本要求的 Java 主版本、备妥运行时，再下载并执行官方 installer
//! （内含 processors 子进程），故这两者的分支多一步 Java 解析。

use std::path::{Path, PathBuf};

use petalfall_install::forge::{FallbackJava, FallbackJavaError};
use petalfall_install::{
    ForgeInstaller, InstallContext, LoaderInstaller, LoaderSummary, VanillaInstaller,
    VanillaSummary, forge_installer_url, neoforge_installer_url,
};
use petalfall_java::JavaInstallation;
use petalfall_version::VersionJson;

use crate::error::Result;
use crate::event::{CoreEvent, EventSink, InstallPhase, emit};
use crate::facade::{Petalfall, make_context};

/// Forge / NeoForge 处理器产物校验不过时换用的 Java 主版本；游戏要求的 Java 低于它时才换。
///
/// 依据是实测：Forge 1.13.2（25.0.223）的 jarsplitter 1.1.0 在 Mojang 运行时自带的 Windows 版 1.8.0_51 上拆出的
/// slim、extra 两个 jar 与安装器声明的 sha1 都对不上（同样的输入，与时区无关），换 Java 16、17、21 就完全一致；
/// 整条处理器链（jarsplitter 1.1.0、SpecialSource 1.8.3、binarypatcher 1.1.1）在这几个版本上都跑得完，产物全部
/// 对得上。取 17 是因为它是 Mojang 运行时里最通用的一档，本机常有，没有时也能下载。
pub(crate) const PROCESSOR_FALLBACK_JAVA_MAJOR: u32 = 17;

/// 可安装的 Mod 加载器选择。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoaderChoice {
    /// Fabric。
    Fabric,
    /// Quilt。
    Quilt,
    /// Forge。
    Forge,
    /// NeoForge。
    NeoForge,
}

impl LoaderChoice {
    /// 加载器显示名（专有名词保留原文）。
    pub fn display_name(self) -> &'static str {
        match self {
            LoaderChoice::Fabric => "Fabric",
            LoaderChoice::Quilt => "Quilt",
            LoaderChoice::Forge => "Forge",
            LoaderChoice::NeoForge => "NeoForge",
        }
    }

    /// IPC 参数与未完成标记里用的小写键。
    pub fn key(self) -> &'static str {
        match self {
            LoaderChoice::Fabric => "fabric",
            LoaderChoice::Quilt => "quilt",
            LoaderChoice::Forge => "forge",
            LoaderChoice::NeoForge => "neoforge",
        }
    }

    /// 由 [`LoaderChoice::key`] 的取值还原；不认识的键返回 `None`。
    pub fn from_key(key: &str) -> Option<Self> {
        LoaderChoice::ALL
            .into_iter()
            .find(|choice| choice.key() == key)
    }

    /// 全部可安装的加载器。
    pub const ALL: [LoaderChoice; 4] = [
        LoaderChoice::Fabric,
        LoaderChoice::Quilt,
        LoaderChoice::Forge,
        LoaderChoice::NeoForge,
    ];

    /// [`Petalfall::install`] 装这个加载器版本时在 `versions/` 下用的版本 id。
    ///
    /// 真正的 id 写在加载器的 profile 或安装器里，要装到那一步才读得到；要在安装开始前占住目录的调用方只能按
    /// 各加载器的写法算。早期 Forge 的 legacy 安装器不一定是这个形状，那时算出的名字不对应任何目录，占了也不碍事。
    pub fn natural_version_id(self, game_version: &str, loader_version: &str) -> String {
        match self {
            LoaderChoice::Fabric => format!("fabric-loader-{loader_version}-{game_version}"),
            LoaderChoice::Quilt => format!("quilt-loader-{loader_version}-{game_version}"),
            // 1.20.1 的 NeoForge 沿用 Forge 的坐标，版本 id 也同形。
            LoaderChoice::NeoForge if game_version != "1.20.1" => {
                format!("neoforge-{loader_version}")
            }
            LoaderChoice::Forge | LoaderChoice::NeoForge => {
                format!("{game_version}-forge-{loader_version}")
            }
        }
    }
}

/// 一次安装的结果：原版摘要 + 可选加载器摘要。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallOutcome {
    /// 原版安装摘要。
    pub vanilla: VanillaSummary,
    /// 加载器安装摘要（未装加载器时为 `None`）。
    pub loader: Option<LoaderSummary>,
}

impl Petalfall {
    /// 安装指定原版版本，并可选叠加一个 Mod 加载器。
    ///
    /// `loader_version` 仅在指定了 `loader` 时有意义，缺省取该加载器对该游戏版本的推荐版。
    pub async fn install(
        &self,
        id: &str,
        loader: Option<LoaderChoice>,
        loader_version: Option<&str>,
        events: Option<&EventSink>,
    ) -> Result<InstallOutcome> {
        self.install_reporting(id, loader, loader_version, events, None)
            .await
    }

    /// 同 [`Petalfall::install`]，另把走到的每一步经 `phases` 报出，供把安装嵌进自己步骤表的任务（导入整合包）
    /// 接着报步骤。依次报 Metadata、Libraries、Assets、Natives，Fabric / Quilt 接着报 LoaderProfile、
    /// LoaderLibraries，Forge / NeoForge 接着报 Java、LoaderInstaller、LoaderLibraries、LoaderProcessors。
    /// `phases` 为 `None` 时一步都不报。
    pub(crate) async fn install_reporting(
        &self,
        id: &str,
        loader: Option<LoaderChoice>,
        loader_version: Option<&str>,
        events: Option<&EventSink>,
        phases: Option<&(dyn Fn(InstallPhase) + Send + Sync)>,
    ) -> Result<InstallOutcome> {
        // 测速选源可能要等几秒，算在读元数据这一步里，不让上一步的进度停在那里。
        if let Some(report) = phases {
            report(InstallPhase::Metadata);
        }
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
        .with_phases(phases);

        emit(events, CoreEvent::stage(format!("开始安装原版 {id}")));
        let vanilla = VanillaInstaller::new(cx)
            .with_manifest_url(self.manifest_url())
            .with_sources(self.source_plan(self.config().version_list_source))
            .install(id)
            .await?;
        emit(
            events,
            CoreEvent::stage(format!(
                "原版 {} 安装完成：库 {} / 资源 {} / natives {}",
                vanilla.id, vanilla.libraries, vanilla.assets, vanilla.natives
            )),
        );

        let loader = match loader {
            Some(choice) => {
                emit(
                    events,
                    CoreEvent::stage(format!("开始安装 {} 加载器", choice.display_name())),
                );
                let summary = match choice {
                    LoaderChoice::Fabric => {
                        LoaderInstaller::fabric(cx)
                            .with_base_url(self.fabric_base())
                            .install(id, loader_version)
                            .await?
                    }
                    LoaderChoice::Quilt => {
                        LoaderInstaller::quilt(cx)
                            .with_base_url(self.quilt_base())
                            .install(id, loader_version)
                            .await?
                    }
                    // Forge/NeoForge 无「最新版」查询接口，版本必须由调用方显式给出。
                    LoaderChoice::Forge => {
                        let forge_version = require_loader_version(choice, id, loader_version)?;
                        let url = forge_installer_url(id, forge_version);
                        self.install_forge(id, choice, forge_version, &url, cx, events)
                            .await?
                    }
                    LoaderChoice::NeoForge => {
                        let neoforge_version = require_loader_version(choice, id, loader_version)?;
                        let url = neoforge_installer_url(neoforge_version);
                        self.install_forge(id, choice, neoforge_version, &url, cx, events)
                            .await?
                    }
                };
                emit(
                    events,
                    CoreEvent::stage(format!(
                        "{} 加载器安装完成：{}（loader {}）",
                        choice.display_name(),
                        summary.id,
                        summary.loader_version
                    )),
                );
                Some(summary)
            }
            None => None,
        };

        Ok(InstallOutcome { vanilla, loader })
    }

    /// 安装 Forge/NeoForge：先据原版版本 JSON 定 Java 主版本、备妥运行时，再下载并执行官方 installer。
    ///
    /// 与 Fabric/Quilt 不同，Forge/NeoForge 的 installer 内含需 Java 子进程执行的 processors，故本步依赖
    /// [`Petalfall::resolve_java`] 取得可执行 java。`installer_url` 由调用方按风味拼好（Forge 需 mc+loader
    /// 两段，NeoForge 只需 loader 一段），本方法只负责 Java 解析、执行与把 [`petalfall_install::ForgeSummary`]
    /// 收敛成 [`LoaderSummary`]。`processors` 计数在 [`LoaderSummary`] 中无对应字段，改由阶段事件保留。
    async fn install_forge(
        &self,
        id: &str,
        choice: LoaderChoice,
        loader_version: &str,
        installer_url: &str,
        cx: InstallContext<'_>,
        events: Option<&EventSink>,
    ) -> Result<LoaderSummary> {
        // 原版安装已把版本 JSON 落在 versions/<id>/<id>.json，读回它取该版本要求的 Java 主版本
        // （缺省回落 8，与 launch 路径一致）。processors 用它执行 Forge 安装逻辑。
        let json_path = cx.layout.version_json(id);
        let bytes =
            tokio::fs::read(&json_path)
                .await
                .map_err(|source| petalfall_base::Error::Io {
                    path: json_path.clone(),
                    source,
                })?;
        // 直接按字节解析：有损转换会把坏字节换成替换符，让损坏的 JSON 看起来能读。
        let version: VersionJson =
            serde_json::from_slice(&bytes).map_err(|source| petalfall_version::Error::Json {
                context: "版本 JSON",
                source,
            })?;
        let required_major = version
            .java_version
            .as_ref()
            .map(|j| j.major_version)
            .unwrap_or(8);

        cx.enter(InstallPhase::Java);
        emit(
            events,
            CoreEvent::stage(format!(
                "{} 安装器需要 Java {required_major}，开始解析运行时",
                choice.display_name()
            )),
        );
        let (_, java_path) = self.resolve_java(required_major, events).await?;

        emit(
            events,
            CoreEvent::stage(format!(
                "下载并执行 {} 安装器：{installer_url}",
                choice.display_name()
            )),
        );
        let summary = self
            .forge_installer(cx, required_major, java_path, events)
            .install(installer_url)
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

        Ok(LoaderSummary {
            id: summary.id,
            loader_version: loader_version.to_owned(),
            libraries: summary.libraries,
        })
    }

    /// Forge / NeoForge 安装器：处理器先用 `java`（按游戏要求的主版本 `required_major` 取的那个）跑，installer 的
    /// `.sha1` 按下载源方案取；游戏要求的 Java 低于 [`PROCESSOR_FALLBACK_JAVA_MAJOR`] 时带上备用 Java。传统布局安装
    /// 与新建、重装实例共用这一份。
    pub(crate) fn forge_installer<'a>(
        &'a self,
        cx: InstallContext<'a>,
        required_major: u32,
        java: PathBuf,
        events: Option<&'a EventSink>,
    ) -> ForgeInstaller<'a> {
        let fallback = self.processor_fallback_java(required_major, &java, events);
        ForgeInstaller::new(cx, java)
            .with_sources(self.source_plan(self.config().download_source))
            .with_fallback_java(fallback)
    }

    /// 处理器产物校验不过时去取的备用 Java；游戏要求的 Java 已经不低于 [`PROCESSOR_FALLBACK_JAVA_MAJOR`] 时不提供。
    /// `primary` 是处理器先用的那个 Java。
    fn processor_fallback_java<'a>(
        &'a self,
        required_major: u32,
        primary: &Path,
        events: Option<&'a EventSink>,
    ) -> Option<FallbackJava<'a>> {
        if required_major >= PROCESSOR_FALLBACK_JAVA_MAJOR {
            return None;
        }
        let primary = primary.to_path_buf();
        Some(Box::new(move || {
            Box::pin(self.fetch_processor_fallback_java(primary.clone(), events))
        }))
    }

    /// 经 [`Petalfall::resolve_java`] 取 Java [`PROCESSOR_FALLBACK_JAVA_MAJOR`]：本机有就用，没有且开着自动下载就下
    /// Mojang 的运行时。不报步骤：这时已经在处理器那一步，Java 那一步早报过了，再报会让步骤倒退；下载运行时的进度
    /// 算进处理器这一步，与校验一步里补下载的做法一样。
    async fn fetch_processor_fallback_java(
        &self,
        primary: PathBuf,
        events: Option<&EventSink>,
    ) -> std::result::Result<PathBuf, FallbackJavaError> {
        let resolved = self
            .resolve_java(PROCESSOR_FALLBACK_JAVA_MAJOR, events)
            .await;
        let fallback = fallback_outcome(resolved)?;
        // 设置里固定了 Java 时取回的就是先用的那个，安装器认出是同一个、不重跑，这里也不说要换。
        if fallback != primary {
            emit(
                events,
                CoreEvent::stage(format!(
                    "安装器生成的文件与它声明的不符，改用 Java {PROCESSOR_FALLBACK_JAVA_MAJOR} 重跑：{}",
                    fallback.display()
                )),
            );
        }
        Ok(fallback)
    }
}

/// 取备用 Java 的结果交给安装器：取消照常以取消结束；别的失败记一条 warn，原因原样交给安装器写进报错。
fn fallback_outcome(
    resolved: Result<(Vec<JavaInstallation>, PathBuf)>,
) -> std::result::Result<PathBuf, FallbackJavaError> {
    match resolved {
        Ok((_, path)) => Ok(path),
        Err(err) if err.is_cancelled() => Err(FallbackJavaError::Cancelled),
        Err(err) => {
            tracing::warn!(
                major = PROCESSOR_FALLBACK_JAVA_MAJOR,
                error = %err,
                "没能取到备用 Java，处理器产物对不上时没法换 Java 重跑"
            );
            Err(FallbackJavaError::Unavailable(Box::new(err)))
        }
    }
}

/// 取出 Forge/NeoForge 的显式 loader 版本；缺省即报错，绝不静默兜底一个版本。
///
/// Forge/NeoForge 的官方 maven 无 Fabric/Quilt 那样的「列出可用 loader」接口，无从推断推荐版，故要求
/// 调用方显式传入。复用 [`petalfall_install::Error::LoaderVersionNotFound`] 表达「没有可用 loader 版本」。
fn require_loader_version<'a>(
    choice: LoaderChoice,
    game_version: &str,
    loader_version: Option<&'a str>,
) -> Result<&'a str> {
    loader_version.ok_or_else(|| {
        petalfall_install::Error::LoaderVersionNotFound {
            loader: choice.display_name(),
            game_version: game_version.to_owned(),
        }
        .into()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PetalfallConfig;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// 算出的 id 要与加载器自己写在版本 JSON 里的一致，否则导入前占的不是它真正写的目录。
    #[test]
    fn natural_version_ids_match_what_each_loader_writes() {
        for (choice, game, loader, id) in [
            (
                LoaderChoice::Fabric,
                "1.21",
                "0.15.11",
                "fabric-loader-0.15.11-1.21",
            ),
            (
                LoaderChoice::Quilt,
                "1.21",
                "0.26.0",
                "quilt-loader-0.26.0-1.21",
            ),
            (
                LoaderChoice::Forge,
                "1.20.1",
                "47.2.0",
                "1.20.1-forge-47.2.0",
            ),
            (
                LoaderChoice::NeoForge,
                "1.21",
                "21.0.167",
                "neoforge-21.0.167",
            ),
            (
                LoaderChoice::NeoForge,
                "1.20.1",
                "47.1.106",
                "1.20.1-forge-47.1.106",
            ),
        ] {
            assert_eq!(choice.natural_version_id(game, loader), id);
        }
    }

    /// 安装一个「空壳」原版：清单指向 mock，版本 JSON 无下载/资源/库，install 只落版本 JSON。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn install_vanilla_downloads_version_json_and_reports_zero_counts() {
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
        Mock::given(method("GET"))
            .and(path("/test.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"id":"test","type":"release","mainClass":"net.minecraft.client.main.Main"}"#,
            ))
            .mount(&server)
            .await;

        let tmp = tempfile::tempdir().unwrap();
        let mc = tmp.path().to_path_buf();
        let petalfall = Petalfall::for_test(PetalfallConfig::default(), mc.clone(), mc.clone())
            .with_manifest_url(format!("{base}/manifest.json"));

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let outcome = petalfall
            .install("test", None, None, Some(&tx))
            .await
            .unwrap();

        assert_eq!(outcome.vanilla.id, "test");
        assert_eq!(outcome.vanilla.libraries, 0);
        assert_eq!(outcome.vanilla.assets, 0);
        assert_eq!(outcome.vanilla.natives, 0);
        assert!(outcome.loader.is_none());

        // 版本 JSON 已落盘到 versions/test/test.json。
        let json_path = mc.join("versions").join("test").join("test.json");
        let on_disk = tokio::fs::read_to_string(&json_path).await.unwrap();
        assert!(on_disk.contains("net.minecraft.client.main.Main"));

        // 至少发出「开始」与「完成」两条阶段事件。
        drop(tx);
        let mut stages = Vec::new();
        while let Some(ev) = rx.recv().await {
            if let CoreEvent::Stage(s) = ev {
                stages.push(s);
            }
        }
        assert!(stages.iter().any(|s| s.contains("开始安装原版 test")));
        assert!(stages.iter().any(|s| s.contains("原版 test 安装完成")));
    }

    /// 自动档装版本：入口先测一轮速，版本清单与版本 JSON 都从实测排在前面的源取（镜像探不通，这里是官方，
    /// 与测不出时的镜像优先相反，证明顺序来自实测）；有效期内再装一次不再探测。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn auto_install_measures_once_and_fetches_metadata_from_the_measured_source() {
        use std::sync::Arc;
        use std::time::Duration;
        use wiremock::matchers::header;

        // 清单与版本 JSON 的官方地址用解析不了的域名，只经源计划落到两台 mock：接线断了也连不到真实网络。
        let manifest_path = "/mc/game/version_manifest_v2.json";
        let version_path = "/v1/packages/0123/test.json";
        let official = MockServer::start().await;
        let mirror = MockServer::start().await;
        for (server, probe_status) in [(&official, 206), (&mirror, 503)] {
            Mock::given(method("GET"))
                .and(path(manifest_path))
                .and(header("range", "bytes=0-0"))
                .respond_with(
                    ResponseTemplate::new(probe_status)
                        .insert_header("content-range", "bytes 0-0/100"),
                )
                .with_priority(1)
                .mount(server)
                .await;
            Mock::given(method("GET"))
                .and(path(manifest_path))
                .respond_with(ResponseTemplate::new(200).set_body_string(format!(
                    r#"{{"latest":{{"release":"test","snapshot":"test"}},
                    "versions":[{{"id":"test","type":"release","url":"https://meta.invalid{version_path}",
                    "time":"t","releaseTime":"t"}}]}}"#
                )))
                .mount(server)
                .await;
            Mock::given(method("GET"))
                .and(path(version_path))
                .respond_with(ResponseTemplate::new(200).set_body_string(
                    r#"{"id":"test","type":"release","mainClass":"net.minecraft.client.main.Main"}"#,
                ))
                .mount(server)
                .await;
        }

        let tmp = tempfile::tempdir().unwrap();
        let mc = tmp.path().to_path_buf();
        let petalfall = Petalfall::for_test(PetalfallConfig::default(), mc.clone(), mc.clone())
            .with_manifest_url(format!("https://meta.invalid{manifest_path}"))
            .with_source_routes(
                Arc::new(crate::sources::tests::Routes {
                    official: official.uri(),
                    mirror: mirror.uri(),
                }),
                Duration::from_secs(600),
                Duration::from_secs(30),
            );

        petalfall.install("test", None, None, None).await.unwrap();
        petalfall.install("test", None, None, None).await.unwrap();

        // (探针, 清单, 版本 JSON) 各收到几次。
        let tally = |requests: Vec<wiremock::Request>| {
            let probes = requests
                .iter()
                .filter(|r| r.headers.contains_key("range"))
                .count();
            let plain = |p: &str| {
                requests
                    .iter()
                    .filter(|r| r.url.path() == p && !r.headers.contains_key("range"))
                    .count()
            };
            (probes, plain(manifest_path), plain(version_path))
        };
        assert_eq!(
            tally(official.received_requests().await.unwrap()),
            (1, 2, 2)
        );
        assert_eq!(tally(mirror.received_requests().await.unwrap()), (1, 0, 0));
        let json_path = mc.join("versions").join("test").join("test.json");
        assert!(
            tokio::fs::read_to_string(&json_path)
                .await
                .unwrap()
                .contains("net.minecraft.client.main.Main")
        );
    }

    /// 原版 + Fabric 加载器：门面先装空壳原版，再叠加 Fabric（profile JSON 落盘 + 下加载器库）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn install_with_fabric_loader_wires_both_stages() {
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
        Mock::given(method("GET"))
            .and(path("/test.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"id":"test","type":"release","mainClass":"net.minecraft.client.main.Main"}"#,
            ))
            .mount(&server)
            .await;
        // Fabric meta profile JSON：inheritsFrom test，含一个走 mock 仓库的加载器库。
        Mock::given(method("GET"))
            .and(path("/v2/versions/loader/test/0.15.0/profile/json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(format!(
                r#"{{"id":"fabric-loader-0.15.0-test","inheritsFrom":"test",
                    "mainClass":"net.fabricmc.loader.impl.launch.knot.KnotClient",
                    "libraries":[{{"name":"net.example:loader:0.15.0","url":"{base}/"}}]}}"#
            )))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/net/example/loader/0.15.0/loader-0.15.0.jar"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"loader-jar".to_vec()))
            .mount(&server)
            .await;

        let tmp = tempfile::tempdir().unwrap();
        let mc = tmp.path().to_path_buf();
        let petalfall = Petalfall::for_test(PetalfallConfig::default(), mc.clone(), mc.clone())
            .with_manifest_url(format!("{base}/manifest.json"))
            .with_fabric_base(&base);

        let outcome = petalfall
            .install("test", Some(LoaderChoice::Fabric), Some("0.15.0"), None)
            .await
            .unwrap();

        assert_eq!(outcome.vanilla.id, "test");
        let loader = outcome.loader.expect("应装上 Fabric 加载器");
        assert_eq!(loader.id, "fabric-loader-0.15.0-test");
        assert_eq!(loader.loader_version, "0.15.0");
        assert_eq!(loader.libraries, 1);

        // 加载器 profile JSON 落盘、加载器库落到 libraries 下。
        let profile = mc
            .join("versions")
            .join("fabric-loader-0.15.0-test")
            .join("fabric-loader-0.15.0-test.json");
        assert!(profile.is_file());
        let lib = mc
            .join("libraries")
            .join("net")
            .join("example")
            .join("loader")
            .join("0.15.0")
            .join("loader-0.15.0.jar");
        assert_eq!(tokio::fs::read(&lib).await.unwrap(), b"loader-jar");
    }

    /// 版本 test：清单、声明了客户端 jar 的版本 JSON，客户端 jar 的响应由用例定。
    async fn mount_vanilla_with_client(server: &MockServer, client_jar: ResponseTemplate) {
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
            .respond_with(ResponseTemplate::new(200).set_body_string(format!(
                r#"{{"id":"test","type":"release","mainClass":"net.minecraft.client.main.Main",
                    "downloads":{{"client":{{"sha1":"0000000000000000000000000000000000000000",
                    "size":6,"url":"{base}/client.jar"}}}}}}"#
            )))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/client.jar"))
            .respond_with(client_jar)
            .mount(server)
            .await;
    }

    /// 客户端 jar 下不下来：原版没装上，版本列表里看不到它，也不多出一行损坏项。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_failed_install_is_not_listed_as_a_version() {
        let server = MockServer::start().await;
        mount_vanilla_with_client(&server, ResponseTemplate::new(404)).await;
        let tmp = tempfile::tempdir().unwrap();
        let mc = tmp.path().to_path_buf();
        let petalfall = Petalfall::for_test(PetalfallConfig::default(), mc.clone(), mc.clone())
            .with_manifest_url(format!("{}/manifest.json", server.uri()));

        petalfall
            .install("test", None, None, None)
            .await
            .expect_err("客户端 jar 下不下来");

        let scan = petalfall_instance::discover_versions(&mc).await.unwrap();
        assert_eq!(scan, petalfall_instance::VersionScan::default());
    }

    /// 取消发生在下客户端 jar 的途中：同样不留版本，门面按已取消返回。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_cancelled_install_is_not_listed_as_a_version() {
        let server = MockServer::start().await;
        let stalled = ResponseTemplate::new(200)
            .set_body_bytes(b"client".to_vec())
            .set_delay(std::time::Duration::from_secs(30));
        mount_vanilla_with_client(&server, stalled).await;
        let tmp = tempfile::tempdir().unwrap();
        let mc = tmp.path().to_path_buf();
        let token = petalfall_download::CancellationToken::new();
        let petalfall = Petalfall::for_test(PetalfallConfig::default(), mc.clone(), mc.clone())
            .with_manifest_url(format!("{}/manifest.json", server.uri()))
            .with_cancel(token.clone());
        let attempt = petalfall.install("test", None, None, None);
        tokio::pin!(attempt);

        let client_jar_requested = async {
            loop {
                let requests = server.received_requests().await.unwrap();
                if requests.iter().any(|r| r.url.path() == "/client.jar") {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        };
        tokio::select! {
            outcome = &mut attempt => panic!("客户端 jar 还没回来，安装不该结束：{outcome:?}"),
            () = client_jar_requested => {}
        }
        token.cancel();
        let err = attempt.await.unwrap_err();

        assert!(
            matches!(
                err,
                crate::error::CoreError::Install(petalfall_install::Error::Download(
                    petalfall_download::Error::Cancelled
                ))
            ),
            "实际：{err:?}"
        );
        let scan = petalfall_instance::discover_versions(&mc).await.unwrap();
        assert_eq!(scan, petalfall_instance::VersionScan::default());
    }

    /// 原版装好了、加载器库下不下来：原版是真的装好了，留着；加载器版本不出现。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_failed_loader_leaves_only_the_finished_vanilla() {
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
        Mock::given(method("GET"))
            .and(path("/test.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"id":"test","type":"release","mainClass":"net.minecraft.client.main.Main"}"#,
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v2/versions/loader/test/0.15.0/profile/json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(format!(
                r#"{{"id":"fabric-loader-0.15.0-test","inheritsFrom":"test","mainClass":"Knot",
                    "libraries":[{{"name":"net.example:loader:0.15.0","url":"{base}/"}}]}}"#
            )))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/net/example/loader/0.15.0/loader-0.15.0.jar"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let tmp = tempfile::tempdir().unwrap();
        let mc = tmp.path().to_path_buf();
        let petalfall = Petalfall::for_test(PetalfallConfig::default(), mc.clone(), mc.clone())
            .with_manifest_url(format!("{base}/manifest.json"))
            .with_fabric_base(&base);

        petalfall
            .install("test", Some(LoaderChoice::Fabric), Some("0.15.0"), None)
            .await
            .expect_err("加载器库下不下来");

        let scan = petalfall_instance::discover_versions(&mc).await.unwrap();
        let ids: Vec<&str> = scan.versions.iter().map(|v| v.id.as_str()).collect();
        assert_eq!(ids, vec!["test"]);
        assert_eq!(scan.broken, Vec::new());
    }

    /// 传统布局的 Forge 与实例路径一样按下载源方案取 installer 的 `.sha1`：官方 maven 不通时才有镜像兜底，
    /// 否则缓存里的 installer 无法复用，下载时也没有校验值。冒充的 installer 不是 zip，流程停在读安装配置。
    #[cfg(windows)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn legacy_forge_install_fetches_the_installer_sha1_through_the_source_plan() {
        const SHA1_PATH: &str =
            "/net/minecraftforge/forge/test-47.2.0/forge-test-47.2.0-installer.jar.sha1";
        let installer_path = SHA1_PATH.trim_end_matches(".sha1");
        let official = MockServer::start().await;
        let mirror = MockServer::start().await;
        let manifest_path = "/mc/game/version_manifest_v2.json";
        let base = "https://meta.invalid";
        for server in [&official, &mirror] {
            Mock::given(method("GET"))
                .and(path(manifest_path))
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
                    r#"{"id":"test","type":"release","mainClass":"net.minecraft.client.main.Main",
                        "javaVersion":{"component":"java-runtime-gamma","majorVersion":17}}"#,
                ))
                .mount(server)
                .await;
            Mock::given(method("GET"))
                .and(path(installer_path))
                .respond_with(ResponseTemplate::new(200).set_body_bytes(b"not a zip".to_vec()))
                .mount(server)
                .await;
        }
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let payload = root.join("payload");
        std::fs::write(&payload, b"not a zip").unwrap();
        let sha1 = petalfall_base::fs::sha1_hex(&payload).await.unwrap();
        Mock::given(method("GET"))
            .and(path(SHA1_PATH))
            .respond_with(ResponseTemplate::new(404))
            .mount(&official)
            .await;
        Mock::given(method("GET"))
            .and(path(SHA1_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_string(sha1))
            .mount(&mirror)
            .await;

        let java = root.join("fake-java-17.cmd");
        std::fs::write(
            &java,
            "@echo off\r\n>&2 echo openjdk version \"17.0.8\" 2023-07-18\r\nexit /b 0\r\n",
        )
        .unwrap();
        let config = PetalfallConfig {
            default_java: Some(java),
            ..PetalfallConfig::default()
        };
        let petalfall = Petalfall::for_test(config, root.clone(), root.clone())
            .with_manifest_url(format!("{base}{manifest_path}"))
            .with_source_routes(
                std::sync::Arc::new(crate::sources::tests::Routes {
                    official: official.uri(),
                    mirror: mirror.uri(),
                }),
                std::time::Duration::from_secs(600),
                std::time::Duration::from_secs(2),
            );

        petalfall
            .install("test", Some(LoaderChoice::Forge), Some("47.2.0"), None)
            .await
            .unwrap_err();

        let sha1_requests = mirror
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|request| request.url.path() == SHA1_PATH)
            .count();
        assert_eq!(sha1_requests, 1, "官方取不到时应换镜像取 .sha1");
    }

    /// 原版 JSON 声明 javaVersion.majorVersion，Forge 分支须读回它并据此解析 Java；这里给一个
    /// 不存在的主版本（999）且关闭自动下载，令 resolve_java 必然报 NoJava。断言：
    /// (1) 选择 Forge 走到了 Java 解析（Fabric/Quilt 分支不碰 Java），
    /// (2) java 主版本正是从原版 JSON 读出的 999（非硬编码 8），
    /// (3) 失败发生在下载 installer（硬编码 maven 地址）之前，故测试不触网。
    /// 删掉「读版本 JSON 定 Java 主版本 + resolve_java」逻辑，本断言即挂。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn install_forge_reads_java_major_before_installer_download() {
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
        // 版本 JSON 携带 javaVersion.majorVersion=999（本机不可能装有 Java 999）。
        Mock::given(method("GET"))
            .and(path("/test.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"id":"test","type":"release","mainClass":"net.minecraft.client.main.Main",
                    "javaVersion":{"component":"jre-legacy","majorVersion":999}}"#,
            ))
            .mount(&server)
            .await;

        let tmp = tempfile::tempdir().unwrap();
        let mc = tmp.path().to_path_buf();
        let mut petalfall = Petalfall::for_test(PetalfallConfig::default(), mc.clone(), mc.clone())
            .with_manifest_url(format!("{base}/manifest.json"));
        // 关闭自动下载：无匹配 Java 时直接报 NoJava，而非去拉 Mojang 运行时（避免触网）。
        petalfall.set_auto_download_java(false);

        let err = petalfall
            .install("test", Some(LoaderChoice::Forge), Some("47.2.0"), None)
            .await
            .unwrap_err();
        assert!(
            matches!(err, crate::error::CoreError::NoJava { major: 999 }),
            "应因缺 Java 999 而在下载 installer 前失败，实际：{err:?}"
        );

        // 原版 JSON 确已落盘（Forge 分支正是读它取 Java 主版本）。
        let json_path = mc.join("versions").join("test").join("test.json");
        assert!(json_path.is_file());
    }

    /// Forge/NeoForge 无「最新版」查询，缺省 loader 版本必须显式报错，绝不静默兜底。
    /// 删掉 require_loader_version 的缺省校验（放任继续），错误类型将变（NoJava/下载失败），本断言即挂。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn install_forge_without_version_reports_loader_version_missing() {
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
        Mock::given(method("GET"))
            .and(path("/test.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"id":"test","type":"release","mainClass":"net.minecraft.client.main.Main"}"#,
            ))
            .mount(&server)
            .await;

        let tmp = tempfile::tempdir().unwrap();
        let mc = tmp.path().to_path_buf();
        let petalfall = Petalfall::for_test(PetalfallConfig::default(), mc.clone(), mc)
            .with_manifest_url(format!("{base}/manifest.json"));

        // NeoForge 缺省版本 -> LoaderVersionNotFound（loader 名取自 display_name，game_version 为原版 id）。
        let err = petalfall
            .install("test", Some(LoaderChoice::NeoForge), None, None)
            .await
            .unwrap_err();
        match err {
            crate::error::CoreError::Install(petalfall_install::Error::LoaderVersionNotFound {
                loader,
                game_version,
            }) => {
                assert_eq!(loader, "NeoForge");
                assert_eq!(game_version, "test");
            }
            other => panic!("应报 LoaderVersionNotFound，实际：{other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn install_unknown_version_bubbles_installer_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/manifest.json"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"latest":{"release":"a","snapshot":"a"},"versions":[]}"#),
            )
            .mount(&server)
            .await;

        let tmp = tempfile::tempdir().unwrap();
        let mc = tmp.path().to_path_buf();
        let petalfall = Petalfall::for_test(PetalfallConfig::default(), mc.clone(), mc)
            .with_manifest_url(format!("{}/manifest.json", server.uri()));

        let err = petalfall
            .install("nope", None, None, None)
            .await
            .unwrap_err();
        // 清单里没有该版本 -> 安装器报 InstallerEntryMissing，经门面冒泡为 Install 变体。
        assert!(matches!(err, crate::error::CoreError::Install(_)));
    }

    /// 空壳原版 test（不声明任何文件）与它上面的 Fabric 0.15.0（一个加载器库）。
    async fn mount_vanilla_and_fabric(server: &MockServer) {
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
        Mock::given(method("GET"))
            .and(path("/v2/versions/loader/test/0.15.0/profile/json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(format!(
                r#"{{"id":"fabric-loader-0.15.0-test","inheritsFrom":"test","mainClass":"Knot",
                    "libraries":[{{"name":"net.example:loader:0.15.0","url":"{base}/"}}]}}"#
            )))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/net/example/loader/0.15.0/loader-0.15.0.jar"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"loader-jar".to_vec()))
            .mount(server)
            .await;
    }

    fn phase_events(events: &[CoreEvent]) -> usize {
        events
            .iter()
            .filter(|event| matches!(event, CoreEvent::Phase { .. }))
            .count()
    }

    /// 带回调装原版：依次报读元数据、库、资源、natives，各一次；步骤只经回调报，不自己发步骤事件（步骤表归
    /// 调用方）。同一版本再装一遍（原版已装过）报的步骤一模一样，不跳、不倒退、不重复。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn install_reporting_announces_the_vanilla_steps_in_order() {
        use InstallPhase::*;
        let server = MockServer::start().await;
        mount_vanilla_and_fabric(&server).await;
        let tmp = tempfile::tempdir().unwrap();
        let mc = tmp.path().to_path_buf();
        let petalfall = Petalfall::for_test(PetalfallConfig::default(), mc.clone(), mc.clone())
            .with_manifest_url(format!("{}/manifest.json", server.uri()));

        for round in ["首次", "原版已装过"] {
            let seen = std::sync::Mutex::new(Vec::new());
            let record = |phase: InstallPhase| seen.lock().unwrap().push(phase);
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let outcome = petalfall
                .install_reporting("test", None, None, Some(&tx), Some(&record))
                .await
                .unwrap();
            drop(tx);

            assert_eq!(outcome.vanilla.id, "test", "{round}");
            assert!(outcome.loader.is_none(), "{round}");
            assert_eq!(
                *seen.lock().unwrap(),
                vec![Metadata, Libraries, Assets, Natives],
                "{round}"
            );
            let events = std::iter::from_fn(|| rx.try_recv().ok()).collect::<Vec<_>>();
            assert_eq!(phase_events(&events), 0, "{round}: {events:?}");
        }
    }

    /// 带回调装 Fabric：原版四步之后报取 profile、下加载器库；加载器已装过时再装一遍，步骤照旧。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn install_reporting_announces_the_fabric_steps_after_the_vanilla_ones() {
        use InstallPhase::*;
        let server = MockServer::start().await;
        mount_vanilla_and_fabric(&server).await;
        let tmp = tempfile::tempdir().unwrap();
        let mc = tmp.path().to_path_buf();
        let petalfall = Petalfall::for_test(PetalfallConfig::default(), mc.clone(), mc.clone())
            .with_manifest_url(format!("{}/manifest.json", server.uri()))
            .with_fabric_base(server.uri());

        for round in ["首次", "加载器已装过"] {
            let seen = std::sync::Mutex::new(Vec::new());
            let record = |phase: InstallPhase| seen.lock().unwrap().push(phase);
            let outcome = petalfall
                .install_reporting(
                    "test",
                    Some(LoaderChoice::Fabric),
                    Some("0.15.0"),
                    None,
                    Some(&record),
                )
                .await
                .unwrap();

            let loader = outcome.loader.expect("应装上 Fabric 加载器");
            assert_eq!(loader.id, "fabric-loader-0.15.0-test", "{round}");
            assert_eq!(
                *seen.lock().unwrap(),
                vec![
                    Metadata,
                    Libraries,
                    Assets,
                    Natives,
                    LoaderProfile,
                    LoaderLibraries
                ],
                "{round}"
            );
        }
        assert!(
            mc.join("versions")
                .join("fabric-loader-0.15.0-test")
                .join("fabric-loader-0.15.0-test.json")
                .is_file()
        );
    }

    /// Forge 在解析 Java 之前报 Java 这一步：找不到 Java 999 时停在这一步，后面的安装器、库、处理器都没报。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn install_reporting_announces_java_before_resolving_it_for_forge() {
        use InstallPhase::*;
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
        Mock::given(method("GET"))
            .and(path("/test.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"id":"test","type":"release","mainClass":"net.minecraft.client.main.Main",
                    "javaVersion":{"component":"jre-legacy","majorVersion":999}}"#,
            ))
            .mount(&server)
            .await;
        let tmp = tempfile::tempdir().unwrap();
        let mc = tmp.path().to_path_buf();
        let mut petalfall = Petalfall::for_test(PetalfallConfig::default(), mc.clone(), mc)
            .with_manifest_url(format!("{base}/manifest.json"));
        petalfall.set_auto_download_java(false);

        let seen = std::sync::Mutex::new(Vec::new());
        let record = |phase: InstallPhase| seen.lock().unwrap().push(phase);
        let err = petalfall
            .install_reporting(
                "test",
                Some(LoaderChoice::Forge),
                Some("47.2.0"),
                None,
                Some(&record),
            )
            .await
            .unwrap_err();

        assert!(
            matches!(err, crate::error::CoreError::NoJava { major: 999 }),
            "实际：{err:?}"
        );
        assert_eq!(
            *seen.lock().unwrap(),
            vec![Metadata, Libraries, Assets, Natives, Java]
        );
    }

    /// 不带回调的 install 一条步骤事件都不发：装原版与装 Fabric 都只有阶段文字与下载快照。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn install_without_a_callback_announces_no_steps() {
        let server = MockServer::start().await;
        mount_vanilla_and_fabric(&server).await;
        let tmp = tempfile::tempdir().unwrap();
        let mc = tmp.path().to_path_buf();
        let petalfall = Petalfall::for_test(PetalfallConfig::default(), mc.clone(), mc.clone())
            .with_manifest_url(format!("{}/manifest.json", server.uri()))
            .with_fabric_base(server.uri());

        for loader in [None, Some(LoaderChoice::Fabric)] {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            petalfall
                .install("test", loader, Some("0.15.0"), Some(&tx))
                .await
                .unwrap();
            drop(tx);
            let events = std::iter::from_fn(|| rx.try_recv().ok()).collect::<Vec<_>>();
            assert_eq!(phase_events(&events), 0, "{loader:?}: {events:?}");
            // 事件确实收到了：阶段文字一条不少，证明上面的「零条」不是因为通道里什么都没有。
            let stages: Vec<&str> = events
                .iter()
                .filter_map(|event| match event {
                    CoreEvent::Stage(text) => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            assert!(stages.contains(&"开始安装原版 test"), "{stages:?}");
            assert_eq!(
                stages
                    .contains(&"Fabric 加载器安装完成：fabric-loader-0.15.0-test（loader 0.15.0）"),
                loader.is_some(),
                "{stages:?}"
            );
        }
    }

    /// 只有游戏要求的 Java 低于 17 时才给安装器备用 Java：已经是 17 及以上，换了也是同一档。
    #[test]
    fn the_fallback_java_is_offered_only_below_java_17() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let petalfall = Petalfall::for_test(PetalfallConfig::default(), root.clone(), root.clone());
        let primary = root.join("java.exe");
        assert_eq!(PROCESSOR_FALLBACK_JAVA_MAJOR, 17);
        for (major, offered) in [(8, true), (16, true), (17, false), (21, false), (25, false)] {
            assert_eq!(
                petalfall
                    .processor_fallback_java(major, &primary, None)
                    .is_some(),
                offered,
                "游戏要求 Java {major}"
            );
        }
    }

    /// 取备用 Java 失败时原因原样交给安装器，取消仍是取消、不当成取不到。
    #[test]
    fn a_failed_fallback_keeps_its_reason_and_a_cancelled_one_stays_cancelled() {
        match fallback_outcome(Err(crate::error::CoreError::NoJava { major: 17 })) {
            Err(FallbackJavaError::Unavailable(reason)) => assert_eq!(
                reason.to_string(),
                "未找到匹配 Java 17 的运行时，且自动下载已关闭（开启 auto_download_java 或手动安装对应 Java）"
            ),
            other => panic!("应为取不到，实际：{other:?}"),
        }
        for cancelled in [
            crate::error::CoreError::Java(petalfall_java::Error::Cancelled),
            crate::error::CoreError::Install(petalfall_install::Error::Download(
                petalfall_download::Error::Cancelled,
            )),
        ] {
            assert!(
                matches!(
                    fallback_outcome(Err(cancelled)),
                    Err(FallbackJavaError::Cancelled)
                ),
                "取消应照常以取消结束"
            );
        }
        let picked = PathBuf::from(r"D:\data\runtime\17\bin\java.exe");
        assert_eq!(
            fallback_outcome(Ok((Vec::new(), picked.clone()))).unwrap(),
            picked
        );
    }

    /// 经 resolve_java 取备用 Java 失败（这里是设置里固定的 Java 不见了）：原因交给安装器，产物对不上的报错正文里
    /// 就带着它。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_failed_fallback_java_hands_its_reason_to_the_installer_error() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let missing = root
            .join("jdk-8")
            .join("bin")
            .join(petalfall_java::JAVA_EXE);
        let config = PetalfallConfig {
            default_java: Some(missing.clone()),
            auto_download_java: false,
            ..PetalfallConfig::default()
        };
        let petalfall = Petalfall::for_test(config, root.clone(), root.clone());
        let fetch = petalfall
            .processor_fallback_java(8, &missing, None)
            .expect("游戏要求 Java 8 时应带备用来源");

        let reason = match fetch().await {
            Err(FallbackJavaError::Unavailable(reason)) => reason,
            other => panic!("应为取不到，实际：{other:?}"),
        };

        let shown = format!("执行 java 可执行文件失败：{}", missing.display());
        assert_eq!(reason.to_string(), shown);
        let err = petalfall_install::Error::ProcessorOutputMismatch(Box::new(
            petalfall_install::error::ProcessorOutputMismatch {
                jar: "net.minecraftforge:jarsplitter:1.1.0".into(),
                path: root.join("client-1.13.2-20190213.203750-extra.jar"),
                expected: "938487f2".into(),
                actual: "2f9435b4".into(),
                java: missing.clone(),
                retried_with: None,
                fallback_error: Some(reason),
            },
        ));
        let message = err.to_string();
        assert!(
            message.ends_with(&format!("；想换一个 Java 重跑，但没能取到：{shown}")),
            "{message}"
        );
    }

    /// 取回的备用 Java 与先用的不是同一个时才发「改用 Java 17 重跑」的阶段事件；设置里固定了 Java、取回的还是先用的
    /// 那个时不说要换。
    #[cfg(windows)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn switching_java_is_announced_only_for_another_java() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let fixed = root.join("fake-java-17.cmd");
        std::fs::write(
            &fixed,
            "@echo off\r\n>&2 echo openjdk version \"17.0.8\" 2023-07-18\r\nexit /b 0\r\n",
        )
        .unwrap();
        let config = PetalfallConfig {
            default_java: Some(fixed.clone()),
            auto_download_java: false,
            ..PetalfallConfig::default()
        };
        let petalfall = Petalfall::for_test(config, root.clone(), root.clone());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let other = root
            .join("jdk-8")
            .join("bin")
            .join(petalfall_java::JAVA_EXE);
        {
            let same = petalfall
                .processor_fallback_java(8, &fixed, Some(&tx))
                .unwrap();
            assert_eq!(same().await.unwrap(), fixed);
            let switched = petalfall
                .processor_fallback_java(8, &other, Some(&tx))
                .unwrap();
            assert_eq!(switched().await.unwrap(), fixed);
        }
        drop(tx);

        let mut announced = Vec::new();
        while let Some(event) = rx.recv().await {
            if let CoreEvent::Stage(text) = event
                && text.contains("重跑")
            {
                announced.push(text);
            }
        }
        assert_eq!(
            announced,
            vec![format!(
                "安装器生成的文件与它声明的不符，改用 Java 17 重跑：{}",
                fixed.display()
            )]
        );
    }
}
