//! 从「已合并解析的版本 JSON」推导出下载任务清单。
//!
//! 这一层是纯函数：给定版本 JSON、运行环境 [`RuntimeContext`] 与目录布局，算出 client.jar、
//! 依赖库、日志配置、assetIndex 各自的 [`DownloadTask`]（官方 URL + 落点 + sha1/size 契约），
//! 以及资源对象清单展开出的批量任务。真正的落盘、换源、校验都交给 petalfall-download 引擎，
//! 这里只负责「下什么、下到哪、期望什么哈希」，因此可以脱离网络做表驱动断言。
//!
//! 落点里的每个相对段（库路径、坐标各段、资源 hash、索引 id、日志配置名、版本 id）都来自版本 JSON 或
//! 资源索引，按 [`crate::layout`] 的规则校验，能走出游戏目录的报 [`Error::UnsafePath`]。
//!
//! 加载器处理器在本机生成、不在库清单里的产物下载不到，也不排任务，只由 [`processor_outputs`] 按启动参数推出
//! 位置，供启动前核对与移动实例使用。

use std::collections::HashSet;

use petalfall_download::DownloadTask;
use petalfall_version::loader::{arg_tokens, flag_value};
use petalfall_version::{
    AssetObjectsIndex, Library, RuntimeContext, VersionJson, select_libraries,
};

use crate::error::{Error, Result};
use crate::layout::{ASSET_OBJECTS_BASE, GameLayout, checked_segment, coordinate_path};

/// 客户端主 jar 的下载任务。缺 `downloads.client` 时报错（无从确定来源）。
pub fn client_jar_task(version: &VersionJson, layout: &GameLayout) -> Result<DownloadTask> {
    let client = version
        .downloads
        .as_ref()
        .and_then(|d| d.client.as_ref())
        .ok_or_else(|| Error::MissingClientDownload {
            version: version.id.clone(),
        })?;
    checked_segment(&version.id, || "版本 id".to_owned())?;
    Ok(
        DownloadTask::new(client.url.clone(), layout.version_jar(&version.id))
            .with_sha1(client.sha1.clone())
            .with_size(client.size),
    )
}

/// 按 rules 过滤 + 去重后的全部库下载任务（含主件与当前平台的 natives 件）。
pub fn library_tasks(
    version: &VersionJson,
    ctx: &RuntimeContext,
    layout: &GameLayout,
) -> Result<Vec<DownloadTask>> {
    let selected = select_libraries(version, ctx);
    let mut tasks = Vec::new();
    for lib in selected {
        push_library_tasks(lib, ctx, layout, &mut tasks)?;
    }
    // 同坐标的主件条目与 natives 条目都声明了主件：两个任务写同一个落点，批次里只留一个。
    let mut seen = HashSet::new();
    tasks.retain(|task| seen.insert(task.dest.clone()));
    Ok(tasks)
}

/// 为一组库（不经 rules 过滤/去重）构造下载任务。供 Forge installer 库、版本 JSON 已合并库等
/// 「调用方已自行决定该下哪些库」的场景复用。
pub fn tasks_for_libraries(
    libraries: &[Library],
    ctx: &RuntimeContext,
    layout: &GameLayout,
) -> Result<Vec<DownloadTask>> {
    let mut tasks = Vec::new();
    for lib in libraries {
        push_library_tasks(lib, ctx, layout, &mut tasks)?;
    }
    Ok(tasks)
}

/// 为单个库追加其下载任务。区分三种形态：Mojang 全量式主件、旧式 natives classifier 件、
/// Fabric/Forge 的 maven 简写式（仅 name + 仓库 url）。
fn push_library_tasks(
    lib: &Library,
    ctx: &RuntimeContext,
    layout: &GameLayout,
    out: &mut Vec<DownloadTask>,
) -> Result<()> {
    // 1) Mojang 全量式主件 downloads.artifact。
    if let Some(artifact) = lib.downloads.as_ref().and_then(|d| d.artifact.as_ref()) {
        let rel = match &artifact.path {
            Some(path) => path.clone(),
            None => coordinate_path(&lib.name)?,
        };
        out.push(
            DownloadTask::new(
                artifact.url.clone(),
                layout.checked_library_path(&lib.name, &rel)?,
            )
            .with_sha1(artifact.sha1.clone())
            .with_size(artifact.size),
        );
    }

    // 2) 旧式 natives：当前平台对应的 classifier 下载件。
    if lib.natives.is_some()
        && let Some(native) = lib.native_artifact(ctx)
    {
        let rel = match &native.path {
            Some(path) => path.clone(),
            None => native_coordinate_path(lib, ctx)?,
        };
        out.push(
            DownloadTask::new(
                native.url.clone(),
                layout.checked_library_path(&lib.name, &rel)?,
            )
            .with_sha1(native.sha1.clone())
            .with_size(native.size),
        );
    }

    // 3) maven 简写式（无 downloads 块，靠仓库地址 + 坐标拼下载地址）。条目顶层写了 sha1 / 大小的
    //    （Fabric meta）照样带上，其余的哈希与大小未知。
    //    没写仓库地址的按官方启动器的约定取 Mojang 的库仓库：旧版 Forge 的 launchwrapper 等就是
    //    这样声明的。带 natives 却没有下载件的条目只有 natives 分类件，不凭坐标猜主件。
    if lib.downloads.is_none() {
        let base = match (&lib.url, &lib.natives) {
            (Some(base), _) => Some(base.as_str()),
            (None, None) => Some(MOJANG_LIBRARIES),
            (None, Some(_)) => None,
        };
        if let Some(base) = base {
            let rel = coordinate_path(&lib.name)?;
            let mut task = DownloadTask::new(
                join_maven(base, &rel),
                layout.checked_library_path(&lib.name, &rel)?,
            );
            if let Some(sha1) = &lib.sha1 {
                task = task.with_sha1(sha1.clone());
            }
            if let Some(size) = lib.size {
                task = task.with_size(size);
            }
            out.push(task);
        }
    }
    Ok(())
}

/// 版本 JSON 里没写仓库地址的库所在的仓库。
const MOJANG_LIBRARIES: &str = "https://libraries.minecraft.net/";

/// 旧式 natives 件的相对路径兜底：把当前平台 classifier 拼进坐标再算路径。classifier 取自版本 JSON 的
/// natives 映射，与坐标其余各段一样校验。
fn native_coordinate_path(lib: &Library, ctx: &RuntimeContext) -> Result<String> {
    let classifier = lib
        .native_classifier(ctx)
        .ok_or_else(|| Error::InvalidLibraryCoordinate {
            name: lib.name.clone(),
        })?;
    coordinate_path(&format!("{}:{classifier}", lib.name))
}

/// 把仓库基址与相对路径拼成完整下载 URL，容忍基址尾部是否带斜杠。
fn join_maven(base: &str, relative: &str) -> String {
    format!("{}/{}", base.trim_end_matches('/'), relative)
}

/// 启动参数没写 `--fml.forgeGroup` 时 FML 按这个组找自己的库，处理器产物也落在这个组下。
const DEFAULT_FORGE_GROUP: &str = "net.minecraftforge";

/// 游戏版本号 `主.次.修订`。
type McVersion = (u32, u32, u32);

/// Forge 参数推出的产物（含沿用这套参数的 1.20.1 NeoForge）在这些游戏版本上对过文件名，两端都含。
const FORGE_CHECKED_MC: (McVersion, McVersion) = ((1, 13, 0), (1, 20, 1));

/// `--fml.neoForgeVersion` 推出的产物在这些游戏版本上对过文件名，两端都含。
const NEOFORGE_CHECKED_MC: (McVersion, McVersion) = ((1, 20, 2), (1, 21, 1));

/// 加载器安装器的处理器在本机生成、不在库清单里的产物，路径都相对库目录（正斜杠分隔）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcessorOutputs {
    /// 补丁后的客户端 jar。
    pub patched_jars: Vec<String>,
    /// 反混淆产物（srg、extra、slim 与映射表）所在的目录。
    pub deobf_dirs: Vec<String>,
}

/// 按合并后版本 JSON 的 `--fml.*` 启动参数推出处理器产物的位置。FML 启动时按同样的参数到库目录里找它们：
/// 处理器的产物按 `[坐标]` 落到坐标对应的库路径（见 forge.rs 的 `resolve_outputs`），Forge 的补丁 jar 是
/// `<组>:forge:<mc>-<forge>:client`，NeoForge 的是 `net.neoforged:neoforge:<版本>:client`，反混淆产物在
/// `net.minecraft:client:<mc>-<mcp 或 neoform>` 的目录下。不是 Forge 系的版本（原版、Fabric、Quilt、1.12 及
/// 更早的旧式 Forge）没有这些参数，得到空表。参数取值带 `..`、分隔符或盘符时报 [`Error::UnsafePath`]。
pub fn processor_outputs(version: &VersionJson) -> Result<ProcessorOutputs> {
    derive_processor_outputs(version, false)
}

/// [`processor_outputs`] 里对过文件名的那部分，启动前的缺失检查只查这些：游戏版本在
/// [`FORGE_CHECKED_MC`]、[`NEOFORGE_CHECKED_MC`] 之外的，加载器可能换了产物的名字或不再生成它们，照旧推出的
/// 位置去查会把好好的实例拦下来，重装也装不出那个文件。
pub(crate) fn checked_processor_outputs(version: &VersionJson) -> Result<ProcessorOutputs> {
    derive_processor_outputs(version, true)
}

fn derive_processor_outputs(version: &VersionJson, checked_only: bool) -> Result<ProcessorOutputs> {
    let tokens = arg_tokens(version);
    let mc = fml_arg(&tokens, "--fml.mcVersion")?;
    let release = mc.as_deref().and_then(release_version);
    let wanted = |(low, high): (McVersion, McVersion)| {
        !checked_only || release.is_some_and(|v| low <= v && v <= high)
    };
    let (forge_wanted, neoforge_wanted) = (wanted(FORGE_CHECKED_MC), wanted(NEOFORGE_CHECKED_MC));
    let mut outputs = ProcessorOutputs::default();
    if let (Some(mc), Some(forge)) = (&mc, fml_arg(&tokens, "--fml.forgeVersion")?) {
        let group =
            fml_arg(&tokens, "--fml.forgeGroup")?.unwrap_or_else(|| DEFAULT_FORGE_GROUP.to_owned());
        if forge_wanted {
            outputs.patched_jars.push(coordinate_path(&format!(
                "{group}:forge:{mc}-{forge}:client"
            ))?);
        }
    }
    if let (Some(mc), Some(mcp)) = (&mc, fml_arg(&tokens, "--fml.mcpVersion")?)
        && forge_wanted
    {
        outputs
            .deobf_dirs
            .push(coordinate_dir(&format!("net.minecraft:client:{mc}-{mcp}"))?);
    }
    if let Some(neoforge) = fml_arg(&tokens, "--fml.neoForgeVersion")?
        && neoforge_wanted
    {
        outputs.patched_jars.push(coordinate_path(&format!(
            "net.neoforged:neoforge:{neoforge}:client"
        ))?);
    }
    if let (Some(mc), Some(neoform)) = (&mc, fml_arg(&tokens, "--fml.neoFormVersion")?)
        && neoforge_wanted
    {
        outputs.deobf_dirs.push(coordinate_dir(&format!(
            "net.minecraft:client:{mc}-{neoform}"
        ))?);
    }
    Ok(outputs)
}

/// 一个 `--fml.*` 参数的取值。它要拼进库目录的路径：拼成 `<mc>-<版本>` 之后 `..` 就不再是一个点段，查不出来，
/// 所以拼之前逐个按单个路径段检查。
fn fml_arg(tokens: &[String], flag: &str) -> Result<Option<String>> {
    let Some(value) = flag_value(tokens, flag) else {
        return Ok(None);
    };
    checked_segment(&value, || format!("启动参数 {flag}"))?;
    Ok(Some(value))
}

/// 正式版的游戏版本号：`1.20.1` 为 `(1, 20, 1)`，`1.21` 补成 `(1, 21, 0)`；快照等别的写法为 `None`。
fn release_version(mc: &str) -> Option<McVersion> {
    let mut parts = mc.split('.').map(|part| part.parse::<u32>().ok());
    let major = parts.next()??;
    let minor = parts.next()??;
    let patch = match parts.next() {
        Some(patch) => patch?,
        None => 0,
    };
    match parts.next() {
        Some(_) => None,
        None => Some((major, minor, patch)),
    }
}

/// maven 坐标 `group:artifact:version` 所在的目录，相对库目录；各段按 [`coordinate_path`] 校验。
fn coordinate_dir(coordinate: &str) -> Result<String> {
    let path = coordinate_path(coordinate)?;
    let (dir, _file) = path
        .rsplit_once('/')
        .expect("坐标算出的路径至少有 artifact 与版本两级目录");
    Ok(dir.to_owned())
}

/// 客户端日志配置文件（log4j2 xml）的下载任务；无 logging 段时返回 `Ok(None)`。配置文件名直接当作
/// `assets/log_configs/` 下的文件名，不是单个安全路径段时报 [`Error::UnsafePath`]。
pub fn logging_task(version: &VersionJson, layout: &GameLayout) -> Result<Option<DownloadTask>> {
    let Some(config) = version.logging.as_ref().and_then(|l| l.client.as_ref()) else {
        return Ok(None);
    };
    let file = &config.file;
    checked_segment(&file.id, || "日志配置".to_owned())?;
    let dest = layout.assets_dir().join("log_configs").join(&file.id);
    Ok(Some(
        DownloadTask::new(file.url.clone(), dest)
            .with_sha1(file.sha1.clone())
            .with_size(file.size),
    ))
}

/// assetIndex 索引文件本身的下载任务。缺 assetIndex 时报错。
pub fn asset_index_task(version: &VersionJson, layout: &GameLayout) -> Result<DownloadTask> {
    let index = version
        .asset_index
        .as_ref()
        .ok_or_else(|| Error::MissingAssetIndex {
            version: version.id.clone(),
        })?;
    Ok(
        DownloadTask::new(index.url.clone(), layout.asset_index_json(&index.id)?)
            .with_sha1(index.sha1.clone())
            .with_size(index.size),
    )
}

/// 把资源索引展开成逐个资源对象的下载任务（标准分桶布局）。hash 直接当作 `objects/` 下的目录名与文件名；
/// 从 JSON 读入的索引已只认 40 位十六进制，这里再挡住代码里直接构造、不是单个安全路径段的 hash。
pub fn asset_object_tasks(
    index: &AssetObjectsIndex,
    layout: &GameLayout,
) -> Result<Vec<DownloadTask>> {
    index
        .objects
        .iter()
        .map(|(name, object)| {
            checked_segment(&object.hash, || format!("资源 {name} 的 hash"))?;
            let url = format!("{ASSET_OBJECTS_BASE}/{}", object.object_path());
            Ok(
                DownloadTask::new(url, layout.asset_object_path(&object.hash))
                    .with_sha1(object.hash.clone())
                    .with_size(object.size),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use petalfall_version::OsName;
    use std::path::PathBuf;

    fn layout() -> GameLayout {
        GameLayout::new(PathBuf::from("/mc"))
    }

    fn win64() -> RuntimeContext {
        RuntimeContext::new(OsName::Windows, "x86_64", 64)
    }

    /// 1.13 到 1.18 的孪生条目：主件只排一次，当前平台的 natives 件要排上，别的平台不排。
    #[test]
    fn natives_twin_queues_the_classifier_jar_and_one_main_jar() {
        let v = VersionJson::from_json_str(
            r#"{"id":"1.18.2","libraries":[
                {"name":"org.lwjgl:lwjgl:3.2.2",
                 "downloads":{"artifact":{"path":"org/lwjgl/lwjgl/3.2.2/lwjgl-3.2.2.jar","sha1":"m","size":3,"url":"https://libraries.minecraft.net/m.jar"}}},
                {"name":"org.lwjgl:lwjgl:3.2.2",
                 "natives":{"linux":"natives-linux","windows":"natives-windows"},
                 "downloads":{
                    "artifact":{"path":"org/lwjgl/lwjgl/3.2.2/lwjgl-3.2.2.jar","sha1":"m","size":3,"url":"https://libraries.minecraft.net/m.jar"},
                    "classifiers":{
                        "natives-linux":{"path":"org/lwjgl/lwjgl/3.2.2/lwjgl-3.2.2-natives-linux.jar","sha1":"l","size":1,"url":"https://libraries.minecraft.net/l.jar"},
                        "natives-windows":{"path":"org/lwjgl/lwjgl/3.2.2/lwjgl-3.2.2-natives-windows.jar","sha1":"w","size":2,"url":"https://libraries.minecraft.net/w.jar"}}}}
            ]}"#,
        )
        .unwrap();
        let mut dests: Vec<PathBuf> = library_tasks(&v, &win64(), &layout())
            .unwrap()
            .into_iter()
            .map(|task| task.dest)
            .collect();
        dests.sort();
        assert_eq!(
            dests,
            vec![
                layout().library_path("org/lwjgl/lwjgl/3.2.2/lwjgl-3.2.2-natives-windows.jar"),
                layout().library_path("org/lwjgl/lwjgl/3.2.2/lwjgl-3.2.2.jar"),
            ]
        );
    }

    #[test]
    fn client_jar_task_uses_downloads_client() {
        let v = VersionJson::from_json_str(
            r#"{"id":"1.21","downloads":{"client":{"sha1":"abc123","size":25000,"url":"https://piston-data.mojang.com/client.jar"}}}"#,
        )
        .unwrap();
        let task = client_jar_task(&v, &layout()).unwrap();
        assert_eq!(task.url, "https://piston-data.mojang.com/client.jar");
        assert_eq!(task.dest, PathBuf::from("/mc/versions/1.21/1.21.jar"));
        assert_eq!(task.sha1.as_deref(), Some("abc123"));
        assert_eq!(task.size, Some(25000));
    }

    #[test]
    fn client_jar_missing_errors() {
        let v = VersionJson::from_json_str(r#"{"id":"x"}"#).unwrap();
        let err = client_jar_task(&v, &layout()).unwrap_err();
        assert!(matches!(err, Error::MissingClientDownload { version } if version == "x"));
    }

    #[test]
    fn library_tasks_mojang_artifact_and_old_native() {
        // gson: 全量式主件；lwjgl-platform: 旧式 natives（无主件，带 classifiers）。
        let v = VersionJson::from_json_str(
            r#"{
                "id":"1.12.2",
                "libraries":[
                    {"name":"com.google.code.gson:gson:2.8.0",
                     "downloads":{"artifact":{"path":"com/google/code/gson/gson/2.8.0/gson-2.8.0.jar","sha1":"g1","size":10,"url":"https://libraries.minecraft.net/com/google/code/gson/gson/2.8.0/gson-2.8.0.jar"}}},
                    {"name":"org.lwjgl.lwjgl:lwjgl-platform:2.9.4",
                     "natives":{"windows":"natives-windows","linux":"natives-linux"},
                     "downloads":{"classifiers":{
                        "natives-windows":{"path":"org/lwjgl/lwjgl/lwjgl-platform/2.9.4/lwjgl-platform-2.9.4-natives-windows.jar","sha1":"w1","size":20,"url":"https://libraries.minecraft.net/lwjgl-natives-windows.jar"},
                        "natives-linux":{"path":"p","sha1":"l1","size":30,"url":"https://libraries.minecraft.net/lwjgl-natives-linux.jar"}
                     }},
                     "extract":{"exclude":["META-INF/"]}}
                ]
            }"#,
        )
        .unwrap();
        let tasks = library_tasks(&v, &win64(), &layout()).unwrap();
        assert_eq!(tasks.len(), 2, "gson 主件 + windows natives 件");

        assert_eq!(
            tasks[0].dest,
            PathBuf::from("/mc/libraries/com/google/code/gson/gson/2.8.0/gson-2.8.0.jar")
        );
        assert_eq!(tasks[0].sha1.as_deref(), Some("g1"));

        // 只挑到 windows natives，不含 linux。
        assert_eq!(
            tasks[1].url,
            "https://libraries.minecraft.net/lwjgl-natives-windows.jar"
        );
        assert_eq!(
            tasks[1].dest,
            PathBuf::from(
                "/mc/libraries/org/lwjgl/lwjgl/lwjgl-platform/2.9.4/lwjgl-platform-2.9.4-natives-windows.jar"
            )
        );
        assert_eq!(tasks[1].sha1.as_deref(), Some("w1"));
    }

    #[test]
    fn library_tasks_maven_shorthand_builds_url_from_coordinate() {
        let v = VersionJson::from_json_str(
            r#"{"id":"fabric","libraries":[
                {"name":"net.fabricmc:fabric-loader:0.15.11","url":"https://maven.fabricmc.net/"}
            ]}"#,
        )
        .unwrap();
        let tasks = library_tasks(&v, &win64(), &layout()).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(
            tasks[0].url,
            "https://maven.fabricmc.net/net/fabricmc/fabric-loader/0.15.11/fabric-loader-0.15.11.jar"
        );
        assert_eq!(
            tasks[0].dest,
            PathBuf::from(
                "/mc/libraries/net/fabricmc/fabric-loader/0.15.11/fabric-loader-0.15.11.jar"
            )
        );
        // 没写 sha1 / 大小的简写式条目没有校验契约。
        assert!(tasks[0].sha1.is_none());
        assert!(tasks[0].size.is_none());
    }

    #[test]
    fn library_without_downloads_or_url_comes_from_mojang_libraries() {
        // 旧版 Forge 的 versionInfo 里 launchwrapper 只有坐标：按官方启动器的约定去 Mojang 的库仓库拉，
        // 漏下它游戏就找不到主类。在盘上即用的判定交给补全批次（见 complete::skip_present_unverifiable）。
        let v = VersionJson::from_json_str(
            r#"{"id":"1.7.10-Forge","libraries":[
                {"name":"net.minecraft:launchwrapper:1.12","serverreq":true},
                {"name":"org.lwjgl.lwjgl:lwjgl-platform:2.9.1","natives":{"windows":"natives-windows"}}
            ]}"#,
        )
        .unwrap();
        let tasks = library_tasks(&v, &win64(), &layout()).unwrap();
        assert_eq!(tasks.len(), 1, "带 natives 却没有下载件的库不凭坐标猜主件");
        assert_eq!(
            tasks[0].url,
            "https://libraries.minecraft.net/net/minecraft/launchwrapper/1.12/launchwrapper-1.12.jar"
        );
        assert_eq!(
            tasks[0].dest,
            PathBuf::from("/mc/libraries/net/minecraft/launchwrapper/1.12/launchwrapper-1.12.jar")
        );
        assert!(tasks[0].sha1.is_none() && tasks[0].size.is_none());
    }

    #[test]
    fn new_style_native_library_downloads_as_main_artifact() {
        // 1.19+ 独立 natives 条目：classifier 在 name 里，downloads.artifact 直给。
        let v = VersionJson::from_json_str(
            r#"{"id":"1.21","libraries":[
                {"name":"org.lwjgl:lwjgl:3.3.3:natives-windows",
                 "downloads":{"artifact":{"path":"org/lwjgl/lwjgl/3.3.3/lwjgl-3.3.3-natives-windows.jar","sha1":"n1","size":50,"url":"https://libraries.minecraft.net/lwjgl-natives.jar"}}}
            ]}"#,
        )
        .unwrap();
        let tasks = library_tasks(&v, &win64(), &layout()).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].sha1.as_deref(), Some("n1"));
        assert_eq!(
            tasks[0].dest,
            PathBuf::from("/mc/libraries/org/lwjgl/lwjgl/3.3.3/lwjgl-3.3.3-natives-windows.jar")
        );
    }

    #[test]
    fn asset_index_and_objects_tasks() {
        let v = VersionJson::from_json_str(
            r#"{"id":"1.21","assetIndex":{"id":"17","sha1":"idx1","size":400,"url":"https://piston-meta.mojang.com/17.json"}}"#,
        )
        .unwrap();
        let idx_task = asset_index_task(&v, &layout()).unwrap();
        assert_eq!(idx_task.dest, PathBuf::from("/mc/assets/indexes/17.json"));
        assert_eq!(idx_task.sha1.as_deref(), Some("idx1"));

        let index = AssetObjectsIndex::from_json_str(
            r#"{"objects":{"minecraft/lang/en_us.json":{"hash":"ab12cd0000000000000000000000000000000000","size":123}}}"#,
        )
        .unwrap();
        let obj_tasks = asset_object_tasks(&index, &layout()).unwrap();
        assert_eq!(obj_tasks.len(), 1);
        assert_eq!(
            obj_tasks[0].url,
            "https://resources.download.minecraft.net/ab/ab12cd0000000000000000000000000000000000"
        );
        assert_eq!(
            obj_tasks[0].dest,
            PathBuf::from("/mc/assets/objects/ab/ab12cd0000000000000000000000000000000000")
        );
        assert_eq!(
            obj_tasks[0].sha1.as_deref(),
            Some("ab12cd0000000000000000000000000000000000")
        );
        assert_eq!(obj_tasks[0].size, Some(123));
    }

    #[test]
    fn logging_task_lands_under_log_configs() {
        let v = VersionJson::from_json_str(
            r#"{"id":"1.21","logging":{"client":{"argument":"-Dlog4j.configurationFile=${path}","type":"log4j2-xml","file":{"id":"client-1.12.xml","sha1":"log1","size":900,"url":"https://piston-data.mojang.com/client-1.12.xml"}}}}"#,
        )
        .unwrap();
        let task = logging_task(&v, &layout()).unwrap().unwrap();
        assert_eq!(
            task.dest,
            PathBuf::from("/mc/assets/log_configs/client-1.12.xml")
        );
        assert_eq!(task.sha1.as_deref(), Some("log1"));
    }

    use petalfall_version::UnsafePath;

    /// 越界的各种形态与对应的问题；绝对路径、盘符在 Windows 上 join 时会整段替换掉库目录。
    const ESCAPING_PATHS: [(&str, UnsafePath); 7] = [
        ("../../../evil.jar", UnsafePath::DotSegment),
        ("/abs/evil.jar", UnsafePath::Absolute),
        ("C:/Windows/evil.dll", UnsafePath::Colon),
        ("org\\..\\..\\evil.jar", UnsafePath::Backslash),
        ("org/evil.jar:stream", UnsafePath::Colon),
        ("org//evil.jar", UnsafePath::EmptySegment),
        ("", UnsafePath::Empty),
    ];

    fn version_with_library(library: serde_json::Value) -> VersionJson {
        serde_json::from_value(serde_json::json!({ "id": "evil", "libraries": [library] })).unwrap()
    }

    fn expect_unsafe(result: Result<Vec<DownloadTask>>, entry: &str, path: &str) -> UnsafePath {
        match result {
            Err(Error::UnsafePath {
                entry: got_entry,
                path: got_path,
                problem,
            }) => {
                assert_eq!(got_entry, entry);
                assert_eq!(got_path, path);
                problem
            }
            other => panic!("应报路径越界，实际：{other:?}"),
        }
    }

    #[test]
    fn library_artifact_paths_escaping_the_libraries_dir_are_rejected() {
        for (path, problem) in ESCAPING_PATHS {
            let v = version_with_library(serde_json::json!({
                "name": "com.example:evil:1.0",
                "downloads": {"artifact": {
                    "path": path, "sha1": "e1", "size": 1, "url": "https://example.com/evil.jar"
                }}
            }));
            let got = expect_unsafe(
                library_tasks(&v, &win64(), &layout()),
                "库 com.example:evil:1.0",
                path,
            );
            assert_eq!(got, problem, "{path:?}");
        }
    }

    #[test]
    fn native_classifier_paths_escaping_the_libraries_dir_are_rejected() {
        for (path, problem) in ESCAPING_PATHS {
            let v = version_with_library(serde_json::json!({
                "name": "org.lwjgl.lwjgl:lwjgl-platform:2.9.4",
                "natives": {"windows": "natives-windows"},
                "downloads": {"classifiers": {"natives-windows": {
                    "path": path, "sha1": "w1", "size": 1, "url": "https://example.com/n.jar"
                }}}
            }));
            let got = expect_unsafe(
                library_tasks(&v, &win64(), &layout()),
                "库 org.lwjgl.lwjgl:lwjgl-platform:2.9.4",
                path,
            );
            assert_eq!(got, problem, "{path:?}");
        }
    }

    /// maven 简写式（只有坐标与仓库地址）与旧式 natives 的坐标兜底：坐标里任何一段越界都报错。
    #[test]
    fn coordinates_escaping_the_libraries_dir_are_rejected() {
        for (name, problem) in [
            ("evil:..:1", UnsafePath::DotSegment),
            ("evil:../../../x:1", UnsafePath::Separator),
            ("/abs:x:1", UnsafePath::Separator),
            ("evil:x:1@C:evil", UnsafePath::Colon),
            ("evil:..\\..\\x:1", UnsafePath::Backslash),
            ("evil:x:1@jar:stream", UnsafePath::Colon),
            ("evil..x:x:1", UnsafePath::EmptySegment),
        ] {
            let v = version_with_library(
                serde_json::json!({ "name": name, "url": "https://maven.example.com/" }),
            );
            match library_tasks(&v, &win64(), &layout()) {
                Err(Error::UnsafePath {
                    entry,
                    problem: got,
                    ..
                }) => {
                    assert_eq!(entry, format!("库 {name}"));
                    assert_eq!(got, problem, "{name}");
                }
                other => panic!("{name} 应报路径越界，实际：{other:?}"),
            }
        }

        let v = version_with_library(serde_json::json!({
            "name": "org.lwjgl.lwjgl:lwjgl-platform:2.9.4",
            "natives": {"windows": "natives-${arch}/../../../evil"},
            "downloads": {"classifiers": {"natives-64/../../../evil": {
                "sha1": "w1", "size": 1, "url": "https://example.com/n.jar"
            }}}
        }));
        assert!(matches!(
            library_tasks(&v, &win64(), &layout()),
            Err(Error::UnsafePath {
                problem: UnsafePath::Separator,
                ..
            })
        ));
    }

    /// 合法的真实条目照常通过：新式 natives classifier 坐标、Forge 的 `@zip` 扩展、带 classifier 的 `@txt`。
    #[test]
    fn real_maven_shorthand_entries_keep_their_paths() {
        let v = VersionJson::from_json_str(
            r#"{"id":"forge","libraries":[
                {"name":"org.lwjgl:lwjgl:3.3.3:natives-windows","url":"https://libraries.minecraft.net/"},
                {"name":"de.oceanlabs.mcp:mcp_config:1.20.1-20230612.114412@zip","url":"https://maven.minecraftforge.net/"},
                {"name":"net.minecraft:client:1.20.1-20230612.114412:mappings@txt","url":"https://maven.minecraftforge.net/"}
            ]}"#,
        )
        .unwrap();
        let tasks = library_tasks(&v, &win64(), &layout()).unwrap();
        let got: Vec<(&str, PathBuf)> = tasks
            .iter()
            .map(|t| (t.url.as_str(), t.dest.clone()))
            .collect();
        assert_eq!(
            got,
            vec![
                (
                    "https://libraries.minecraft.net/org/lwjgl/lwjgl/3.3.3/lwjgl-3.3.3-natives-windows.jar",
                    PathBuf::from(
                        "/mc/libraries/org/lwjgl/lwjgl/3.3.3/lwjgl-3.3.3-natives-windows.jar"
                    ),
                ),
                (
                    "https://maven.minecraftforge.net/de/oceanlabs/mcp/mcp_config/1.20.1-20230612.114412/mcp_config-1.20.1-20230612.114412.zip",
                    PathBuf::from(
                        "/mc/libraries/de/oceanlabs/mcp/mcp_config/1.20.1-20230612.114412/mcp_config-1.20.1-20230612.114412.zip"
                    ),
                ),
                (
                    "https://maven.minecraftforge.net/net/minecraft/client/1.20.1-20230612.114412/client-1.20.1-20230612.114412-mappings.txt",
                    PathBuf::from(
                        "/mc/libraries/net/minecraft/client/1.20.1-20230612.114412/client-1.20.1-20230612.114412-mappings.txt"
                    ),
                ),
            ]
        );
    }

    /// 资源索引 id、日志配置名、版本 id 都被原样当作目录名或文件名；资源 hash 是 `objects/` 下的落点。
    #[test]
    fn ids_and_hashes_used_as_file_names_must_be_single_segments() {
        let v = VersionJson::from_json_str(
            r#"{"id":"1.21","assetIndex":{"id":"../../../evil","sha1":"s","size":1,"url":"https://example.com/i.json"}}"#,
        )
        .unwrap();
        assert!(matches!(
            asset_index_task(&v, &layout()),
            Err(Error::UnsafePath { ref entry, ref path, problem: UnsafePath::Separator })
                if entry == "资源索引 id" && path == "../../../evil"
        ));

        let v = VersionJson::from_json_str(
            r#"{"id":"1.21","logging":{"client":{"argument":"a","type":"log4j2-xml","file":{"id":"..","sha1":"s","size":1,"url":"https://example.com/l.xml"}}}}"#,
        )
        .unwrap();
        assert!(matches!(
            logging_task(&v, &layout()),
            Err(Error::UnsafePath { ref entry, problem: UnsafePath::DotSegment, .. }) if entry == "日志配置"
        ));

        let v = VersionJson::from_json_str(
            r#"{"id":"..\\..\\evil","downloads":{"client":{"sha1":"s","size":1,"url":"https://example.com/c.jar"}}}"#,
        )
        .unwrap();
        assert!(matches!(
            client_jar_task(&v, &layout()),
            Err(Error::UnsafePath { ref entry, problem: UnsafePath::Backslash, .. }) if entry == "版本 id"
        ));

        let mut index = AssetObjectsIndex::default();
        index.objects.insert(
            "pack.mcmeta".to_owned(),
            petalfall_version::AssetObject {
                hash: "../../../evil".to_owned(),
                size: 1,
            },
        );
        assert!(matches!(
            asset_object_tasks(&index, &layout()),
            Err(Error::UnsafePath { ref entry, problem: UnsafePath::Separator, .. })
                if entry == "资源 pack.mcmeta 的 hash"
        ));
    }

    /// 游戏参数按空白分开写成一行。
    fn with_game_args(args: &str) -> VersionJson {
        let args: Vec<&str> = args.split_whitespace().collect();
        serde_json::from_value(serde_json::json!({
            "id": "加载器",
            "mainClass": "cpw.mods.bootstraplauncher.BootstrapLauncher",
            "arguments": {"game": args, "jvm": []}
        }))
        .unwrap()
    }

    fn outputs(jars: &[&str], dirs: &[&str]) -> ProcessorOutputs {
        ProcessorOutputs {
            patched_jars: jars.iter().map(|jar| (*jar).to_owned()).collect(),
            deobf_dirs: dirs.iter().map(|dir| (*dir).to_owned()).collect(),
        }
    }

    /// 三组真实的启动参数（Forge 1.20.1、1.20.1 的 NeoForge、NeoForge 21.1）各自推出的产物，文件名与本机装出来的
    /// 一致：补丁 jar 是确切的文件，反混淆产物给到目录。这三组都在核对过的版本里，启动前全数要查。
    #[test]
    fn fml_arguments_point_at_the_processor_outputs() {
        let forge = with_game_args(
            "--launchTarget forgeclient --fml.forgeVersion 47.4.10 --fml.mcVersion 1.20.1 \
             --fml.forgeGroup net.minecraftforge --fml.mcpVersion 20230612.114412",
        );
        let forge_outputs = outputs(
            &["net/minecraftforge/forge/1.20.1-47.4.10/forge-1.20.1-47.4.10-client.jar"],
            &["net/minecraft/client/1.20.1-20230612.114412"],
        );
        let neoforge_fork = with_game_args(
            "--launchTarget forgeclient --fml.forgeVersion 47.1.106 --fml.mcVersion 1.20.1 \
             --fml.forgeGroup net.neoforged --fml.mcpVersion 20230612.114412",
        );
        let fork_outputs = outputs(
            &["net/neoforged/forge/1.20.1-47.1.106/forge-1.20.1-47.1.106-client.jar"],
            &["net/minecraft/client/1.20.1-20230612.114412"],
        );
        let neoforge = with_game_args(
            "--fml.neoForgeVersion 21.1.250 --fml.fmlVersion 4.0.44 --fml.mcVersion 1.21.1 \
             --fml.neoFormVersion 20240808.144430 --launchTarget forgeclient",
        );
        let neoforge_outputs = outputs(
            &["net/neoforged/neoforge/21.1.250/neoforge-21.1.250-client.jar"],
            &["net/minecraft/client/1.21.1-20240808.144430"],
        );
        for (version, expected) in [
            (forge, forge_outputs),
            (neoforge_fork, fork_outputs),
            (neoforge, neoforge_outputs),
        ] {
            assert_eq!(processor_outputs(&version).unwrap(), expected);
            assert_eq!(checked_processor_outputs(&version).unwrap(), expected);
        }

        // 没写 --fml.forgeGroup 时按 FML 的默认组 net.minecraftforge。
        let ungrouped = with_game_args(
            "--fml.forgeVersion 36.2.39 --fml.mcVersion 1.16.5 --fml.mcpVersion 20210115.111550",
        );
        assert_eq!(
            checked_processor_outputs(&ungrouped).unwrap(),
            outputs(
                &["net/minecraftforge/forge/1.16.5-36.2.39/forge-1.16.5-36.2.39-client.jar"],
                &["net/minecraft/client/1.16.5-20210115.111550"],
            )
        );
    }

    /// 原版、Fabric、Quilt 与 1.12 及更早的旧式 Forge 没有 `--fml.*` 参数：什么都推不出，也就什么都不查。
    #[test]
    fn versions_without_fml_arguments_have_no_processor_outputs() {
        let versions = [
            r#"{"id":"1.21.1","mainClass":"net.minecraft.client.main.Main",
                "arguments":{"game":["--username","${auth_player_name}","--version","${version_name}"],"jvm":[]}}"#,
            r#"{"id":"fabric-loader-0.16.10-1.21.1","inheritsFrom":"1.21.1",
                "mainClass":"net.fabricmc.loader.impl.launch.knot.KnotClient",
                "arguments":{"game":[],"jvm":["-DFabricMcEmu= net.minecraft.client.main.Main "]},
                "libraries":[{"name":"net.fabricmc:fabric-loader:0.16.10","url":"https://maven.fabricmc.net/"}]}"#,
            r#"{"id":"quilt-loader-0.28.0-1.21.1","inheritsFrom":"1.21.1",
                "mainClass":"org.quiltmc.loader.impl.launch.knot.KnotClient",
                "libraries":[{"name":"org.quiltmc:quilt-loader:0.28.0","url":"https://maven.quiltmc.org/repository/release/"}]}"#,
            r#"{"id":"1.12.2-forge-14.23.5.2860","inheritsFrom":"1.12.2",
                "mainClass":"net.minecraft.launchwrapper.Launch",
                "minecraftArguments":"--username ${auth_player_name} --version ${version_name} --tweakClass net.minecraftforge.fml.common.launcher.FMLTweaker --versionType Forge",
                "libraries":[{"name":"net.minecraftforge:forge:1.12.2-14.23.5.2860"}]}"#,
        ];
        for json in versions {
            let version = VersionJson::from_json_str(json).unwrap();
            assert_eq!(
                processor_outputs(&version).unwrap(),
                ProcessorOutputs::default(),
                "{}",
                version.id
            );
            assert_eq!(
                checked_processor_outputs(&version).unwrap(),
                ProcessorOutputs::default(),
                "{}",
                version.id
            );
        }
    }

    /// 核对过的版本区间两端都含：Forge 参数查 1.13 到 1.20.1，NeoForge 参数查 1.20.2 到 1.21.1。区间之外的照样
    /// 推出位置（移动实例要带上整个目录），启动前却不查；游戏版本认不出（快照写法、缺参数）的同样不查。
    #[test]
    fn only_versions_with_checked_file_names_are_checked() {
        use std::collections::hash_map::RandomState;
        use std::hash::BuildHasher;

        // 构建号随机，免得推导碰巧对上一个写死的数。
        let build = RandomState::new().hash_one("processor outputs") % 1000;
        let forge_on = |mc: &str| {
            with_game_args(&format!(
                "--fml.forgeVersion 47.1.{build} --fml.mcVersion {mc} \
                 --fml.mcpVersion 20230612.114412"
            ))
        };
        let neoforge_on = |mc: &str| {
            with_game_args(&format!(
                "--fml.neoForgeVersion 21.4.{build} --fml.mcVersion {mc} \
                 --fml.neoFormVersion 20241203.161809"
            ))
        };
        for (version, checked) in [
            (forge_on("1.13"), true),
            (forge_on("1.20.1"), true),
            (forge_on("1.12.2"), false),
            (forge_on("1.20.2"), false),
            (forge_on("1.20.1-pre1"), false),
            (neoforge_on("1.20.2"), true),
            (neoforge_on("1.21.1"), true),
            (neoforge_on("1.20.1"), false),
            (neoforge_on("1.21.2"), false),
            (neoforge_on("26.1"), false),
        ] {
            let all = processor_outputs(&version).unwrap();
            assert_eq!(all.patched_jars.len(), 1);
            assert!(
                all.patched_jars[0].contains(&format!(".{build}/")),
                "{:?}",
                all.patched_jars
            );
            assert_eq!(all.deobf_dirs.len(), 1);
            let expected = if checked {
                all
            } else {
                ProcessorOutputs::default()
            };
            assert_eq!(checked_processor_outputs(&version).unwrap(), expected);
        }

        let neoforge_without_mc = with_game_args("--fml.neoForgeVersion 21.1.250");
        assert_eq!(
            processor_outputs(&neoforge_without_mc).unwrap(),
            outputs(
                &["net/neoforged/neoforge/21.1.250/neoforge-21.1.250-client.jar"],
                &[]
            )
        );
        assert_eq!(
            checked_processor_outputs(&neoforge_without_mc).unwrap(),
            ProcessorOutputs::default()
        );
    }

    /// `--fml.*` 的取值要拼进库路径：带 `..`、分隔符或盘符的报路径越界，指认是哪个参数；不在核对区间里的版本
    /// 同样报，参数本身就不对。
    #[test]
    fn unsafe_fml_values_are_rejected_by_both_derivations() {
        for (args, flag, value, problem) in [
            (
                "--fml.forgeVersion 47.3.33 --fml.mcVersion 1.20.1 --fml.mcpVersion ..",
                "--fml.mcpVersion",
                "..",
                UnsafePath::DotSegment,
            ),
            (
                "--fml.neoForgeVersion ../../evil --fml.mcVersion 1.21.4",
                "--fml.neoForgeVersion",
                "../../evil",
                UnsafePath::Separator,
            ),
            (
                "--fml.forgeVersion 47.3.33 --fml.mcVersion C:",
                "--fml.mcVersion",
                "C:",
                UnsafePath::Colon,
            ),
        ] {
            let version = with_game_args(args);
            for result in [
                processor_outputs(&version),
                checked_processor_outputs(&version),
            ] {
                match result {
                    Err(Error::UnsafePath {
                        entry,
                        path,
                        problem: got,
                    }) => {
                        assert_eq!(entry, format!("启动参数 {flag}"));
                        assert_eq!(path, value);
                        assert_eq!(got, problem, "{flag} {value}");
                    }
                    other => panic!("{flag} {value} 应报路径越界，实际：{other:?}"),
                }
            }
        }
    }
}
