//! 原版材质服务的命令：状态查询、方块目录、下载原版材质、账户皮肤、远程缩略图登记。图本身经
//! `petalfall-tex` 协议取（见 [`crate::textures`]），这里只回答「有没有、用哪个键去取」。

use std::future::Future;
use std::path::Path;

use petalfall_core::art::BlockCatalog;
use petalfall_core::{
    default_skin, Account, AccountType, ArtDownloadProgress, ArtDownloadStage, ArtStatus,
    DefaultSkinSet, DownloadedClient, SkinInfo, SkinModel, TextureService, ThumbError,
};
use serde::Serialize;
use serde_json::json;
use tauri::{AppHandle, State};

use crate::commands::accounts::with_accounts;
use crate::commands::error::code;
use crate::commands::CommandError;
use crate::events;
use crate::facade::Facade;

/// 下载原版材质的进度，负载为 [`ArtDownloadProgress`]。
pub const ART_DOWNLOAD_EVENT: &str = "petalfall://art-download";

/// 同一时刻只允许一次下载：两次并发会往同一个暂存目录写同一个 jar。
#[derive(Default)]
pub struct ArtDownloadSlot(tokio::sync::Mutex<()>);

impl ArtDownloadSlot {
    /// 占住下载位；上一次还没结束时报 `task_conflict`。
    fn claim(&self) -> Result<tokio::sync::MutexGuard<'_, ()>, CommandError> {
        self.0
            .try_lock()
            .map_err(|_| CommandError::new(code::TASK_CONFLICT, "原版材质正在下载，等这一次结束"))
    }
}

/// 材质源 DTO。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ArtSourceDto {
    pub version_id: String,
    /// 本机的降级集（1.13 到 1.19）：预设里较新的方块缺材质，前端照样给「下载原版材质」入口，下载来的最新正式版盖过它。
    pub degraded: bool,
    /// 材质源早于 1.19.3，默认皮肤只有 steve 与 alex：离线账户弹窗按旧版规则预览，与 `skin_info` 给的一致。
    pub legacy_skins: bool,
}

/// 材质服务状态 DTO，也是 `petalfall://art-changed` 的负载。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ArtStatusDto {
    /// 材质源；为 null 时前端不发任何材质请求，所有点缀位画回落件。
    pub source: Option<ArtSourceDto>,
    /// 原版标题全景是否可用。
    pub panorama: bool,
}

impl From<ArtStatus> for ArtStatusDto {
    fn from(status: ArtStatus) -> Self {
        ArtStatusDto {
            source: status.source.map(|version_id| ArtSourceDto {
                version_id,
                degraded: status.degraded,
                legacy_skins: status.default_skins == DefaultSkinSet::Legacy,
            }),
            panorama: status.panorama,
        }
    }
}

/// 方块目录 DTO，即前端的 `BlockCatalogDto`。门面的 [`BlockCatalog`] 序列化形状本来就是它（缓存里的
/// catalog.json 也是这个形状），不另抄一份字段；形状由下面的测试钉住。
pub type BlockCatalogDto = BlockCatalog;

/// 账户皮肤 DTO。远程皮肤的 `sha1` 用来拼协议地址 `/skin/<sha1>.png`；默认皮肤的 `path` 是材质名单里的
/// 路径，经 `/art/<path>.png` 从本机游戏文件读。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SkinInfoDto {
    Remote {
        sha1: String,
        model: SkinModel,
        /// 头像是否叠帽子层。
        hat: bool,
    },
    Default {
        path: &'static str,
        model: SkinModel,
    },
}

pub(crate) fn skin_info_dto(info: &SkinInfo) -> SkinInfoDto {
    SkinInfoDto::Remote {
        sha1: info.sha1.clone(),
        model: info.model,
        hat: info.hat,
    }
}

impl From<ThumbError> for CommandError {
    fn from(err: ThumbError) -> Self {
        let (code, detail) = match &err {
            ThumbError::InvalidUrl(_) => (code::INVALID_ARGUMENT, None),
            ThumbError::HostNotAllowed(host) => {
                (code::INVALID_ARGUMENT, Some(json!({ "host": host })))
            }
            ThumbError::Http { url, source } => (
                code::DOWNLOAD_FAILED,
                Some(json!({ "url": url, "causes": [source.to_string()] })),
            ),
            ThumbError::Status { url, status } => (
                code::DOWNLOAD_FAILED,
                Some(json!({ "url": url, "status": status })),
            ),
            ThumbError::TooLarge { url, .. } | ThumbError::NotImage { url } => {
                (code::DOWNLOAD_FAILED, Some(json!({ "url": url })))
            }
            ThumbError::Io { path, source } => (
                code::IO,
                Some(json!({ "path": path.display().to_string(), "causes": [source.to_string()] })),
            ),
            ThumbError::Write(source) => {
                (code::IO, Some(json!({ "causes": [source.to_string()] })))
            }
        };
        let message = err.to_string();
        tracing::warn!(code, %message, ?detail, "缩略图登记失败");
        let error = CommandError::new(code, message);
        match detail {
            Some(detail) => error.with_detail(detail),
            None => error,
        }
    }
}

/// 材质服务当前状态（只读内存）。前端应先订阅 `petalfall://art-changed` 再调用它，避免漏掉
/// 启动时那次刷新的变化。
#[tauri::command]
pub async fn art_status(textures: State<'_, TextureService>) -> Result<ArtStatusDto, CommandError> {
    Ok(textures.status().into())
}

/// 当前材质源的方块目录（只读内存）：材质源里全部完整立方体方块的各面、染色、色相与名称，按 id 排序；
/// 没有材质源时为 null。材质源或目录变了会推送 `petalfall://art-changed`，前端收到后重新取。
#[tauri::command]
pub async fn art_blocks(
    textures: State<'_, TextureService>,
) -> Result<Option<BlockCatalogDto>, CommandError> {
    Ok(textures.catalog())
}

/// 本机没有原版游戏、或只有 1.20 之前的版本（材质源是降级集）时，从官方源（按文件下载源设置，镜像回退）下载
/// 最新正式版的客户端，抽方块材质、方块状态、模型与名称进缓存，然后重选材质源并返回新状态（状态变了同时推送
/// `petalfall://art-changed`）。下载缓存排在本机降级集之前，下完就换成它。
///
/// 只在玩家点下载时跑。上一次还没下完时报 `task_conflict`。暂存目录只在抽取成功后删：下载或抽取失败时
/// 已下好的分片与 jar 留着，重试时续传、校验通过就不再重下；成功一次就连同旧版本残留一起清掉。
#[tauri::command]
pub async fn art_download(
    app: AppHandle,
    facade: State<'_, Facade>,
    textures: State<'_, TextureService>,
    slot: State<'_, ArtDownloadSlot>,
) -> Result<ArtStatusDto, CommandError> {
    let service = textures.inner();
    let dest = service.download_dir();
    let emitter = app.clone();
    let progress = move |p: ArtDownloadProgress| events::emit(&emitter, ART_DOWNLOAD_EVENT, p);
    let progress = &progress;
    let download = async {
        let petalfall = facade.snapshot().await;
        petalfall
            .download_art_client(&dest, progress)
            .await
            .map_err(CommandError::from)
    };
    let extract = move |client| async move {
        progress(ArtDownloadProgress {
            stage: ArtDownloadStage::Extract,
            downloaded: 0,
            total: None,
        });
        service.adopt_download(client).await
    };
    run_download(&slot, &dest, download, extract).await?;
    Ok(crate::textures::refresh(&app).await.into())
}

/// 一次下载的全过程：占住下载位，下进暂存 `dest`，抽进缓存，再删暂存。下载与抽取两步由调用方给，
/// 命令里是门面下载与材质服务抽取；这里只管占位、两步的先后与暂存的去留，测试换上假的两步就能测到。
/// `download` 是还没开始跑的 future，占到下载位之后才 await 它。
async fn run_download<X, XF>(
    slot: &ArtDownloadSlot,
    dest: &Path,
    download: impl Future<Output = Result<DownloadedClient, CommandError>>,
    extract: X,
) -> Result<(), CommandError>
where
    X: FnOnce(DownloadedClient) -> XF,
    XF: Future<Output = Result<(), String>>,
{
    let _running = slot.claim()?;
    let client = download.await?;
    extract(client).await.map_err(|reason| {
        tracing::warn!(%reason, "抽取下载来的原版材质失败");
        CommandError::new(code::IO, format!("抽取原版材质失败：{reason}"))
    })?;
    if let Err(err) = tokio::fs::remove_dir_all(dest).await {
        // 材质已经进了缓存，暂存只是占地方：删不掉留到下次启动或下次成功再删，不让这次下载算失败
        tracing::warn!(dir = %dest.display(), error = %err, "清理原版材质下载暂存失败");
    }
    Ok(())
}

/// 启动时第一轮选源之后清掉没人再用的下载暂存 `dir`：下载成功后暂存被杀软短暂占着没删掉，或下到一半
/// 失败后玩家改去装了新版游戏，有了完整的材质源前端就不再给下载入口，暂存再没有人清（选源时的旧缓存清理特意
/// 跳过它）。没有材质源或只有降级集时留着：前端还给下载入口，重试要续传。有下载在跑时暂存归它，它成功后自己删。
/// 清理期间占住下载位，免得刚开始的下载往正被删的目录里写。暂存本来就不在时什么都不做。
pub(crate) async fn discard_staging(
    slot: &ArtDownloadSlot,
    status: &ArtStatus,
    dir: &Path,
) -> std::io::Result<()> {
    if status.source.is_none() || status.degraded {
        return Ok(());
    }
    let Ok(_running) = slot.claim() else {
        tracing::info!(dir = %dir.display(), "原版材质正在下载，下载暂存留给它");
        return Ok(());
    };
    match tokio::fs::remove_dir_all(dir).await {
        Ok(()) => {
            tracing::info!(dir = %dir.display(), "已清理用不上的原版材质下载暂存");
            Ok(())
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

/// 账户当前皮肤：远程皮肤就绪时返回取图键与模型；拿不到远程皮肤时返回它进游戏后显示的原版默认皮肤（见
/// [`account_skin`]），默认皮肤图在本机也读不到时界面画剪影。正版与外置账户的 UUID 不是 32 位十六进制时为 null。
#[tauri::command]
pub async fn skin_info(
    account_uuid: String,
    facade: State<'_, Facade>,
    textures: State<'_, TextureService>,
) -> Result<Option<SkinInfoDto>, CommandError> {
    let lookup = account_uuid.clone();
    let account = with_accounts(&facade, move |petalfall| petalfall.find_account(&lookup))
        .await?
        .ok_or_else(|| {
            CommandError::from(petalfall_core::AuthError::AccountNotFound(account_uuid))
        })?;
    let remote = match account.skin.as_ref() {
        Some(skin) => textures.skin_info(skin).await,
        None => None,
    };
    account_skin(&account, remote.as_ref(), textures.status().default_skins)
}

/// 账户画哪张皮肤。`remote` 是下载校验好的远程皮肤，就绪时用它。拿不到时（离线账户永远没有；正版与外置账户
/// 档案里没有皮肤、档案没取到、皮肤下载失败）按 UUID 选原版默认皮肤：游戏里拿不到玩家皮肤时显示的就是这一套，
/// 界面与游戏一致，不画一个哪里都见不到的剪影。按材质源里的那一套（`set`）选，选出的图才读得到。
///
/// 默认皮肤只是这一次的回答，不写回账户，也不记「下载失败过」：档案后来取到皮肤、或下载后来成功了，下一次查询
/// 给的就是真实皮肤，前端换了取图地址，按新地址取图。
fn account_skin(
    account: &Account,
    remote: Option<&SkinInfo>,
    set: DefaultSkinSet,
) -> Result<Option<SkinInfoDto>, CommandError> {
    if remote.is_none() && account.account_type == AccountType::Offline {
        // 离线账户的 UUID 是启动器按名字算出来的，选不出默认皮肤就是启动器自己的缺陷
        let skin = default_skin_info(&account.uuid, set).ok_or_else(|| {
            CommandError::new(
                code::INTERNAL,
                format!("离线账户的 UUID 不是 32 位十六进制：{}", account.uuid),
            )
        })?;
        return Ok(Some(skin));
    }
    Ok(remote_or_default(&account.uuid, remote, set))
}

/// 正版与外置账户（含外置登录待选的角色）的皮肤：远程皮肤就绪时用它，拿不到时按 UUID 给默认皮肤（理由见
/// [`account_skin`]）。UUID 来自登录服务器，启动器改不了；不是 32 位十六进制时选不出默认皮肤，为 None，
/// 界面画剪影，原因记进日志。
pub(crate) fn remote_or_default(
    uuid: &str,
    remote: Option<&SkinInfo>,
    set: DefaultSkinSet,
) -> Option<SkinInfoDto> {
    if let Some(info) = remote {
        return Some(skin_info_dto(info));
    }
    let skin = default_skin_info(uuid, set);
    if skin.is_none() {
        tracing::warn!(
            uuid,
            "账户 UUID 不是 32 位十六进制，选不出默认皮肤，界面画剪影"
        );
    }
    skin
}

fn default_skin_info(uuid: &str, set: DefaultSkinSet) -> Option<SkinInfoDto> {
    default_skin(uuid, set).map(|skin| SkinInfoDto::Default {
        path: skin.path,
        model: skin.model,
    })
}

/// 登记一张远程缩略图：下载并缓存后返回取图键，前端再经 `/thumb/<键>` 取图。
/// 只接受 Modrinth、CurseForge 与官方资讯的图片域名，其余报 `invalid_argument`。
#[tauri::command]
pub async fn thumb_register(
    url: String,
    textures: State<'_, TextureService>,
) -> Result<String, CommandError> {
    Ok(textures.thumb_register(&url).await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use petalfall_core::SkinModel;

    #[test]
    fn art_status_serializes_with_nullable_source() {
        assert_eq!(
            serde_json::to_value(ArtStatusDto::from(ArtStatus::default())).unwrap(),
            json!({ "source": null, "panorama": false })
        );
        assert_eq!(
            serde_json::to_value(ArtStatusDto::from(ArtStatus {
                source: Some("1.21.1".into()),
                degraded: false,
                default_skins: DefaultSkinSet::Modern,
                panorama: true,
            }))
            .unwrap(),
            json!({ "source": { "version_id": "1.21.1", "degraded": false, "legacy_skins": false }, "panorama": true })
        );
        assert_eq!(
            serde_json::to_value(ArtStatusDto::from(ArtStatus {
                source: Some("1.16.5".into()),
                degraded: true,
                default_skins: DefaultSkinSet::Legacy,
                panorama: false,
            }))
            .unwrap(),
            json!({ "source": { "version_id": "1.16.5", "degraded": true, "legacy_skins": true }, "panorama": false })
        );
        assert_eq!(
            serde_json::to_value(ArtStatusDto::from(ArtStatus {
                source: Some("1.19.4".into()),
                degraded: true,
                default_skins: DefaultSkinSet::Modern,
                panorama: false,
            }))
            .unwrap(),
            json!({ "source": { "version_id": "1.19.4", "degraded": true, "legacy_skins": false }, "panorama": false })
        );
    }

    /// 前端 `ipc/art.ts` 的 `BlockCatalogDto` 按这个形状校验：字段名、null、染色是 [r, g, b] 数组、色相是整数。
    #[test]
    fn block_catalog_serializes_in_the_frontend_shape() {
        let catalog: BlockCatalogDto = BlockCatalog {
            version_id: "26.3".into(),
            blocks: vec![
                petalfall_core::art::CatalogBlock {
                    id: "furnace".into(),
                    top: "block/furnace_top".into(),
                    side: "block/furnace_side".into(),
                    front: Some("block/furnace_front".into()),
                    top_tint: None,
                    side_tint: None,
                    hue: 30,
                    name_en: Some("Furnace".into()),
                    name_zh: Some("熔炉".into()),
                },
                petalfall_core::art::CatalogBlock {
                    id: "oak_leaves".into(),
                    top: "block/oak_leaves".into(),
                    side: "block/oak_leaves".into(),
                    front: None,
                    top_tint: Some([72, 181, 24]),
                    side_tint: Some([72, 181, 24]),
                    hue: 102,
                    name_en: None,
                    name_zh: None,
                },
            ],
        };
        assert_eq!(
            serde_json::to_value(Some(catalog)).unwrap(),
            json!({
                "version_id": "26.3",
                "blocks": [
                    {
                        "id": "furnace",
                        "top": "block/furnace_top",
                        "side": "block/furnace_side",
                        "front": "block/furnace_front",
                        "top_tint": null,
                        "side_tint": null,
                        "hue": 30,
                        "name_en": "Furnace",
                        "name_zh": "熔炉"
                    },
                    {
                        "id": "oak_leaves",
                        "top": "block/oak_leaves",
                        "side": "block/oak_leaves",
                        "front": null,
                        "top_tint": [72, 181, 24],
                        "side_tint": [72, 181, 24],
                        "hue": 102,
                        "name_en": null,
                        "name_zh": null
                    }
                ]
            })
        );
        assert_eq!(
            serde_json::to_value(None::<BlockCatalogDto>).unwrap(),
            json!(null)
        );
    }

    #[test]
    fn skin_info_serializes_as_tagged_remote() {
        let dto = skin_info_dto(&SkinInfo {
            sha1: "a9993e364706816aba3e25717850c26c9cd0d89d".into(),
            model: SkinModel::Slim,
            hat: true,
        });
        assert_eq!(
            serde_json::to_value(dto).unwrap(),
            json!({
                "kind": "remote",
                "sha1": "a9993e364706816aba3e25717850c26c9cd0d89d",
                "model": "slim",
                "hat": true
            })
        );
    }

    #[test]
    fn offline_account_without_skin_gets_its_in_game_default() {
        // Steve 的离线 UUID 5627dd98-e6be-3c21-b8a8-e92344183641：原版里是宽臂 Efe
        let steve = petalfall_core::Account::new(
            "5627dd98e6be3c21b8a8e92344183641",
            "Steve",
            petalfall_core::AccountCredentials::Offline,
        );
        let dto = account_skin(&steve, None, DefaultSkinSet::Modern)
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::to_value(dto).unwrap(),
            json!({ "kind": "default", "path": "entity/player/wide/efe", "model": "wide" })
        );

        let alex = petalfall_core::Account::new(
            "36532b5ec4423dbba24cc7e55d0f979a",
            "Alex",
            petalfall_core::AccountCredentials::Offline,
        );
        assert_eq!(
            account_skin(&alex, None, DefaultSkinSet::Modern).unwrap(),
            Some(SkinInfoDto::Default {
                path: "entity/player/slim/makena",
                model: SkinModel::Slim,
            })
        );
    }

    /// 材质源早于 1.19.3 时只有 steve 与 alex 两张：Steve 的离线 UUID 哈希是奇数，原版里是细臂 alex。
    #[test]
    fn offline_account_on_a_legacy_source_gets_steve_or_alex() {
        let steve = petalfall_core::Account::new(
            "5627dd98e6be3c21b8a8e92344183641",
            "Steve",
            petalfall_core::AccountCredentials::Offline,
        );
        assert_eq!(
            account_skin(&steve, None, DefaultSkinSet::Legacy).unwrap(),
            Some(SkinInfoDto::Default {
                path: "entity/alex",
                model: SkinModel::Slim,
            })
        );
        let alex = petalfall_core::Account::new(
            "36532b5ec4423dbba24cc7e55d0f979a",
            "Alex",
            petalfall_core::AccountCredentials::Offline,
        );
        assert_eq!(
            serde_json::to_value(account_skin(&alex, None, DefaultSkinSet::Legacy).unwrap())
                .unwrap(),
            json!({ "kind": "default", "path": "entity/steve", "model": "wide" })
        );
    }

    fn microsoft(uuid: &str, name: &str) -> Account {
        petalfall_core::Account::new(
            uuid,
            name,
            petalfall_core::AccountCredentials::Microsoft(petalfall_core::MicrosoftCredentials {
                refresh_token: "r".into(),
                minecraft_token: None,
                minecraft_expires_at: None,
            }),
        )
    }

    fn authlib(uuid: &str, name: &str) -> Account {
        petalfall_core::Account::new(
            uuid,
            name,
            petalfall_core::AccountCredentials::AuthlibInjector(
                petalfall_core::YggdrasilCredentials {
                    api_root: "https://skin.example/api/yggdrasil/".into(),
                    access_token: "a".into(),
                    client_token: "c".into(),
                    username: None,
                },
            ),
        )
    }

    fn default_dto(path: &'static str, model: SkinModel) -> Option<SkinInfoDto> {
        Some(SkinInfoDto::Default { path, model })
    }

    /// 三种拿不到远程皮肤的情况各取一个 UUID（哈希分别为 -369792882、1946714239、1277771483，与
    /// `default_skin` 测试里由 JDK 算出的哈希向量同一组），按材质源的两套各选一次：与离线账户同一个选取实现。
    #[test]
    fn online_accounts_without_a_remote_skin_get_the_default_for_their_uuid() {
        // 微软档案里没有可用的皮肤：档案记录的 skin 为空
        let no_skin = microsoft("069a79f444e94726a5befca90e38aaf5", "Notch");
        // 外置登录时会话服务器没答上来：登录照样成功，skin 为空
        let unfetched = authlib("853c80ef-3c37-49fd-aa49-938b674adae6", "jeb_");
        // 档案里有皮肤地址，下载失败：没有就绪的远程皮肤
        let mut failed = microsoft("5627dd98e6be3c21b8a8e92344183641", "Steve");
        failed.skin = Some(petalfall_core::SkinRef {
            url: "https://textures.minecraft.net/texture/unreachable".into(),
            model: SkinModel::Slim,
        });
        for (account, modern, legacy) in [
            (
                &no_skin,
                default_dto("entity/player/slim/alex", SkinModel::Slim),
                default_dto("entity/steve", SkinModel::Wide),
            ),
            (
                &unfetched,
                default_dto("entity/player/slim/ari", SkinModel::Slim),
                default_dto("entity/alex", SkinModel::Slim),
            ),
            (
                &failed,
                default_dto("entity/player/wide/efe", SkinModel::Wide),
                default_dto("entity/alex", SkinModel::Slim),
            ),
        ] {
            assert_eq!(
                account_skin(account, None, DefaultSkinSet::Modern).unwrap(),
                modern,
                "{}",
                account.name
            );
            assert_eq!(
                account_skin(account, None, DefaultSkinSet::Legacy).unwrap(),
                legacy,
                "{}",
                account.name
            );
        }
        // 体型跟着默认皮肤走，不跟档案里那张没下下来的皮肤
        assert_eq!(
            serde_json::to_value(account_skin(&failed, None, DefaultSkinSet::Modern).unwrap())
                .unwrap(),
            json!({ "kind": "default", "path": "entity/player/wide/efe", "model": "wide" })
        );
    }

    /// 默认皮肤不写回账户也不缓存：档案后来取到了皮肤、下载也成了，这一次给的就是真实皮肤。
    #[test]
    fn a_real_skin_that_arrives_later_replaces_the_default() {
        let mut account = authlib("853c80ef3c3749fdaa49938b674adae6", "jeb_");
        assert_eq!(
            account_skin(&account, None, DefaultSkinSet::Modern).unwrap(),
            default_dto("entity/player/slim/ari", SkinModel::Slim)
        );
        account.skin = Some(petalfall_core::SkinRef {
            url: "https://skin.example/textures/jeb".into(),
            model: SkinModel::Wide,
        });
        let real = SkinInfo {
            sha1: "a9993e364706816aba3e25717850c26c9cd0d89d".into(),
            model: SkinModel::Wide,
            hat: false,
        };
        assert_eq!(
            serde_json::to_value(
                account_skin(&account, Some(&real), DefaultSkinSet::Modern).unwrap()
            )
            .unwrap(),
            json!({ "kind": "remote", "sha1": "a9993e364706816aba3e25717850c26c9cd0d89d", "model": "wide", "hat": false })
        );
    }

    /// 外置登录待选的角色与账户同一条规则：远程皮肤就绪用它，拿不到按角色 UUID 给默认皮肤。
    #[test]
    fn authlib_profiles_fall_back_to_their_default_skin() {
        assert_eq!(
            remote_or_default(
                "069a79f4-44e9-4726-a5be-fca90e38aaf5",
                None,
                DefaultSkinSet::Modern
            ),
            default_dto("entity/player/slim/alex", SkinModel::Slim)
        );
        assert_eq!(
            remote_or_default(
                "853c80ef3c3749fdaa49938b674adae6",
                None,
                DefaultSkinSet::Legacy
            ),
            default_dto("entity/alex", SkinModel::Slim)
        );
        let real = SkinInfo {
            sha1: "a9993e364706816aba3e25717850c26c9cd0d89d".into(),
            model: SkinModel::Slim,
            hat: true,
        };
        assert_eq!(
            remote_or_default(
                "853c80ef3c3749fdaa49938b674adae6",
                Some(&real),
                DefaultSkinSet::Modern
            ),
            Some(skin_info_dto(&real))
        );
    }

    /// 正版与外置账户的 UUID 来自登录服务器：不合规时选不出默认皮肤，画剪影，不当成启动器的错误报出去。
    #[test]
    fn online_account_with_a_broken_uuid_is_a_silhouette() {
        for account in [
            microsoft("not-a-uuid", "Steve"),
            authlib("5627dd98-e6be", "Steve"),
        ] {
            assert_eq!(
                account_skin(&account, None, DefaultSkinSet::Modern).unwrap(),
                None
            );
            assert_eq!(
                account_skin(&account, None, DefaultSkinSet::Legacy).unwrap(),
                None
            );
        }
    }

    #[test]
    fn offline_account_with_a_broken_uuid_fails_loudly() {
        let account = petalfall_core::Account::new(
            "not-a-uuid",
            "Steve",
            petalfall_core::AccountCredentials::Offline,
        );
        assert_eq!(
            account_skin(&account, None, DefaultSkinSet::Modern)
                .unwrap_err()
                .code,
            code::INTERNAL
        );
    }

    #[test]
    fn only_one_art_download_runs_at_a_time() {
        let slot = ArtDownloadSlot::default();
        let first = slot.claim().unwrap();
        assert_eq!(slot.claim().unwrap_err().code, code::TASK_CONFLICT);
        drop(first);
        assert!(slot.claim().is_ok(), "上一次结束后可以再下");
    }

    /// 暂存目录里放一个下了一半的 jar，返回暂存目录。
    fn staged(root: &Path) -> std::path::PathBuf {
        let dest = root.join(".download");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("client.jar"), b"jar").unwrap();
        dest
    }

    fn client_in(dest: &Path) -> DownloadedClient {
        DownloadedClient {
            version_id: "26.1".into(),
            jar: dest.join("client.jar"),
            jar_sha1: "a9993e364706816aba3e25717850c26c9cd0d89d".into(),
            zh_cn: None,
        }
    }

    #[tokio::test]
    async fn successful_download_hands_the_client_over_then_drops_the_staging_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = staged(tmp.path());
        let slot = ArtDownloadSlot::default();
        let mut adopted = None;
        run_download(
            &slot,
            &dest,
            async { Ok(client_in(&dest)) },
            |client: DownloadedClient| {
                // 抽取时 jar 还得在：暂存要等抽完才删
                adopted = Some((client.version_id.clone(), client.jar.is_file()));
                async { Ok(()) }
            },
        )
        .await
        .unwrap();
        assert_eq!(adopted, Some(("26.1".to_owned(), true)));
        assert!(!dest.exists(), "抽取成功后暂存目录应被删掉");
    }

    #[tokio::test]
    async fn failed_steps_keep_the_staging_dir_for_resuming() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = staged(tmp.path());
        let slot = ArtDownloadSlot::default();

        let extract_failed =
            run_download(&slot, &dest, async { Ok(client_in(&dest)) }, |_| async {
                Err("磁盘满了".to_owned())
            })
            .await
            .unwrap_err();
        assert_eq!(extract_failed.code, code::IO);
        assert_eq!(extract_failed.message, "抽取原版材质失败：磁盘满了");
        assert!(dest.join("client.jar").is_file(), "抽取失败时暂存要留着");

        let mut extracted = false;
        let download_failed = run_download(
            &slot,
            &dest,
            async {
                Err::<DownloadedClient, _>(CommandError::new(code::DOWNLOAD_FAILED, "连不上"))
            },
            |_| {
                extracted = true;
                async { Ok(()) }
            },
        )
        .await
        .unwrap_err();
        assert_eq!(download_failed.code, code::DOWNLOAD_FAILED);
        assert!(!extracted, "下载失败就不该抽取");
        assert!(dest.join("client.jar").is_file(), "下载失败时暂存要留着");
    }

    #[tokio::test]
    async fn a_second_download_conflicts_while_the_first_is_still_downloading() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = staged(tmp.path());
        let slot = ArtDownloadSlot::default();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();

        let first = run_download(
            &slot,
            &dest,
            async {
                entered_tx.send(()).unwrap();
                release_rx.await.unwrap();
                Ok(client_in(&dest))
            },
            |_| async { Ok(()) },
        );
        let second = async {
            entered_rx.await.unwrap();
            // 下载位没占住的话这一次会跑下去，报的就是这里的 internal 而不是 task_conflict
            let outcome = run_download(
                &slot,
                &dest,
                async { Err::<DownloadedClient, _>(CommandError::new(code::INTERNAL, "撞车")) },
                |_| async { Ok(()) },
            )
            .await;
            release_tx.send(()).unwrap();
            outcome
        };
        let (first, second) = tokio::join!(first, second);
        assert_eq!(second.unwrap_err().code, code::TASK_CONFLICT);
        first.unwrap();
        assert!(!dest.exists());

        let dest = staged(tmp.path());
        run_download(&slot, &dest, async { Ok(client_in(&dest)) }, |_| async {
            Ok(())
        })
        .await
        .expect("上一次结束后可以再下");
        assert!(!dest.exists());
    }

    fn with_source() -> ArtStatus {
        ArtStatus {
            source: Some("1.21.1".into()),
            degraded: false,
            default_skins: DefaultSkinSet::Modern,
            panorama: false,
        }
    }

    #[tokio::test]
    async fn staging_is_dropped_at_startup_once_a_source_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = staged(tmp.path());
        let slot = ArtDownloadSlot::default();
        discard_staging(&slot, &with_source(), &dest).await.unwrap();
        assert!(!dest.exists());
        assert!(slot.claim().is_ok(), "清理完要放开下载位");
        // 暂存本来就不在：什么都不做，也不报错
        discard_staging(&slot, &with_source(), &dest).await.unwrap();
    }

    #[tokio::test]
    async fn staging_stays_without_a_source_for_resuming() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = staged(tmp.path());
        let slot = ArtDownloadSlot::default();
        discard_staging(&slot, &ArtStatus::default(), &dest)
            .await
            .unwrap();
        assert!(dest.join("client.jar").is_file());
    }

    /// 只有降级集时前端照样给下载入口：上次下到一半的暂存留着续传。
    #[tokio::test]
    async fn staging_stays_with_only_a_degraded_source() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = staged(tmp.path());
        let slot = ArtDownloadSlot::default();
        let degraded = ArtStatus {
            source: Some("1.16.5".into()),
            degraded: true,
            default_skins: DefaultSkinSet::Legacy,
            panorama: false,
        };
        discard_staging(&slot, &degraded, &dest).await.unwrap();
        assert!(dest.join("client.jar").is_file());
        assert!(slot.claim().is_ok(), "没动暂存也不占着下载位");
    }

    #[tokio::test]
    async fn staging_stays_while_a_download_holds_the_slot() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = staged(tmp.path());
        let slot = ArtDownloadSlot::default();
        let running = slot.claim().unwrap();
        discard_staging(&slot, &with_source(), &dest).await.unwrap();
        assert!(dest.join("client.jar").is_file());
        drop(running);
    }

    #[test]
    fn art_download_progress_serializes_like_the_frontend_dto() {
        assert_eq!(
            serde_json::to_value(ArtDownloadProgress {
                stage: ArtDownloadStage::Extract,
                downloaded: 0,
                total: None,
            })
            .unwrap(),
            json!({ "stage": "extract", "downloaded": 0, "total": null })
        );
    }

    #[test]
    fn thumb_errors_map_to_stable_codes() {
        let host: CommandError = ThumbError::HostNotAllowed("evil.example".into()).into();
        assert_eq!(host.code, code::INVALID_ARGUMENT);
        assert_eq!(host.detail.unwrap()["host"], json!("evil.example"));

        let status: CommandError = ThumbError::Status {
            url: "https://cdn.modrinth.com/x.png".into(),
            status: 404,
        }
        .into();
        assert_eq!(status.code, code::DOWNLOAD_FAILED);
        assert_eq!(status.detail.unwrap()["status"], json!(404));

        let not_image: CommandError = ThumbError::NotImage {
            url: "https://cdn.modrinth.com/x.png".into(),
        }
        .into();
        assert_eq!(not_image.code, code::DOWNLOAD_FAILED);

        let invalid: CommandError = ThumbError::InvalidUrl("x".into()).into();
        assert_eq!(invalid.code, code::INVALID_ARGUMENT);
        assert!(invalid.detail.is_none());
    }
}
