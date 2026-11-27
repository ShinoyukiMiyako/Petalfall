//! 资源包、光影、数据包从平台装进实例。
//!
//! 每种资源落到实例工作目录下各自的位置：资源包 `resourcepacks/`、光影 `shaderpacks/`、
//! 数据包 `saves/<存档>/datapacks/`（数据包只对单个存档生效，必须指明装进哪个存档）。Mod 不走这里，
//! 见 [`ResourceTarget::resource_type`]。
//! 工作目录与启动时同一套隔离判定（[`Petalfall::resolve_working_dir`]），装进去的东西启动后才找得到。
//!
//! 平台给的文件名直接拼进落盘路径，必须先验成单个安全的文件名：一个带 `..` 或路径分隔符的
//! 文件名就能把文件写到实例目录之外。

use std::path::{Path, PathBuf};

use petalfall_download::DownloadTask;
use petalfall_instance::is_dos_short_name;
use petalfall_modplatform::{CurseForgeClient, ModrinthClient, Platform, ResourceType};

use crate::blocked::{BlockedFile, ClaimedCopy, place_claimed, single_blocked};
use crate::data_format::GuardMode;
use crate::error::{CoreError, Result};
use crate::event::{CoreEvent, EventSink, emit};
use crate::facade::Petalfall;

/// 一次资源安装的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceInstallOutcome {
    /// 落盘的文件名。
    pub file_name: String,
    /// 文件完整路径。
    pub path: PathBuf,
    /// 来源平台。
    pub platform: Platform,
    /// 资源类型。
    pub resource_type: ResourceType,
    /// 数据包装进的存档；其余类型为 `None`。
    pub world: Option<String>,
}

/// 一次资源安装的目标。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceTarget<'a> {
    /// 实例（版本目录名）。
    pub version_id: &'a str,
    /// 资源类型；整合包、插件与 Mod 不在此列。Mod 要经 `install_mod`：那条路径占住 mods 目录、
    /// 写卷宗与历史、先探测游戏是否占着同名文件，直接下载到 mods/ 会无备份地覆盖玩家的同名 jar，
    /// 装进来的 Mod 也没有来源，查不了更新、回滚不了。
    pub resource_type: ResourceType,
    /// 数据包装进的存档目录名；其余类型必须为 `None`。
    pub world: Option<&'a str>,
}

impl Petalfall {
    /// 把平台上某工程的一个版本（Modrinth 版本 id / CurseForge 文件 id）装进实例里该类型的目录。
    ///
    /// 先在本地定位目标目录（实例不存在、存档不存在都在触网前报错），再取平台文件、校验文件名、
    /// 经批量下载池落盘（带 sha1 与大小契约，不符即重下换源）。
    ///
    /// CurseForge 文件拿不到下载地址时，设置 `download_restricted_curseforge_files` 开着就先拼 CDN 地址探测，
    /// 能下照常下载；拼不出或探测不能下，再用手动下载并认领过的副本，没有副本才报受限。
    ///
    /// 这之前先以「使用」方式核对游戏目录的数据格式记号：任务中心的重试与列实例被拒之后的旧列表都会不经
    /// 列实例直接调进来，记号比这一版新或读不懂时什么都不下载、不写。
    pub async fn install_resource(
        &self,
        target: ResourceTarget<'_>,
        platform: Platform,
        project_id: &str,
        file_version_id: &str,
        events: Option<&EventSink>,
    ) -> Result<ResourceInstallOutcome> {
        self.guard_game_dir(GuardMode::Use).await?;
        let (dir, landing) = self.resource_dir(target).await?;
        emit(
            events,
            CoreEvent::stage(format!(
                "准备从 {} 安装到 {}",
                platform.display_name(),
                target.version_id
            )),
        );
        let (file_name, dest) = match self
            .platform_file_task(platform, project_id, file_version_id, &dir, &landing)
            .await
        {
            Ok((file_name, task)) => {
                let dest = task.dest.clone();
                emit(events, CoreEvent::stage(format!("开始下载 {file_name}")));
                let report = self
                    .download_pool_reporting(events)
                    .download_all(vec![task], None)
                    .await?;
                if let Some(failure) = report.failures.into_iter().next() {
                    return Err(failure.error.into());
                }
                (file_name, dest)
            }
            Err(err) => {
                let (file, staged) = self.claimed_instead_of(err).await?;
                let dest = dir.join(&file.file_name);
                emit(
                    events,
                    CoreEvent::stage(format!("使用手动下载的 {}", file.file_name)),
                );
                place_claimed(&[ClaimedCopy {
                    staged,
                    dest: dest.clone(),
                }])
                .await?;
                (file.file_name, dest)
            }
        };
        emit(
            events,
            CoreEvent::stage(format!("{file_name} 已安装到 {}", dest.display())),
        );
        Ok(ResourceInstallOutcome {
            file_name,
            path: dest,
            platform,
            resource_type: target.resource_type,
            world: target.world.map(str::to_owned),
        })
    }

    /// 资源该落到的目录，以及它相对工作目录、正斜杠分隔的写法（受限文件的落点用）；数据包的存档在这里
    /// 校验存在。
    async fn resource_dir(&self, target: ResourceTarget<'_>) -> Result<(PathBuf, String)> {
        enum Dest<'a> {
            Sub(&'static str),
            World(&'a str),
        }
        let dest = match (target.resource_type, target.world) {
            (ResourceType::DataPack, Some(world)) => Dest::World(world),
            (ResourceType::DataPack, None) => {
                return Err(CoreError::InvalidResourceTarget(
                    "数据包要指明装进哪个存档".to_owned(),
                ));
            }
            (_, Some(_)) => {
                return Err(CoreError::InvalidResourceTarget(
                    "只有数据包需要指定存档".to_owned(),
                ));
            }
            (ResourceType::Mod, None) => {
                return Err(CoreError::InvalidResourceTarget(
                    "Mod 要经 install_mod 安装，不能作为单个资源装进实例".to_owned(),
                ));
            }
            (ResourceType::ResourcePack, None) => Dest::Sub("resourcepacks"),
            (ResourceType::Shader, None) => Dest::Sub("shaderpacks"),
            (other @ (ResourceType::Modpack | ResourceType::Plugin), None) => {
                return Err(CoreError::InvalidResourceTarget(format!(
                    "{other:?} 不能作为单个资源装进实例"
                )));
            }
        };
        if let Dest::World(world) = dest
            && !is_safe_file_name(world)
        {
            return Err(CoreError::InvalidResourceTarget(format!(
                "存档名不合法：{world}"
            )));
        }
        let working = self
            .resolve_working_dir(target.version_id)
            .await?
            .working_dir;
        let world = match dest {
            Dest::Sub(sub) => return Ok((working.join(sub), sub.to_owned())),
            Dest::World(world) => world,
        };
        let world_dir = working.join("saves").join(world);
        let is_dir = match tokio::fs::metadata(&world_dir).await {
            Ok(meta) => meta.is_dir(),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
            Err(source) => {
                return Err(petalfall_base::Error::Io {
                    path: world_dir,
                    source,
                }
                .into());
            }
        };
        if !is_dir {
            return Err(CoreError::WorldNotFound {
                version_id: target.version_id.to_owned(),
                world: world.to_owned(),
            });
        }
        Ok((
            world_dir.join("datapacks"),
            format!("saves/{world}/datapacks"),
        ))
    }

    /// 取平台上一个版本（文件）的主文件，生成落到 `dir` 下的下载任务。`landing` 是 `dir` 给用户看的
    /// 相对写法：文件受限时它是受限文件的落点目录。
    ///
    /// CurseForge 的地址依次取文件对象自带的、download-url 端点给的，这两种任务挂上下载凭据；都拿不到而
    /// 设置 `download_restricted_curseforge_files` 开着时，按文件编号与文件名拼出 CDN 地址并探测，能下就用
    /// 它建任务，照样带文件对象里的大小与 sha1，但不挂凭据（见 [`FallbackUrl`]）。仍拿不到时报
    /// [`CoreError::FileDistributionBlocked`]。设置关着时不拼地址、不发探测请求。
    pub(crate) async fn platform_file_task(
        &self,
        platform: Platform,
        project_id: &str,
        file_version_id: &str,
        dir: &Path,
        landing: &str,
    ) -> Result<(String, DownloadTask)> {
        let not_found = || CoreError::ModVersionNotFound {
            platform: platform.display_name(),
            project_id: project_id.to_owned(),
            version_id: file_version_id.to_owned(),
        };
        match platform {
            Platform::Modrinth => {
                let client = ModrinthClient::new(self.http()).with_base_url(self.modrinth_base());
                let version = client
                    .version(file_version_id)
                    .await?
                    .ok_or_else(not_found)?;
                let file = version.primary_file().ok_or_else(not_found)?;
                let file_name = checked_file_name(platform, &file.filename)?;
                let task = file.to_download_task(dir.join(&file_name));
                Ok((file_name, task))
            }
            Platform::CurseForge => {
                let invalid = || CoreError::InvalidProjectId {
                    platform: platform.display_name(),
                    project_id: format!("{project_id}/{file_version_id}"),
                };
                let mod_id: u32 = project_id.parse().map_err(|_| invalid())?;
                let file_id: u32 = file_version_id.parse().map_err(|_| invalid())?;
                let client = self.curseforge_client()?;
                let file = client
                    .mod_file(mod_id, file_id)
                    .await?
                    .ok_or_else(not_found)?;
                let file_name = checked_file_name(platform, &file.file_name)?;
                let dest = dir.join(&file_name);
                let credential = self.curseforge_credential(&client)?;
                let task = match file.to_download_task(&dest) {
                    Some(task) => task.with_credential(credential),
                    None => match curseforge_fallback_url(
                        &client,
                        mod_id,
                        file_id,
                        &file_name,
                        self.config().download_restricted_curseforge_files,
                    )
                    .await?
                    {
                        Some(url) => {
                            url.into_task(|url| task_for_file_url(url, &dest, &file), &credential)
                        }
                        None => {
                            let destination = join_landing(landing, &file_name);
                            return Err(single_blocked(&client, mod_id, |project| {
                                BlockedFile::new(project, &file, destination)
                            })
                            .await);
                        }
                    },
                };
                Ok((file_name, task))
            }
        }
    }
}

/// 文件对象没给下载地址时向 download-url 端点要。作者禁止第三方分发时该端点拒绝（403/404）或给空串，
/// 返回 `None`：调用方把它报成「需要到网页手动下载」，而不是笼统的请求失败。
async fn curseforge_download_url(
    client: &CurseForgeClient,
    mod_id: u32,
    file_id: u32,
) -> Result<Option<String>> {
    match client.file_download_url(mod_id, file_id).await {
        Ok(url) if !url.trim().is_empty() => Ok(Some(url)),
        Ok(_)
        | Err(petalfall_modplatform::Error::Status {
            status: 403 | 404, ..
        }) => Ok(None),
        Err(other) => Err(other.into()),
    }
}

/// 文件对象没给下载地址时补到的地址，按来源分两种：下载任务挂不挂 CDN 凭据取决于它。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FallbackUrl {
    /// download-url 端点给的。与文件对象自带的地址同等对待，下载任务照常挂 CDN 凭据。
    Endpoint(String),
    /// 平台不给地址，按文件编号与文件名拼出来、并且探测过能下的。作者没有把这个文件开放给第三方分发，
    /// 取它不该打着落樱那把 key 的名义：下载任务不挂凭据。装的是探测跟随跳转后的最终地址，不是拼出的那个：
    /// 拼出的地址在 edge.forgecdn.net 上，它对带 Range 的请求回 404、不跳转，整合包摘要的范围读取只能对最终地址发。
    Derived(String),
}

impl FallbackUrl {
    /// 用这个地址建下载任务：`build` 只管把地址换成任务，凭据由这里按来源决定挂不挂。
    pub(crate) fn into_task(
        self,
        build: impl FnOnce(String) -> DownloadTask,
        credential: &petalfall_download::DownloadCredential,
    ) -> DownloadTask {
        match self {
            Self::Endpoint(url) => build(url).with_credential(credential.clone()),
            Self::Derived(url) => build(url),
        }
    }
}

/// 文件对象没给下载地址时去哪要：先问 download-url 端点；端点也不给（403、404 或空串，作者禁止第三方
/// 分发时就是这样）而 `derive_restricted` 为真时，按文件编号与文件名拼出 CDN 地址，探测能下才用，用的是
/// 探测交回的跳转终点（见 [`FallbackUrl::Derived`]）。
///
/// 返回 `None` 表示拿不到任何能下的地址，调用方把它报成「需要到网页手动下载」。`derive_restricted` 来自
/// 配置里的 `download_restricted_curseforge_files`，默认关闭：关着时不拼地址、不发探测请求，行为与没有
/// 这个设置时一样。探测出错（网络不通、CDN 回 5xx）照常报错，不当成「不能下」：否则一次网络抖动就会
/// 让用户去手动下载本来能自动下的文件。
pub(crate) async fn curseforge_fallback_url(
    client: &CurseForgeClient,
    mod_id: u32,
    file_id: u32,
    file_name: &str,
    derive_restricted: bool,
) -> Result<Option<FallbackUrl>> {
    if let Some(url) = curseforge_download_url(client, mod_id, file_id).await? {
        return Ok(Some(FallbackUrl::Endpoint(url)));
    }
    if !derive_restricted {
        return Ok(None);
    }
    let Some(url) = client.derived_download_url(file_id, file_name) else {
        return Ok(None);
    };
    Ok(client
        .probe_derived_download(&url)
        .await?
        .map(FallbackUrl::Derived))
}

/// download-url 端点给的地址换成下载任务，带上文件对象里的大小与 sha1 契约。
pub(crate) fn task_for_file_url(
    url: String,
    dest: &Path,
    file: &petalfall_modplatform::CurseForgeFile,
) -> DownloadTask {
    let mut task = DownloadTask::new(url, dest);
    if let Some(size) = file.file_length {
        task = task.with_size(size);
    }
    if let Some(sha1) = file.sha1() {
        task = task.with_sha1(sha1.to_owned());
    }
    task
}

/// 相对目录与文件名拼成正斜杠分隔的落点；目录为空时就是文件名。
pub(crate) fn join_landing(landing: &str, file_name: &str) -> String {
    if landing.is_empty() {
        file_name.to_owned()
    } else {
        format!("{landing}/{file_name}")
    }
}

/// 平台给的文件名只能是单个普通文件名，否则拒收。
fn checked_file_name(platform: Platform, raw: &str) -> Result<String> {
    if is_safe_platform_file_name(raw) {
        Ok(raw.to_owned())
    } else {
        Err(CoreError::UnsafeFileName {
            platform: platform.display_name(),
            file_name: raw.to_owned(),
        })
    }
}

/// 是否为可以直接拼进路径的单个文件或目录名：非空、不是 `.` / `..`、不含路径分隔符、盘符冒号
/// 与控制字符、不以空格或点结尾（Windows 会悄悄去掉它们，落盘名与预期不一致）。
pub fn is_safe_file_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.ends_with([' ', '.'])
        && !name
            .chars()
            .any(|c| matches!(c, '/' | '\\' | ':') || c.is_control())
}

/// 平台（Modrinth、CurseForge）给的文件名能不能照原样落盘：[`is_safe_file_name`] 之外还不能是 8.3 短名的形状
/// （[`is_dos_short_name`]）。这些名字由工程作者定，文件落进 `mods/` 这类已有别的文件的目录，按短名写下去会盖掉
/// 短名恰好是它的兄弟文件。存档名、版本 id 不走这一条：它们是本机已有的名字，不是外来的落盘名。
pub(crate) fn is_safe_platform_file_name(name: &str) -> bool {
    is_safe_file_name(name) && !is_dos_short_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PetalfallConfig;
    use crate::data_format::testing::with_fixture;
    use crate::instances::tests::{NEWER_STAMP, NEWER_WRITER, disk_state, own_format, write_stamp};
    use petalfall_instance::IsolationPolicy;
    use petalfall_release::data_format::{StampFile, StampRead, parse_stamp};
    use sha1::{Digest, Sha1};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn put_version(mc: &Path, id: &str) {
        let dir = mc.join("versions").join(id);
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(
            dir.join(format!("{id}.json")),
            format!(r#"{{"id":"{id}","type":"release","mainClass":"m"}}"#),
        )
        .await
        .unwrap();
    }

    fn sha1_hex(bytes: &[u8]) -> String {
        Sha1::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    async fn mount_modrinth_version(server: &MockServer, id: &str, file: &str, bytes: &[u8]) {
        Mock::given(method("GET"))
            .and(path(format!("/version/{id}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": id, "project_id": "proj", "name": id, "version_number": "1.0",
                "version_type": "release", "date_published": "2026-01-01T00:00:00Z",
                "files": [{"hashes": {"sha1": sha1_hex(bytes)}, "url": format!("{}/{file}", server.uri()),
                           "filename": file, "primary": true, "size": bytes.len()}]
            })))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/{file}")))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes.to_vec()))
            .mount(server)
            .await;
    }

    fn petalfall(mc: &Path, policy: IsolationPolicy, base: &str) -> Petalfall {
        let mut petalfall = Petalfall::for_test(
            PetalfallConfig::default(),
            mc.to_path_buf(),
            mc.to_path_buf(),
        );
        petalfall.set_isolation_policy(policy);
        petalfall
            .with_modrinth_base(base)
            .with_curseforge_base(base)
    }

    /// 资源包落到隔离工作目录的 resourcepacks/，光影落到 shaderpacks/，内容与平台一致。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn resource_pack_and_shader_land_in_their_own_directories() {
        let server = MockServer::start().await;
        mount_modrinth_version(&server, "rp1", "Faithful.zip", b"resource-pack").await;
        mount_modrinth_version(&server, "sh1", "Complementary.zip", b"shader-pack").await;
        let tmp = tempfile::tempdir().unwrap();
        let mc = tmp.path();
        put_version(mc, "1.21.1").await;
        let petalfall = petalfall(mc, IsolationPolicy::All, &server.uri());

        let rp = petalfall
            .install_resource(
                ResourceTarget {
                    version_id: "1.21.1",
                    resource_type: ResourceType::ResourcePack,
                    world: None,
                },
                Platform::Modrinth,
                "faithful",
                "rp1",
                None,
            )
            .await
            .unwrap();
        let expected = mc.join("versions/1.21.1/resourcepacks/Faithful.zip");
        assert_eq!(rp.path, expected);
        assert_eq!(rp.resource_type, ResourceType::ResourcePack);
        assert_eq!(rp.world, None);
        assert_eq!(tokio::fs::read(&expected).await.unwrap(), b"resource-pack");

        let shader = petalfall
            .install_resource(
                ResourceTarget {
                    version_id: "1.21.1",
                    resource_type: ResourceType::Shader,
                    world: None,
                },
                Platform::Modrinth,
                "complementary",
                "sh1",
                None,
            )
            .await
            .unwrap();
        let expected = mc.join("versions/1.21.1/shaderpacks/Complementary.zip");
        assert_eq!(shader.path, expected);
        assert_eq!(tokio::fs::read(&expected).await.unwrap(), b"shader-pack");
    }

    /// 关闭隔离时资源落在 .minecraft 根下的同名目录。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shared_instance_uses_root_directories() {
        let server = MockServer::start().await;
        mount_modrinth_version(&server, "rp1", "Faithful.zip", b"rp").await;
        let tmp = tempfile::tempdir().unwrap();
        let mc = tmp.path();
        put_version(mc, "1.21.1").await;
        let petalfall = petalfall(mc, IsolationPolicy::Disabled, &server.uri());
        let rp = petalfall
            .install_resource(
                ResourceTarget {
                    version_id: "1.21.1",
                    resource_type: ResourceType::ResourcePack,
                    world: None,
                },
                Platform::Modrinth,
                "faithful",
                "rp1",
                None,
            )
            .await
            .unwrap();
        assert_eq!(rp.path, mc.join("resourcepacks/Faithful.zip"));
    }

    /// 数据包装进指定存档的 datapacks/；存档不存在、未指明存档、存档名越界都在触网前报错。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn data_pack_goes_into_the_named_world_and_bad_worlds_are_rejected() {
        let server = MockServer::start().await;
        mount_modrinth_version(&server, "dp1", "terralith.zip", b"datapack").await;
        let tmp = tempfile::tempdir().unwrap();
        let mc = tmp.path();
        put_version(mc, "1.21.1").await;
        tokio::fs::create_dir_all(mc.join("versions/1.21.1/saves/新的世界"))
            .await
            .unwrap();
        let petalfall = petalfall(mc, IsolationPolicy::All, &server.uri());
        let target = |world| ResourceTarget {
            version_id: "1.21.1",
            resource_type: ResourceType::DataPack,
            world,
        };

        let dp = petalfall
            .install_resource(
                target(Some("新的世界")),
                Platform::Modrinth,
                "terralith",
                "dp1",
                None,
            )
            .await
            .unwrap();
        let expected = mc.join("versions/1.21.1/saves/新的世界/datapacks/terralith.zip");
        assert_eq!(dp.path, expected);
        assert_eq!(dp.world.as_deref(), Some("新的世界"));
        assert_eq!(tokio::fs::read(&expected).await.unwrap(), b"datapack");

        let missing = petalfall
            .install_resource(
                target(Some("别的世界")),
                Platform::Modrinth,
                "terralith",
                "dp1",
                None,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&missing, CoreError::WorldNotFound { world, .. } if world == "别的世界"),
            "实际：{missing:?}"
        );
        for bad in ["..", "a/b", "..\\x", ""] {
            let err = petalfall
                .install_resource(
                    target(Some(bad)),
                    Platform::Modrinth,
                    "terralith",
                    "dp1",
                    None,
                )
                .await
                .unwrap_err();
            assert!(
                matches!(err, CoreError::InvalidResourceTarget(_)),
                "{bad:?}：{err:?}"
            );
        }
        let no_world = petalfall
            .install_resource(target(None), Platform::Modrinth, "terralith", "dp1", None)
            .await
            .unwrap_err();
        assert!(matches!(no_world, CoreError::InvalidResourceTarget(_)));
        // 越界的存档名没有在实例目录之外造出任何东西。
        assert!(!mc.join("versions/1.21.1/a").exists());
    }

    /// 平台给的文件名带路径分隔符时拒收，什么都不写。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn platform_file_name_with_traversal_is_rejected() {
        let server = MockServer::start().await;
        mount_modrinth_version(&server, "evil", "../../evil.zip", b"x").await;
        let tmp = tempfile::tempdir().unwrap();
        let mc = tmp.path();
        put_version(mc, "1.21.1").await;
        let petalfall = petalfall(mc, IsolationPolicy::All, &server.uri());
        let err = petalfall
            .install_resource(
                ResourceTarget {
                    version_id: "1.21.1",
                    resource_type: ResourceType::ResourcePack,
                    world: None,
                },
                Platform::Modrinth,
                "p",
                "evil",
                None,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&err, CoreError::UnsafeFileName { file_name, .. } if file_name == "../../evil.zip"),
            "实际：{err:?}"
        );
        assert!(!mc.join("evil.zip").exists());
        assert!(!mc.join("versions/evil.zip").exists());
    }

    /// 平台给的文件名是 8.3 短名的形状时同样拒收：按它写下去可能落到同目录里短名恰好是它的已有文件上。
    /// 目录里原有的文件不动。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn platform_file_name_shaped_like_a_short_name_is_rejected() {
        let server = MockServer::start().await;
        mount_modrinth_version(&server, "alias", "FAITHF~1.ZIP", b"x").await;
        let tmp = tempfile::tempdir().unwrap();
        let mc = tmp.path();
        put_version(mc, "1.21.1").await;
        let packs = mc.join("versions/1.21.1/resourcepacks");
        std::fs::create_dir_all(&packs).unwrap();
        std::fs::write(packs.join("faithful-32x.zip"), b"old").unwrap();
        let petalfall = petalfall(mc, IsolationPolicy::All, &server.uri());
        let err = petalfall
            .install_resource(
                ResourceTarget {
                    version_id: "1.21.1",
                    resource_type: ResourceType::ResourcePack,
                    world: None,
                },
                Platform::Modrinth,
                "p",
                "alias",
                None,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&err, CoreError::UnsafeFileName { file_name, .. } if file_name == "FAITHF~1.ZIP"),
            "实际：{err:?}"
        );
        assert_eq!(
            std::fs::read(packs.join("faithful-32x.zip")).unwrap(),
            b"old"
        );
        let names: Vec<_> = std::fs::read_dir(&packs)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["faithful-32x.zip"]);
    }

    /// Mod 也不行：直接下到 mods/ 会绕过 Mod 管理，同名 jar 被无备份地覆盖。目录里原有的文件不动。
    #[tokio::test]
    async fn modpack_plugin_and_mod_are_not_single_resources() {
        let tmp = tempfile::tempdir().unwrap();
        let mc = tmp.path();
        put_version(mc, "1.21.1").await;
        let mods = mc.join("versions/1.21.1/mods");
        tokio::fs::create_dir_all(&mods).await.unwrap();
        tokio::fs::write(mods.join("sodium.jar"), b"player-patched")
            .await
            .unwrap();
        let petalfall = petalfall(mc, IsolationPolicy::All, "http://127.0.0.1:9");
        for resource_type in [
            ResourceType::Modpack,
            ResourceType::Plugin,
            ResourceType::Mod,
        ] {
            let err = petalfall
                .install_resource(
                    ResourceTarget {
                        version_id: "1.21.1",
                        resource_type,
                        world: None,
                    },
                    Platform::Modrinth,
                    "p",
                    "v",
                    None,
                )
                .await
                .unwrap_err();
            assert!(
                matches!(err, CoreError::InvalidResourceTarget(_)),
                "{resource_type:?}：{err:?}"
            );
        }
        assert_eq!(
            tokio::fs::read(mods.join("sodium.jar")).await.unwrap(),
            b"player-patched"
        );
    }

    /// CurseForge 资源包没有下载地址、download-url 端点也拒绝时，报成需要手动下载：多问一次工程拼出网页链接，
    /// 落点是这次安装的 resourcepacks/，哈希与大小照文件对象给；什么都不下载。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn curseforge_blocked_resource_is_reported_with_its_page_and_landing() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/mods/123/files/456"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": {
                    "id": 456, "modId": 123, "displayName": "Pack", "fileName": "pack.zip",
                    "hashes": [{"value": "0f4a890d07402280686a518579fa9fa02309e315", "algo": 1}],
                    "downloadUrl": null, "fileLength": 10, "fileFingerprint": 778
                }})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/mods/123/files/456/download-url"))
            .respond_with(ResponseTemplate::new(403))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/mods/123"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": {
                    "id": 123, "name": "Pack", "slug": "pack", "summary": "", "classId": 12,
                    "links": {"websiteUrl": "https://www.curseforge.com/minecraft/texture-packs/pack/"}
                }})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let tmp = tempfile::tempdir().unwrap();
        let mc = tmp.path();
        put_version(mc, "1.21.1").await;
        let petalfall = petalfall(mc, IsolationPolicy::All, &server.uri()).with_curseforge_key("k");

        let err = petalfall
            .install_resource(
                ResourceTarget {
                    version_id: "1.21.1",
                    resource_type: ResourceType::ResourcePack,
                    world: None,
                },
                Platform::CurseForge,
                "123",
                "456",
                None,
            )
            .await
            .unwrap_err();
        let CoreError::FileDistributionBlocked { files } = &err else {
            panic!("实际：{err:?}");
        };
        assert_eq!(
            files,
            &[BlockedFile {
                project_id: 123,
                file_id: 456,
                file_name: "pack.zip".into(),
                web_url: "https://www.curseforge.com/minecraft/texture-packs/pack/files/456".into(),
                destination: "resourcepacks/pack.zip".into(),
                sha1: Some("0f4a890d07402280686a518579fa9fa02309e315".into()),
                fingerprint: 778,
                size: Some(10),
            }]
        );
        assert_eq!(
            err.to_string(),
            "这些文件的作者不允许启动器直接下载，需要到 CurseForge 网页手动下载：pack.zip"
        );
        assert!(!mc.join("versions/1.21.1/resourcepacks/pack.zip").exists());
        server.verify().await;
    }

    /// 受限资源包手动下载并认领之后再装一次：不再报受限，暂存位的副本复制进 resourcepacks/，覆盖同名旧文件，
    /// 暂存位留着副本。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn blocked_resource_installs_from_the_claimed_copy() {
        let bytes: &[u8] = b"claimed-pack";
        let sha1: String = Sha1::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/mods/123/files/456"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": {
                    "id": 456, "modId": 123, "displayName": "Pack", "fileName": "pack.zip",
                    "hashes": [{"value": sha1, "algo": 1}],
                    "downloadUrl": null, "fileLength": bytes.len(), "fileFingerprint": 778
                }})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/mods/123/files/456/download-url"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/mods/123"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": {
                    "id": 123, "name": "Pack", "slug": "pack", "summary": "", "classId": 12,
                    "links": {"websiteUrl": "https://www.curseforge.com/minecraft/texture-packs/pack/"}
                }})),
            )
            .mount(&server)
            .await;
        let tmp = tempfile::tempdir().unwrap();
        let mc = tmp.path();
        put_version(mc, "1.21.1").await;
        let petalfall = petalfall(mc, IsolationPolicy::All, &server.uri()).with_curseforge_key("k");
        let target = ResourceTarget {
            version_id: "1.21.1",
            resource_type: ResourceType::ResourcePack,
            world: None,
        };

        let err = petalfall
            .install_resource(target, Platform::CurseForge, "123", "456", None)
            .await
            .unwrap_err();
        let CoreError::FileDistributionBlocked { files } = err else {
            panic!("没认领时应报受限：{err:?}");
        };
        let downloads = mc.join("Downloads");
        tokio::fs::create_dir_all(&downloads).await.unwrap();
        tokio::fs::write(downloads.join("pack (1).zip"), bytes)
            .await
            .unwrap();
        let report = petalfall
            .claim_blocked_files(&downloads, &files)
            .await
            .unwrap();
        assert!(report.missing.is_empty(), "{report:?}");
        let landing = mc.join("versions/1.21.1/resourcepacks");
        tokio::fs::create_dir_all(&landing).await.unwrap();
        tokio::fs::write(landing.join("pack.zip"), b"old")
            .await
            .unwrap();

        let outcome = petalfall
            .install_resource(target, Platform::CurseForge, "123", "456", None)
            .await
            .unwrap();

        assert_eq!(outcome.file_name, "pack.zip");
        assert_eq!(outcome.path, landing.join("pack.zip"));
        assert_eq!(outcome.platform, Platform::CurseForge);
        assert_eq!(tokio::fs::read(&outcome.path).await.unwrap(), bytes);
        let mut names: Vec<String> = Vec::new();
        let mut entries = tokio::fs::read_dir(&landing).await.unwrap();
        while let Some(entry) = entries.next_entry().await.unwrap() {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
        assert_eq!(names, vec!["pack.zip"]);
        assert!(
            petalfall
                .blocked_claims_dir()
                .join("curseforge-456/pack.zip")
                .is_file()
        );
    }

    /// 文件对象自带地址与 download-url 端点兜底出来的 CurseForge 任务挂 CurseForge 凭据，Modrinth 的任务
    /// 不挂（按文件编号拼出来的地址也不挂，见后面拼地址的用例）。数据包的落点写法带上存档。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn curseforge_tasks_carry_the_download_credential_and_modrinth_tasks_do_not() {
        let server = MockServer::start().await;
        let uri = server.uri();
        mount_modrinth_version(&server, "rp1", "Faithful.zip", b"rp").await;
        Mock::given(method("GET"))
            .and(path("/v1/mods/7/files/70"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": {
                    "id": 70, "modId": 7, "displayName": "d", "fileName": "direct.zip",
                    "downloadUrl": format!("{uri}/cdn/direct.zip"), "fileLength": 2
                }})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/mods/8/files/80"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": {
                    "id": 80, "modId": 8, "displayName": "f", "fileName": "fallback.zip",
                    "downloadUrl": null, "fileLength": 3
                }})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/mods/8/files/80/download-url"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"data": format!("{uri}/cdn/fallback.zip")})),
            )
            .mount(&server)
            .await;
        let tmp = tempfile::tempdir().unwrap();
        let petalfall = petalfall(tmp.path(), IsolationPolicy::All, &uri).with_curseforge_key("k");
        let dir = tmp.path().join("rp");

        let (_, modrinth) = petalfall
            .platform_file_task(Platform::Modrinth, "faithful", "rp1", &dir, "resourcepacks")
            .await
            .unwrap();
        assert!(modrinth.credential.is_none());
        for (project, file, url) in [
            ("7", "70", format!("{uri}/cdn/direct.zip")),
            ("8", "80", format!("{uri}/cdn/fallback.zip")),
        ] {
            let (_, task) = petalfall
                .platform_file_task(Platform::CurseForge, project, file, &dir, "resourcepacks")
                .await
                .unwrap();
            assert_eq!(task.url, url);
            let credential = task.credential.as_ref().expect("CurseForge 任务应带凭据");
            assert_eq!(
                credential.realm(),
                petalfall_download::CredentialRealm::CurseForge
            );
            assert_eq!(credential.header_name().as_str(), "x-api-key");
        }
        assert_eq!(
            join_landing("saves/新的世界/datapacks", "a.zip"),
            "saves/新的世界/datapacks/a.zip"
        );
        assert_eq!(join_landing("", "a.zip"), "a.zip");
    }

    // ---- 拿不到下载地址时按设置拼出 CDN 地址 ----

    fn derived_path(file_id: u32, file_name: &str) -> String {
        format!("/files/{}/{}/{file_name}", file_id / 1000, file_id % 1000)
    }

    /// 作者关掉第三方分发的一个 CurseForge 资源包。编号随机且不小于 1000（拼得出 CDN 地址），内容随机、
    /// 长度从 1 字节起。
    struct Restricted {
        file_id: u32,
        bytes: Vec<u8>,
        /// 按编号与文件名拼出的 CDN 路径。
        cdn_path: String,
    }

    impl Restricted {
        fn new() -> Self {
            let file_id = fastrand::u32(1000..);
            let mut bytes = vec![0u8; fastrand::usize(1..=4096)];
            fastrand::fill(&mut bytes);
            Self {
                file_id,
                bytes,
                cdn_path: derived_path(file_id, "pack.zip"),
            }
        }

        fn version_id(&self) -> String {
            self.file_id.to_string()
        }

        /// 报受限时该给出的那一条：与设置无关。
        fn blocked(&self) -> BlockedFile {
            BlockedFile {
                project_id: 123,
                file_id: self.file_id,
                file_name: "pack.zip".into(),
                web_url: format!(
                    "https://www.curseforge.com/minecraft/texture-packs/pack/files/{}",
                    self.file_id
                ),
                destination: "resourcepacks/pack.zip".into(),
                sha1: Some(sha1_hex(&self.bytes)),
                fingerprint: 778,
                size: Some(self.bytes.len() as u64),
            }
        }
    }

    const PACK: ResourceTarget<'static> = ResourceTarget {
        version_id: "1.21.1",
        resource_type: ResourceType::ResourcePack,
        world: None,
    };

    /// 文件对象不给地址、download-url 端点 403，工程给网页链接（报受限时拼链接用）。
    async fn mount_restricted(server: &MockServer, file: &Restricted) {
        Mock::given(method("GET"))
            .and(path(format!("/v1/mods/123/files/{}", file.file_id)))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": {
                    "id": file.file_id, "modId": 123, "displayName": "Pack", "fileName": "pack.zip",
                    "hashes": [{"value": sha1_hex(&file.bytes), "algo": 1}],
                    "downloadUrl": null, "fileLength": file.bytes.len(), "fileFingerprint": 778
                }})),
            )
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/v1/mods/123/files/{}/download-url",
                file.file_id
            )))
            .respond_with(ResponseTemplate::new(403))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/mods/123"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": {
                    "id": 123, "name": "Pack", "slug": "pack", "summary": "", "classId": 12,
                    "links": {"websiteUrl": "https://www.curseforge.com/minecraft/texture-packs/pack/"}
                }})),
            )
            .mount(server)
            .await;
    }

    /// 拼出的地址上的探测（HEAD，回 `head`）与下载（GET，回文件内容），各该收到多少次。
    async fn mount_derived(
        server: &MockServer,
        file: &Restricted,
        head: u16,
        heads: impl Into<wiremock::Times>,
        gets: u64,
    ) {
        Mock::given(method("HEAD"))
            .and(path(file.cdn_path.as_str()))
            .respond_with(ResponseTemplate::new(head))
            .expect(heads)
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path(file.cdn_path.as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(file.bytes.clone()))
            .expect(gets)
            .mount(server)
            .await;
    }

    /// CurseForge API 与拼地址用的 CDN 根都指到 `server`。凭据作用域也指到它：任务误挂了凭据，x-api-key
    /// 就会真的发到拼出的地址上，测试才抓得到。
    fn restricted_petalfall(mc: &Path, server: &MockServer, enabled: bool) -> Petalfall {
        let port = server.address().port();
        let mut petalfall = petalfall(mc, IsolationPolicy::All, &server.uri())
            .with_curseforge_key("k")
            .with_curseforge_cdn_scope(petalfall_download::CredentialScope::new(
                [format!("127.0.0.1:{port}")],
                false,
            ))
            .with_curseforge_cdn_base(server.uri());
        petalfall.set_download_restricted_curseforge_files(enabled);
        petalfall
    }

    /// 拼出的地址收到的请求，按到达顺序：方法，以及带没带 x-api-key。
    async fn derived_requests(server: &MockServer, file: &Restricted) -> Vec<(String, bool)> {
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|request| request.url.path() == file.cdn_path)
            .map(|request| {
                (
                    request.method.as_str().to_owned(),
                    request.headers.contains_key("x-api-key"),
                )
            })
            .collect()
    }

    /// 设置关着：照旧报受限，清单照文件对象给；拼出的地址既没被探测也没被下载。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn restricted_resource_with_the_setting_off_never_touches_the_derived_url() {
        let server = MockServer::start().await;
        let file = Restricted::new();
        mount_restricted(&server, &file).await;
        mount_derived(&server, &file, 200, 0, 0).await;
        let tmp = tempfile::tempdir().unwrap();
        let mc = tmp.path();
        put_version(mc, "1.21.1").await;
        let petalfall = restricted_petalfall(mc, &server, false);

        let err = petalfall
            .install_resource(PACK, Platform::CurseForge, "123", &file.version_id(), None)
            .await
            .unwrap_err();

        let CoreError::FileDistributionBlocked { files } = &err else {
            panic!("实际：{err:?}");
        };
        assert_eq!(files, &[file.blocked()]);
        assert!(!mc.join("versions/1.21.1/resourcepacks/pack.zip").exists());
        server.verify().await;
        assert_eq!(
            derived_requests(&server, &file).await,
            Vec::<(String, bool)>::new()
        );
    }

    /// 设置开着、探测 200：不报受限，按拼出的地址下到 resourcepacks/，内容与平台一致（大小与 sha1 照文件
    /// 对象校验）。凭据作用域覆盖这个地址，下载那一跳仍不带 x-api-key：拼出的地址不挂凭据。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn restricted_resource_with_the_setting_on_downloads_from_the_derived_url_without_the_key()
     {
        let server = MockServer::start().await;
        let file = Restricted::new();
        mount_restricted(&server, &file).await;
        mount_derived(&server, &file, 200, 1, 1).await;
        let tmp = tempfile::tempdir().unwrap();
        let mc = tmp.path();
        put_version(mc, "1.21.1").await;
        let petalfall = restricted_petalfall(mc, &server, true);

        let outcome = petalfall
            .install_resource(PACK, Platform::CurseForge, "123", &file.version_id(), None)
            .await
            .unwrap();

        let expected = mc.join("versions/1.21.1/resourcepacks/pack.zip");
        assert_eq!(outcome.file_name, "pack.zip");
        assert_eq!(outcome.path, expected);
        assert_eq!(outcome.platform, Platform::CurseForge);
        assert_eq!(tokio::fs::read(&expected).await.unwrap(), file.bytes);
        server.verify().await;
        assert_eq!(
            derived_requests(&server, &file).await,
            vec![("HEAD".to_owned(), false), ("GET".to_owned(), false)]
        );
    }

    /// 拼出的地址建的任务带上文件对象里的大小与 sha1，下载后照样校验；凭据不挂。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn derived_task_keeps_the_size_and_sha1_but_not_the_credential() {
        let server = MockServer::start().await;
        let file = Restricted::new();
        mount_restricted(&server, &file).await;
        mount_derived(&server, &file, 200, 1, 0).await;
        let tmp = tempfile::tempdir().unwrap();
        let petalfall = restricted_petalfall(tmp.path(), &server, true);
        let dir = tmp.path().join("rp");

        let (file_name, task) = petalfall
            .platform_file_task(
                Platform::CurseForge,
                "123",
                &file.version_id(),
                &dir,
                "resourcepacks",
            )
            .await
            .unwrap();

        assert_eq!(file_name, "pack.zip");
        assert_eq!(
            task,
            DownloadTask {
                url: format!("{}{}", server.uri(), file.cdn_path),
                dest: dir.join("pack.zip"),
                sha1: Some(sha1_hex(&file.bytes)),
                size: Some(file.bytes.len() as u64),
                credential: None,
            }
        );
        server.verify().await;
    }

    /// 设置开着、探测回 404 或 410：仍报受限，清单与设置关着时一字不差；拼出的地址只被探测、没被下载。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn restricted_resource_the_cdn_refuses_is_still_reported_blocked() {
        for status in [404u16, 410] {
            let server = MockServer::start().await;
            let file = Restricted::new();
            mount_restricted(&server, &file).await;
            mount_derived(&server, &file, status, 1, 0).await;
            let tmp = tempfile::tempdir().unwrap();
            let mc = tmp.path();
            put_version(mc, "1.21.1").await;
            let mut petalfall = restricted_petalfall(mc, &server, false);
            let off = petalfall
                .install_resource(PACK, Platform::CurseForge, "123", &file.version_id(), None)
                .await
                .unwrap_err();
            petalfall.set_download_restricted_curseforge_files(true);

            let on = petalfall
                .install_resource(PACK, Platform::CurseForge, "123", &file.version_id(), None)
                .await
                .unwrap_err();

            let (
                CoreError::FileDistributionBlocked { files: off_files },
                CoreError::FileDistributionBlocked { files: on_files },
            ) = (&off, &on)
            else {
                panic!("{status}：关着 {off:?}，开着 {on:?}");
            };
            assert_eq!(on_files, off_files, "{status}");
            assert_eq!(on_files, &[file.blocked()], "{status}");
            assert_eq!(
                on.to_string(),
                "这些文件的作者不允许启动器直接下载，需要到 CurseForge 网页手动下载：pack.zip",
                "{status}"
            );
            assert!(
                !mc.join("versions/1.21.1/resourcepacks/pack.zip").exists(),
                "{status}"
            );
            server.verify().await;
            assert_eq!(
                derived_requests(&server, &file).await,
                vec![("HEAD".to_owned(), false)],
                "{status}"
            );
        }
    }

    /// 设置开着、探测回 500：报探测的错而不是受限（一次网络抖动不该把能自动下的文件推给用户手动下），
    /// 什么都不下载。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn restricted_resource_probe_failure_is_an_error_not_blocked() {
        let server = MockServer::start().await;
        let file = Restricted::new();
        mount_restricted(&server, &file).await;
        mount_derived(&server, &file, 500, 1.., 0).await;
        let tmp = tempfile::tempdir().unwrap();
        let mc = tmp.path();
        put_version(mc, "1.21.1").await;
        let petalfall = restricted_petalfall(mc, &server, true);

        let err = petalfall
            .install_resource(PACK, Platform::CurseForge, "123", &file.version_id(), None)
            .await
            .unwrap_err();

        let derived = format!("{}{}", server.uri(), file.cdn_path);
        assert!(
            matches!(
                &err,
                CoreError::ModPlatform(petalfall_modplatform::Error::Status { url, status: 500 })
                    if url == &derived
            ),
            "实际：{err:?}"
        );
        assert!(!mc.join("versions/1.21.1/resourcepacks/pack.zip").exists());
        server.verify().await;
    }

    /// 认领过副本之后打开设置、拼出的地址能下：按地址下载，不用认领的副本（阶段是「开始下载」而不是
    /// 「使用手动下载的」，拼出的地址被 GET 了一次）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn derived_download_comes_before_the_claimed_copy() {
        let server = MockServer::start().await;
        let file = Restricted::new();
        mount_restricted(&server, &file).await;
        mount_derived(&server, &file, 200, 1, 1).await;
        let tmp = tempfile::tempdir().unwrap();
        let mc = tmp.path();
        put_version(mc, "1.21.1").await;
        let mut petalfall = restricted_petalfall(mc, &server, false);
        let err = petalfall
            .install_resource(PACK, Platform::CurseForge, "123", &file.version_id(), None)
            .await
            .unwrap_err();
        let CoreError::FileDistributionBlocked { files } = err else {
            panic!("设置关着、没认领时应报受限：{err:?}");
        };
        let downloads = mc.join("Downloads");
        tokio::fs::create_dir_all(&downloads).await.unwrap();
        tokio::fs::write(downloads.join("pack (1).zip"), &file.bytes)
            .await
            .unwrap();
        let report = petalfall
            .claim_blocked_files(&downloads, &files)
            .await
            .unwrap();
        assert!(report.missing.is_empty(), "{report:?}");
        petalfall.set_download_restricted_curseforge_files(true);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        let outcome = petalfall
            .install_resource(
                PACK,
                Platform::CurseForge,
                "123",
                &file.version_id(),
                Some(&tx),
            )
            .await
            .unwrap();

        let mut stages = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if let CoreEvent::Stage(stage) = event {
                stages.push(stage);
            }
        }
        assert!(
            stages.iter().any(|stage| stage == "开始下载 pack.zip"),
            "{stages:?}"
        );
        assert!(
            !stages
                .iter()
                .any(|stage| stage.starts_with("使用手动下载的")),
            "{stages:?}"
        );
        assert_eq!(
            outcome.path,
            mc.join("versions/1.21.1/resourcepacks/pack.zip")
        );
        assert_eq!(tokio::fs::read(&outcome.path).await.unwrap(), file.bytes);
        server.verify().await;
        assert_eq!(
            derived_requests(&server, &file).await,
            vec![("HEAD".to_owned(), false), ("GET".to_owned(), false)]
        );
    }

    // ---- curseforge_fallback_url ----

    /// 直接对着 `server` 的客户端：API 与拼地址用的 CDN 根都指到它。最多试两次且几乎不等，探测出错的用例
    /// 不必等满默认退避。
    fn fallback_client(server: &MockServer, mc: &Path) -> CurseForgeClient {
        petalfall(mc, IsolationPolicy::All, &server.uri())
            .with_curseforge_key("k")
            .with_curseforge_cdn_base(server.uri())
            .curseforge_client()
            .unwrap()
            .with_retry(petalfall_base::retry::RetryPolicy {
                max_attempts: 2,
                initial_delay: std::time::Duration::from_millis(1),
                max_delay: std::time::Duration::from_millis(1),
                multiplier: 1.0,
                jitter: false,
            })
    }

    /// download-url 端点对 `file_id` 回 `response`，且只该被问一次。
    async fn mount_endpoint(server: &MockServer, file_id: u32, response: ResponseTemplate) {
        Mock::given(method("GET"))
            .and(path(format!("/v1/mods/123/files/{file_id}/download-url")))
            .respond_with(response)
            .expect(1)
            .mount(server)
            .await;
    }

    /// `server` 收到的 HEAD（探测）请求路径。
    async fn probes(server: &MockServer) -> Vec<String> {
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|request| request.method.as_str() == "HEAD")
            .map(|request| request.url.path().to_owned())
            .collect()
    }

    /// 端点给了地址就用它，标成 Endpoint；设置开不开都不拼地址、不探测。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fallback_url_takes_the_endpoint_url_first() {
        for derive in [false, true] {
            let server = MockServer::start().await;
            let file_id = fastrand::u32(1000..);
            let given = format!("{}/endpoint/pack.zip", server.uri());
            mount_endpoint(
                &server,
                file_id,
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": given})),
            )
            .await;
            let tmp = tempfile::tempdir().unwrap();
            let client = fallback_client(&server, tmp.path());

            let got = curseforge_fallback_url(&client, 123, file_id, "pack.zip", derive)
                .await
                .unwrap();

            assert_eq!(got, Some(FallbackUrl::Endpoint(given)), "{derive}");
            assert_eq!(probes(&server).await, Vec::<String>::new(), "{derive}");
            server.verify().await;
        }
    }

    /// 端点 403 而设置关着：返回 None，拼出的地址一次都没被请求。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fallback_url_with_the_setting_off_stops_at_the_endpoint() {
        let server = MockServer::start().await;
        let file_id = fastrand::u32(1000..);
        mount_endpoint(&server, file_id, ResponseTemplate::new(403)).await;
        Mock::given(path(derived_path(file_id, "pack.zip")))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        let tmp = tempfile::tempdir().unwrap();
        let client = fallback_client(&server, tmp.path());

        let got = curseforge_fallback_url(&client, 123, file_id, "pack.zip", false)
            .await
            .unwrap();

        assert_eq!(got, None);
        assert_eq!(probes(&server).await, Vec::<String>::new());
        server.verify().await;
    }

    /// 端点拒绝（403、404、空串）而设置开着、探测 200：返回 Derived，地址以注入的 CDN 根开头，路径按编号
    /// 整除 1000 与余数分两段。编号取 1000（余数为 0 的下界）与随机值。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fallback_url_derives_and_probes_when_the_endpoint_refuses() {
        let refusals: [fn() -> ResponseTemplate; 3] = [
            || ResponseTemplate::new(403),
            || ResponseTemplate::new(404),
            || ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": ""})),
        ];
        for (case, refusal) in refusals.into_iter().enumerate() {
            for file_id in [1000, fastrand::u32(1000..)] {
                let server = MockServer::start().await;
                mount_endpoint(&server, file_id, refusal()).await;
                let cdn_path = derived_path(file_id, "pack.zip");
                Mock::given(method("HEAD"))
                    .and(path(cdn_path.as_str()))
                    .respond_with(ResponseTemplate::new(200))
                    .expect(1)
                    .mount(&server)
                    .await;
                let tmp = tempfile::tempdir().unwrap();
                let client = fallback_client(&server, tmp.path());

                let got = curseforge_fallback_url(&client, 123, file_id, "pack.zip", true)
                    .await
                    .unwrap();

                let Some(FallbackUrl::Derived(url)) = got else {
                    panic!("第 {case} 种拒绝、编号 {file_id}：{got:?}");
                };
                assert!(
                    url.starts_with(&format!("{}/files/", server.uri())),
                    "{url}"
                );
                assert_eq!(url, format!("{}{cdn_path}", server.uri()));
                server.verify().await;
            }
        }
        assert_eq!(derived_path(1000, "a.jar"), "/files/1/0/a.jar");
    }

    /// 探测经 302 落到别的路径（线上 edge.forgecdn.net 跳到 mediafilez.forgecdn.net）：Derived 装的是跳转终点，
    /// 之后的下载与范围读取都对它发，不再碰对带 Range 的请求回 404 的拼出地址。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fallback_url_carries_the_redirect_target_of_the_probe() {
        let server = MockServer::start().await;
        let file_id = fastrand::u32(1000..);
        mount_endpoint(&server, file_id, ResponseTemplate::new(403)).await;
        let cdn_path = derived_path(file_id, "pack.zip");
        let media = format!("/mediafilez{cdn_path}");
        Mock::given(method("HEAD"))
            .and(path(cdn_path.as_str()))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("location", format!("{}{media}", server.uri()).as_str()),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("HEAD"))
            .and(path(media.as_str()))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        let tmp = tempfile::tempdir().unwrap();
        let client = fallback_client(&server, tmp.path());

        let got = curseforge_fallback_url(&client, 123, file_id, "pack.zip", true)
            .await
            .unwrap();

        assert_eq!(
            got,
            Some(FallbackUrl::Derived(format!("{}{media}", server.uri())))
        );
        assert_eq!(probes(&server).await, vec![cdn_path, media]);
        server.verify().await;
    }

    /// 探测回 401、403、404、410：CDN 不给，返回 None 而不是报错。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fallback_url_is_none_when_the_probe_says_no() {
        for status in [401u16, 403, 404, 410] {
            let server = MockServer::start().await;
            let file_id = fastrand::u32(1000..);
            mount_endpoint(&server, file_id, ResponseTemplate::new(403)).await;
            Mock::given(method("HEAD"))
                .and(path(derived_path(file_id, "pack.zip")))
                .respond_with(ResponseTemplate::new(status))
                .expect(1)
                .mount(&server)
                .await;
            let tmp = tempfile::tempdir().unwrap();
            let client = fallback_client(&server, tmp.path());

            let got = curseforge_fallback_url(&client, 123, file_id, "pack.zip", true)
                .await
                .unwrap();

            assert_eq!(got, None, "{status}");
            server.verify().await;
        }
    }

    /// 探测回 500：报错，错里是拼出的地址与状态码，不当成「不能下」。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fallback_url_probe_failure_is_an_error() {
        let server = MockServer::start().await;
        let file_id = fastrand::u32(1000..);
        mount_endpoint(&server, file_id, ResponseTemplate::new(403)).await;
        let cdn_path = derived_path(file_id, "pack.zip");
        Mock::given(method("HEAD"))
            .and(path(cdn_path.as_str()))
            .respond_with(ResponseTemplate::new(500))
            .expect(1..)
            .mount(&server)
            .await;
        let tmp = tempfile::tempdir().unwrap();
        let client = fallback_client(&server, tmp.path());

        let err = curseforge_fallback_url(&client, 123, file_id, "pack.zip", true)
            .await
            .unwrap_err();

        let derived = format!("{}{cdn_path}", server.uri());
        assert!(
            matches!(
                &err,
                CoreError::ModPlatform(petalfall_modplatform::Error::Status { url, status: 500 })
                    if url == &derived
            ),
            "实际：{err:?}"
        );
        server.verify().await;
    }

    /// 拼不出地址（文件名带路径分隔符或是 `..`，编号小于 1000）：返回 None，不发探测。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fallback_url_is_none_without_probing_when_nothing_can_be_derived() {
        for (file_id, file_name) in [
            (fastrand::u32(1000..), "mods/pack.zip"),
            (fastrand::u32(1000..), "mods\\pack.zip"),
            (fastrand::u32(1000..), ".."),
            (999, "pack.zip"),
        ] {
            let server = MockServer::start().await;
            mount_endpoint(&server, file_id, ResponseTemplate::new(403)).await;
            let tmp = tempfile::tempdir().unwrap();
            let client = fallback_client(&server, tmp.path());

            let got = curseforge_fallback_url(&client, 123, file_id, file_name, true)
                .await
                .unwrap();

            assert_eq!(got, None, "{file_id} {file_name:?}");
            assert_eq!(
                probes(&server).await,
                Vec::<String>::new(),
                "{file_id} {file_name:?}"
            );
            server.verify().await;
        }
    }

    #[test]
    fn safe_file_names_exclude_traversal_separators_and_trailing_dots() {
        for ok in ["sodium-0.6.jar", "新的世界", "My World (2)", ".hidden"] {
            assert!(is_safe_file_name(ok), "{ok}");
        }
        for bad in ["", ".", "..", "a/b", "a\\b", "C:x", "x.", "x ", "a\u{0}b"] {
            assert!(!is_safe_file_name(bad), "{bad:?}");
        }
    }

    /// 平台给的文件名另拒 8.3 短名的形状，形状对上的真实长文件名（`foo~1.jar`）同样拒收，这是代价；带「~」而
    /// 形状不对的照收。存档名、版本 id 走的 is_safe_file_name 不受影响。
    #[test]
    fn platform_file_names_also_refuse_dos_short_name_shapes() {
        for alias in [
            "SODIUM~1.JAR",
            "sodium~1.jar",
            "foo~1.jar",
            "资源包~2.zip",
            "~WEEKE~1.JSO",
            "LONGFI~12",
        ] {
            assert!(!is_safe_platform_file_name(alias), "{alias}");
            assert!(is_safe_file_name(alias), "{alias}");
        }
        for plain in [
            "sodium-0.6.jar",
            "~backup.jar",
            "a~b.jar",
            "abcdefg~1.jar",
            "notes~draft~2026.txt",
            "foo~.jar",
            "foo~1a.jar",
        ] {
            assert!(is_safe_platform_file_name(plain), "{plain}");
        }
        assert!(!is_safe_platform_file_name("../evil.jar"));
    }

    /// 记号比这一版新或读不懂时，资源在定位目录、取平台文件之前就被挡下：带出记号的结论与游戏目录，游戏
    /// 目录里一个文件都没被改、被碰或多出来，平台一个请求都没收到。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn install_resource_refuses_a_rejected_game_dir_before_asking_the_platform() {
        let server = MockServer::start().await;
        mount_modrinth_version(&server, "rp1", "Faithful.zip", b"resource-pack").await;
        mount_modrinth_version(&server, "dp1", "terralith.zip", b"datapack").await;
        let tmp = tempfile::tempdir().unwrap();
        let mc = tmp.path();
        put_version(mc, "1.21.1").await;
        tokio::fs::create_dir_all(mc.join("versions/1.21.1/saves/新的世界"))
            .await
            .unwrap();
        let petalfall = petalfall(mc, IsolationPolicy::All, &server.uri());
        let game = petalfall.game_dir().to_path_buf();
        // 关卡缺席时两项都装得上：资源包进 resourcepacks/，数据包进那个存档的 datapacks/。
        let installs = [
            (
                ResourceTarget {
                    version_id: "1.21.1",
                    resource_type: ResourceType::ResourcePack,
                    world: None,
                },
                "rp1",
            ),
            (
                ResourceTarget {
                    version_id: "1.21.1",
                    resource_type: ResourceType::DataPack,
                    world: Some("新的世界"),
                },
                "dp1",
            ),
        ];

        write_stamp(&game, NEWER_STAMP);
        let before = disk_state(&game);
        for (target, file_version_id) in installs {
            let install =
                petalfall.install_resource(target, Platform::Modrinth, "p", file_version_id, None);
            match with_fixture(own_format(2), install).await.unwrap_err() {
                CoreError::GameDirDataTooNew {
                    path,
                    found,
                    supported,
                    written_by,
                } => {
                    assert_eq!(path, game, "{target:?}");
                    assert_eq!((found, supported), (3, 2), "{target:?}");
                    assert_eq!(written_by.as_deref(), Some(NEWER_WRITER), "{target:?}");
                }
                other => panic!("{target:?} 应报游戏目录太新，实际是 {other:?}"),
            }
            assert_eq!(disk_state(&game), before, "{target:?}");
            assert_eq!(
                server.received_requests().await.unwrap().len(),
                0,
                "{target:?}"
            );
        }

        // 读不懂与号是多少无关：不换本版号，走生产的这条路。
        let unreadable = r#"{"data_format":"3"}"#;
        write_stamp(&game, unreadable);
        let expected = match parse_stamp(StampFile::Contents(unreadable.as_bytes())) {
            StampRead::Unreadable { reason } => reason,
            other => panic!("夹具应当读不懂，实际是 {other:?}"),
        };
        let before = disk_state(&game);
        for (target, file_version_id) in installs {
            match petalfall
                .install_resource(target, Platform::Modrinth, "p", file_version_id, None)
                .await
                .unwrap_err()
            {
                CoreError::DataFormatUnreadable { path, reason } => {
                    assert_eq!(path, game, "{target:?}");
                    assert_eq!(reason, expected, "{target:?}");
                }
                other => panic!("{target:?} 应报记号读不懂，实际是 {other:?}"),
            }
            assert_eq!(disk_state(&game), before, "{target:?}");
            assert_eq!(
                server.received_requests().await.unwrap().len(),
                0,
                "{target:?}"
            );
        }
    }
}
