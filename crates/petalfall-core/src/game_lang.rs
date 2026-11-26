//! 第一次启动前补全 options.txt：游戏语言跟随系统显示语言，默认跳过无障碍引导。
//!
//! 游戏自己不看系统语言，没有 options.txt 就是英文；现代版本第一次进游戏还要先走一遍无障碍引导。启动前在这里
//! 补上缺的行，新建、导入、补全出来的实例都走这一步。已有的行是玩家、整合包或默认设置定下的，意思一律不动；玩家
//! 在游戏里改过之后文件里自然有这些行，这里也就不再介入。唯一的例外是 `lang:` 的大小写要跟着这一局的版本走，
//! 见 [`respell_language`]。
//!
//! 语言码查这个版本的资源索引而不按版本号判断：同一种语言 1.11 起写 `zh_cn`，更早写 `zh_CN`，索引里的
//! 写法就是这个版本认的写法；哪个版本有哪些语言（`zh_hk` 只有新版才有）也一并由索引说了算。
//!
//! 整合包带着模组的默认设置（见 [`MOD_DEFAULT_OPTIONS`]）时，options.txt 要等模组第一次启动时铺出来，缺的行
//! 补进模组那份文件；那几份全是空的不算，照常新建。整合包里写着游戏默认语言的 `lang:` 行不算作者的选择，
//! 导入时去掉（见 [`drop_pack_default_language`]），之后与没写一样由这里补。补语言与去掉默认语言行都听
//! 配置里的 `follow_system_game_language`。补进模组那份文件的行按本机定，记账（见 [`FilledLines`]），
//! 导出整合包时去掉。

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use petalfall_install::GameLayout;
use petalfall_instance::PETALFALL_META_DIR;
use petalfall_version::AssetObjectsIndex;
use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result};
use crate::launch::read_asset_index;

const OPTIONS_FILE: &str = "options.txt";
const LANG_KEY: &[u8] = b"lang:";
const ONBOARDING_KEY: &[u8] = b"onboardAccessibility:";

/// 不认识这个键的旧版本会忽略这一行，所以不必按版本区分。
const SKIP_ONBOARDING_LINE: &str = "onboardAccessibility:false";

/// 游戏的 `OptionsAccessibilityOnboardFix` 挂在这个数据版本上：加载 options.txt 时，数据版本比它低的文件
/// 一律把引导标成走过，没有 `version:` 行按 0 算，所以只写了语言的新文件也在内。
const ONBOARDING_FIX_DATA_VERSION: u32 = 3319;

/// 游戏默认语言。它在客户端 jar 里，资源索引查不到，要在查索引之前认出来。
const GAME_DEFAULT: &str = "en_us";

/// 系统语言的地区在索引里没有时退到的语言码。这张表只写把握大的：表外的语言猜一个地区不如保持英文。
/// `nb` 单列：书面挪威语在游戏里叫 `no_no`，没有 `nb_no`。
const DEFAULT_LANGUAGES: [(&str, &str); 26] = [
    ("ar", "ar_sa"),
    ("cs", "cs_cz"),
    ("da", "da_dk"),
    ("de", "de_de"),
    ("el", "el_gr"),
    ("en", GAME_DEFAULT),
    ("es", "es_es"),
    ("fi", "fi_fi"),
    ("fr", "fr_fr"),
    ("he", "he_il"),
    ("hu", "hu_hu"),
    ("id", "id_id"),
    ("it", "it_it"),
    ("ja", "ja_jp"),
    ("ko", "ko_kr"),
    ("nb", "no_no"),
    ("nl", "nl_nl"),
    ("pl", "pl_pl"),
    ("pt", "pt_br"),
    ("ro", "ro_ro"),
    ("ru", "ru_ru"),
    ("sv", "sv_se"),
    ("th", "th_th"),
    ("tr", "tr_tr"),
    ("uk", "uk_ua"),
    ("vi", "vi_vn"),
];

/// 模组放默认设置的位置（YOSBR、Default Options）。这两个模组只在 options.txt 不存在时才把它铺过去，
/// 启动器抢先写一份，整合包作者调好的设置就全作废了。所以 options.txt 还没有时不新建，缺的行补进这份
/// 文件，由模组连同作者的设置一起铺过去。在场的几份全是空的除外（见 [`has_content`]）。
///
/// Default Options 有两处：自己的 options.txt，和 1.18 起的 `extra/`（里面的文件按相对路径整份铺到游戏
/// 目录）。两处谁先铺随模组版本而变，先铺的那份成为 options.txt，所以两份都要补。
const MOD_DEFAULT_OPTIONS: [&str; 3] = [
    "config/yosbr/options.txt",
    "config/defaultoptions/options.txt",
    "config/defaultoptions/extra/options.txt",
];

/// 这两个模组放默认设置的目录，与 [`MOD_DEFAULT_OPTIONS`] 逐项对应。目录里的文件由模组在第一次启动时铺到
/// 游戏目录的同名位置，目标已经存在就不铺：options.txt 之外的文件（服务器列表、OptiFine 的设置）也是这样。
pub(crate) const MOD_DEFAULTS_DIRS: [&str; 3] = [
    "config/yosbr",
    "config/defaultoptions",
    "config/defaultoptions/extra",
];

/// 系统语言能落到的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
enum GameLanguage {
    /// 写进 `lang:` 的语言码，大小写是索引里的原样。
    Code(String),
    /// 就是游戏默认语言，不用写。
    GameDefault,
    /// 这个版本的索引里没有对应语言。
    Unavailable,
}

/// 这个版本的索引里有的语言：全小写的语言码到索引里的原样写法。
fn index_languages(index: &AssetObjectsIndex) -> HashMap<String, &str> {
    index
        .objects
        .keys()
        .filter_map(|logical| language_code(logical))
        .map(|code| (code.to_ascii_lowercase(), code))
        .collect()
}

/// 系统语言标签在这个版本的资源索引里对应的语言。
fn resolve_game_language(system: &str, index: &AssetObjectsIndex) -> GameLanguage {
    let available = index_languages(index);
    for candidate in candidates(system) {
        if candidate == GAME_DEFAULT {
            return GameLanguage::GameDefault;
        }
        if let Some(code) = available.get(&candidate) {
            return GameLanguage::Code((*code).to_owned());
        }
    }
    GameLanguage::Unavailable
}

/// 资源逻辑名里的语言码。1.7 起是 `minecraft/lang/`，`legacy` 索引（1.6）是 `lang/`。`realms/lang/` 不算：
/// 那是 Realms 界面的翻译，有它不等于游戏有这门语言。
fn language_code(logical: &str) -> Option<&str> {
    let file = logical
        .strip_prefix("minecraft/lang/")
        .or_else(|| logical.strip_prefix("lang/"))?;
    file.strip_suffix(".json")
        .or_else(|| file.strip_suffix(".lang"))
}

/// 语言标签（`zh-Hans-CN`、`de-AT`）对应的语言码，全部小写，靠前的优先。
fn candidates(tag: &str) -> Vec<String> {
    let tag = tag.to_ascii_lowercase();
    let mut subtags = tag.split(['-', '_']);
    let Some(language) = subtags.next() else {
        return Vec::new();
    };
    // 单字母子标签之后是扩展（`-u-co-phonebk`），里面的两字母段不是地区。
    let subtags: Vec<&str> = subtags.take_while(|subtag| subtag.len() > 1).collect();
    let region = subtags.iter().copied().find(|subtag| subtag.len() == 2);
    if language == "zh" {
        return chinese_candidates(&subtags, region)
            .iter()
            .map(|code| (*code).to_owned())
            .collect();
    }
    let exact = region.map(|region| format!("{language}_{region}"));
    let default = DEFAULT_LANGUAGES
        .iter()
        .find(|(name, _)| *name == language)
        .map(|(_, code)| (*code).to_owned());
    exact.into_iter().chain(default).collect()
}

/// 中文按简繁分，不按地区名拼：`zh_sg` 这样的语言码并不存在，新加坡用的是简体。
fn chinese_candidates(subtags: &[&str], region: Option<&str>) -> &'static [&'static str] {
    if subtags.contains(&"hans") {
        return &["zh_cn"];
    }
    match region {
        Some("hk" | "mo") => &["zh_hk", "zh_tw"],
        Some("tw") => &["zh_tw"],
        _ if subtags.contains(&"hant") => &["zh_tw"],
        _ => &["zh_cn"],
    }
}

/// options.txt 里是否已有这个键的一行。按字节看：文件不一定是 UTF-8，读成字符串再写回会毁掉别的内容。
fn has_line(options: &[u8], key: &[u8]) -> bool {
    options
        .split(|byte| matches!(byte, b'\n' | b'\r'))
        .any(|line| line.starts_with(key))
}

/// 文件里有没有内容，只有空白的不算。空文件里没有谁调过的东西，也没有要迁移的内容，补语言时与新文件一样
/// 处理（见 [`fill_options`]）。YOSBR 每次启动都会建一份空的默认设置，整合包常把它原样带进来，模组铺过去的
/// 也是一份空文件：在场的几份全是这样时不为它们让路，另有一份有内容或读不了的时才连它一起补（见 [`fill`]）。
pub(crate) fn has_content(bytes: &[u8]) -> bool {
    bytes.iter().any(|byte| !byte.is_ascii_whitespace())
}

/// 文件不存在，或存在但没有这个键。
fn lacks(existing: Option<&[u8]>, key: &[u8]) -> bool {
    !existing.is_some_and(|options| has_line(options, key))
}

/// 文件里有默认语言以外的 `lang:` 行。
fn names_another_language(options: &[u8]) -> bool {
    options
        .split(|byte| matches!(byte, b'\n' | b'\r'))
        .filter_map(|line| line.strip_prefix(LANG_KEY))
        .any(|value| !value.eq_ignore_ascii_case(GAME_DEFAULT.as_bytes()))
}

/// `lang:` 行换成这个版本的大小写后的完整内容，什么都不用改时返回 `None`。
///
/// 默认隔离策略下原版实例共用一份 options.txt，而语言码的大小写 1.11 变过：新版留下的 `zh_cn` 老版不认，老版
/// 写法的 `zh_CN` 碰上文件里新版的 `version:` 也不走游戏自己的小写迁移，两头都退回英文。只差大小写还是同一门
/// 语言，换写法不算改玩家的选择；这个版本没有的语言原样留着。
fn respell_language(options: &[u8], languages: &HashMap<String, &str>) -> Option<Vec<u8>> {
    let mut written = options.to_vec();
    for line in written.split_mut(|byte| matches!(byte, b'\n' | b'\r')) {
        if !line.starts_with(LANG_KEY) {
            continue;
        }
        let value = &mut line[LANG_KEY.len()..];
        let known = std::str::from_utf8(value)
            .ok()
            .and_then(|code| languages.get(&code.to_ascii_lowercase()));
        if let Some(code) = known {
            value.copy_from_slice(code.as_bytes());
        }
    }
    (written != options).then_some(written)
}

/// 加上这些行后的完整内容：原内容逐字节保留，换行风格跟随原文件。
fn with_lines(options: &[u8], lines: &[String]) -> Vec<u8> {
    let newline: &[u8] = match options.iter().position(|byte| *byte == b'\n') {
        Some(end) if end > 0 && options[end - 1] == b'\r' => b"\r\n",
        _ => b"\n",
    };
    let mut written = options.to_vec();
    if !options.is_empty() && !options.ends_with(b"\n") {
        written.extend_from_slice(newline);
    }
    for line in lines {
        written.extend_from_slice(line.as_bytes());
        written.extend_from_slice(newline);
    }
    written
}

/// 要不要声明数据版本。玩家要看引导（开关关着）时，游戏读到一份没有 `version:` 行的设置文件就把引导标成
/// 走过（见 [`ONBOARDING_FIX_DATA_VERSION`]）。会让它读到这种文件的有三种：往新文件或空文件里只写了语言；
/// 文件已经在、是空的（只有空白）；空的模组默认设置等着被模组铺成 options.txt（`blank_defaults_pending`，
/// 这时新建一份声明了版本的 options.txt 挡在前面）。比这个版本老的游戏遇到更高的声明版本什么也不做，空文件
/// 里也没有要迁移的内容。有内容的已有文件不加：声明版本会让它跳过自己需要的迁移，引导弹不弹本来就由它自己
/// 的版本决定。文件不存在、也没有东西会被铺过来时不为这一行新建文件：游戏没有设置文件时自己会弹引导。
fn declares_data_version(
    existing: Option<&[u8]>,
    writes_language: bool,
    skip_onboarding: bool,
    blank_defaults_pending: bool,
) -> bool {
    !skip_onboarding
        && !existing.is_some_and(has_content)
        && (writes_language || existing.is_some() || blank_defaults_pending)
}

/// 补上缺的行后的完整内容，与补上的那几行（不含行尾）；`existing` 是现有内容（文件不存在时为 `None`），
/// 什么都不用补时返回 `None`。`blank_defaults_pending` 见 [`declares_data_version`]。
fn fill_options(
    existing: Option<&[u8]>,
    language: Option<&str>,
    skip_onboarding: bool,
    blank_defaults_pending: bool,
) -> Option<(Vec<u8>, Vec<String>)> {
    let language = language.filter(|_| lacks(existing, LANG_KEY));
    let mut lines = Vec::new();
    if declares_data_version(
        existing,
        language.is_some(),
        skip_onboarding,
        blank_defaults_pending,
    ) {
        lines.push(format!("version:{ONBOARDING_FIX_DATA_VERSION}"));
    }
    if let Some(code) = language {
        lines.push(format!("lang:{code}"));
    }
    if skip_onboarding && lacks(existing, ONBOARDING_KEY) {
        lines.push(SKIP_ONBOARDING_LINE.to_owned());
    }
    (!lines.is_empty()).then(|| (with_lines(existing.unwrap_or_default(), &lines), lines))
}

/// 第一行连同行尾的长度。`\r\n` 算一个行尾，单独的 `\r` 与 `\n` 各算一个，与 [`has_line`] 的切法一致。
fn first_line_len(bytes: &[u8]) -> usize {
    match bytes.iter().position(|byte| matches!(byte, b'\n' | b'\r')) {
        None => bytes.len(),
        Some(at) if bytes[at] == b'\r' && bytes.get(at + 1) == Some(&b'\n') => at + 2,
        Some(at) => at + 1,
    }
}

/// 去掉写着游戏默认语言的 `lang:` 行后的完整内容，其余逐字节保留；没有 `lang:` 行、或文件里还写着别的语言时
/// 返回 `None`。缺 `lang:` 时游戏用的就是默认语言，所以去掉这些行不改变游戏读到的结果。
fn without_default_language(options: &[u8]) -> Option<Vec<u8>> {
    if !has_line(options, LANG_KEY) || names_another_language(options) {
        return None;
    }
    let mut kept = Vec::with_capacity(options.len());
    let mut rest = options;
    while !rest.is_empty() {
        let (line, tail) = rest.split_at(first_line_len(rest));
        if !line.starts_with(LANG_KEY) {
            kept.extend_from_slice(line);
        }
        rest = tail;
    }
    Some(kept)
}

/// 文件的内容，不存在时为 `None`。
async fn read_optional(path: &Path) -> Result<Option<Vec<u8>>> {
    match tokio::fs::read(path).await {
        Ok(bytes) => Ok(Some(bytes)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(petalfall_base::Error::Io {
            path: path.to_owned(),
            source,
        }
        .into()),
    }
}

/// 去掉整合包带来的默认语言行，返回改过的文件（相对工作目录），按 options.txt、[`MOD_DEFAULT_OPTIONS`] 的顺序。
///
/// 整合包里的 `lang:en_us` 多半是作者把自己的 options.txt 整份带进来的，不是特意让玩家用英文；去掉它之后
/// 启动前的补全才会按系统语言来。写着别的语言的是作者有意选的，原样留着。只在导入的那一刻做：这时文件一定
/// 来自整合包，之后再出现的 `lang:en_us` 可能是玩家自己在游戏里选的。
///
/// 去掉后只剩空白的 options.txt 由 [`remove_blank_options_file`] 接着删掉。位置上不是普通文件的没有行可去，
/// 不为它让导入失败。
pub(crate) async fn drop_pack_default_language(working_dir: &Path) -> Result<Vec<&'static str>> {
    let mut changed = Vec::new();
    for relative in std::iter::once(OPTIONS_FILE).chain(MOD_DEFAULT_OPTIONS) {
        let path = working_dir.join(relative);
        if !is_regular_file(&path).await? {
            continue;
        }
        let Some(options) = read_optional(&path).await? else {
            continue;
        };
        let Some(content) = without_default_language(&options) else {
            continue;
        };
        petalfall_base::fs::atomic_write(&path, &content).await?;
        changed.push(relative);
    }
    Ok(changed)
}

/// 整合包带来的 options.txt 是空的（只有空白）时删掉它，返回删没删。本来就空的、去掉默认语言行后变空的都算，
/// 所以不看语言开关。空文件不是谁的设置，留着却处处碍事：游戏当它是老设置、直接把无障碍引导标成走过，模组见
/// 它已经在就不铺作者的默认设置，套默认设置也当它是包里自带的而不补模板。只在导入的那一刻做，这时文件一定
/// 来自整合包。模组那几份空的默认设置不删：补全与套默认设置都认得（见 [`has_content`]）。
pub(crate) async fn remove_blank_options_file(working_dir: &Path) -> Result<bool> {
    let path = working_dir.join(OPTIONS_FILE);
    if !is_regular_file(&path).await? {
        return Ok(false);
    }
    let Some(options) = read_optional(&path).await? else {
        return Ok(false);
    };
    if has_content(&options) {
        return Ok(false);
    }
    tokio::fs::remove_file(&path)
        .await
        .map_err(|source| petalfall_base::Error::Io { path, source })?;
    Ok(true)
}

/// 路径上是不是普通文件（不跟随符号链接）；不存在时为 `false`。
async fn is_regular_file(path: &Path) -> Result<bool> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(meta) => Ok(meta.is_file()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(petalfall_base::Error::Io {
            path: path.to_owned(),
            source,
        }
        .into()),
    }
}

/// 决定要补哪些行所需的启动时状态。
pub(crate) struct OptionsInputs<'a> {
    /// 系统显示语言的 BCP 47 标签；读不出来时为 `None`。
    pub(crate) system: Option<&'a str>,
    /// 这个版本的资源索引 id；版本没有资源索引时为 `None`。
    pub(crate) asset_index_id: Option<&'a str>,
    /// 配置里的 `skip_accessibility_onboarding`。
    pub(crate) skip_onboarding: bool,
    /// 配置里的 `follow_system_game_language`。关掉时不补 `lang:`；已有语言的大小写照样跟着版本走，那是让
    /// 文件里已有的选择在这个版本继续生效，不是替玩家选语言。
    pub(crate) follow_system: bool,
}

/// 补全的经过，由调用方写进启动日志。
#[derive(Debug, Default)]
pub(crate) struct OptionsReport {
    /// 做了什么，或语言为什么没写。
    pub(crate) notes: Vec<String>,
    /// 这一步没做成的原因。这里只有便利项，不该为它挡下启动，所以不向上冒泡。
    pub(crate) failures: Vec<CoreError>,
}

/// 补全工作目录里的 options.txt。
pub(crate) async fn fill_missing_options(
    inputs: &OptionsInputs<'_>,
    layout: &GameLayout,
    working_dir: &Path,
) -> OptionsReport {
    let mut report = OptionsReport::default();
    if let Err(err) = fill(inputs, layout, working_dir, &mut report).await {
        report.failures.push(err);
    }
    report
}

/// options.txt 在就补它；不在而模组的默认设置在，就补那份文件、不新建 options.txt（见
/// [`MOD_DEFAULT_OPTIONS`]）；都不在才新建。模组的默认设置全是空的也算不在（见 [`has_content`]）。
///
/// 只要有一份有内容（读不了的也按有内容算），在场的每一份连同空的都补：哪一份先被铺成 options.txt 由模组
/// 说了算（YOSBR 在加载器初始化时就铺，早于 Default Options），漏掉的恰是先铺的那份时，铺出来的
/// options.txt 里就没有语言。补进模组那份文件的行记进记账（见 [`FilledLines`]）。
async fn fill(
    inputs: &OptionsInputs<'_>,
    layout: &GameLayout,
    working_dir: &Path,
    report: &mut OptionsReport,
) -> Result<()> {
    let path = working_dir.join(OPTIONS_FILE);
    if let Some(existing) = read_optional(&path).await? {
        fill_file(&path, Some(&existing), inputs, layout, report, false).await?;
        return Ok(());
    }
    let mut pending = Vec::new();
    let mut unreadable = false;
    for relative in MOD_DEFAULT_OPTIONS {
        let defaults = working_dir.join(relative);
        match read_optional(&defaults).await {
            Ok(Some(existing)) => pending.push((relative, defaults, existing)),
            Ok(None) => {}
            // 读不了的说不清有没有内容，按有算：不新建 options.txt，读得了的几份照补。
            Err(err) => {
                unreadable = true;
                report.failures.push(err);
            }
        }
    }
    if !unreadable && !pending.iter().any(|(_, _, existing)| has_content(existing)) {
        // 在场的几份全是空的：不新建 options.txt 的话，模组会把其中一份空文件铺过去。
        let blank_defaults_pending = !pending.is_empty();
        fill_file(&path, None, inputs, layout, report, blank_defaults_pending).await?;
        return Ok(());
    }
    let mut filled = Vec::new();
    for (relative, defaults, existing) in &pending {
        // 哪一份先被铺过去说不准，一份写不进去不耽误别的；说明只在这一份没出错时才记，排在它自己的几条前面。
        let at = report.notes.len();
        match fill_file(defaults, Some(existing), inputs, layout, report, false).await {
            Ok(added) => {
                report.notes.insert(
                    at,
                    format!(
                        "options.txt 还没有，{relative} 会在第一次启动时铺过去，缺的行补进这份文件"
                    ),
                );
                if !added.is_empty() {
                    filled.push((*relative, added));
                }
            }
            Err(err) => report.failures.push(err),
        }
    }
    // 记账写不进去也不撤回补上的行：它们照样随模组铺进 options.txt，只是导出整合包时会跟着带走。
    record_filled_lines(working_dir, &filled).await
}

/// 补全一份文件，返回补上的那几行（不含行尾；只改了语言写法时为空）：`existing` 是它现有的内容，文件不存在时
/// 为 `None`。`blank_defaults_pending` 见 [`declares_data_version`]。
async fn fill_file(
    path: &Path,
    existing: Option<&[u8]>,
    inputs: &OptionsInputs<'_>,
    layout: &GameLayout,
    report: &mut OptionsReport,
    blank_defaults_pending: bool,
) -> Result<Vec<String>> {
    let language = if lacks(existing, LANG_KEY) {
        choose_language(inputs, layout, report).await
    } else {
        None
    };
    let respelled = match existing {
        Some(options) => respell_existing_language(options, inputs, layout, report).await,
        None => None,
    };
    let was_respelled = respelled.is_some();
    let filled = fill_options(
        respelled.as_deref().or(existing),
        language.as_deref(),
        inputs.skip_onboarding,
        blank_defaults_pending,
    );
    let (content, added) = match (filled, respelled) {
        (Some(filled), _) => filled,
        (None, Some(respelled)) => (respelled, Vec::new()),
        (None, None) => return Ok(Vec::new()),
    };
    petalfall_base::fs::atomic_write(path, &content).await?;
    if let (Some(code), Some(system)) = (&language, inputs.system) {
        report
            .notes
            .push(format!("游戏语言设为 {code}，跟随系统语言 {system}"));
    }
    if was_respelled {
        report
            .notes
            .push("游戏语言的写法换成这个版本的大小写".to_owned());
    }
    if inputs.skip_onboarding && lacks(existing, ONBOARDING_KEY) {
        report.notes.push("已跳过游戏的首次无障碍引导".to_owned());
    }
    // 只为引导写了数据版本时单独说一句：没写语言也没写引导那一行，不说就看不出文件为什么被动过。
    if language.is_none()
        && declares_data_version(
            existing,
            false,
            inputs.skip_onboarding,
            blank_defaults_pending,
        )
    {
        report
            .notes
            .push("设置文件是空的，已写上数据版本，免得游戏把首次无障碍引导标成走过".to_owned());
    }
    Ok(added)
}

/// 现有语言按这个版本的写法改过后的内容（见 [`respell_language`]）。索引读不出来只算这一项没做成，记进
/// `report`，引导那一行照写。
async fn respell_existing_language(
    options: &[u8],
    inputs: &OptionsInputs<'_>,
    layout: &GameLayout,
    report: &mut OptionsReport,
) -> Option<Vec<u8>> {
    // 默认语言不在索引里，也没有写法之分。英文玩家每局都走到这里，不为它读一次索引。
    if !names_another_language(options) {
        return None;
    }
    let id = inputs.asset_index_id?;
    match read_asset_index(layout, id).await {
        Ok(index) => respell_language(options, &index_languages(&index)),
        Err(err) => {
            report.failures.push(err);
            None
        }
    }
}

/// 这个版本的 `lang:` 该写什么。写不了时为 `None`，原因记进 `report`：索引读不出来只算语言这一项没做成，
/// 引导那一行照写。
async fn choose_language(
    inputs: &OptionsInputs<'_>,
    layout: &GameLayout,
    report: &mut OptionsReport,
) -> Option<String> {
    if !inputs.follow_system {
        report
            .notes
            .push("「游戏语言跟随系统语言」已在设置里关闭，游戏语言保持默认".to_owned());
        return None;
    }
    let Some(system) = inputs.system else {
        report
            .notes
            .push("读不出系统语言，游戏语言保持默认".to_owned());
        return None;
    };
    let Some(id) = inputs.asset_index_id else {
        report
            .notes
            .push("这个版本没有资源索引，游戏语言保持默认".to_owned());
        return None;
    };
    let index = match read_asset_index(layout, id).await {
        Ok(index) => index,
        Err(err) => {
            report.failures.push(err);
            return None;
        }
    };
    match resolve_game_language(system, &index) {
        GameLanguage::Code(code) => Some(code),
        GameLanguage::GameDefault => None,
        GameLanguage::Unavailable => {
            report.notes.push(format!(
                "这个版本的资源索引里没有系统语言 {system}，游戏语言保持默认"
            ));
            None
        }
    }
}

/// 记账文件（见 [`FilledLines`]），放在工作目录的元数据目录下。
const FILLED_LINES_FILE: &str = "filled-options.json";

/// 记账的读改写排队：不隔离的实例共用一个工作目录，同时启动时不排队，后写的会盖掉先写的。
static FILLED_LINES_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 记账：启动器补进模组默认设置的行，按文件记。这几行按本机的系统语言与启动器设置补，却落在整合包自己的文件里，
/// 导出整合包时要去掉（见 [`without_lines`]），否则收到包的人会把它们当成作者选的。options.txt 不记：导出它
/// 本来就是把玩家自己的设置带走。
///
/// 放在工作目录的元数据目录下，与那几份文件同在一个工作目录：不隔离的实例工作目录是游戏目录，共用的是同一批
/// 模组默认设置，也就共用这一份记账。
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct FilledLines {
    /// [`MOD_DEFAULT_OPTIONS`] 里的相对路径到补上的整行（不含行尾），按补上的先后，同一行只记一次。
    files: BTreeMap<String, Vec<String>>,
}

impl FilledLines {
    /// 记在这份文件名下的行；`relative` 是工作目录下的正斜杠相对路径，不分大小写（Windows 上只差大小写的是同一个
    /// 文件）。不是模组的默认设置、或没记过时为 `None`。
    pub(crate) fn lines_for(&self, relative: &str) -> Option<&[String]> {
        let known = MOD_DEFAULT_OPTIONS
            .into_iter()
            .find(|path| path.eq_ignore_ascii_case(relative))?;
        self.files.get(known).map(Vec::as_slice)
    }
}

pub(crate) fn filled_lines_path(working_dir: &Path) -> PathBuf {
    working_dir.join(PETALFALL_META_DIR).join(FILLED_LINES_FILE)
}

/// 工作目录里的记账，没有记账文件时为空。读不懂照实报错，不当成没记过：当成没记过，补的行就随包带走了。
pub(crate) async fn read_filled_lines(working_dir: &Path) -> Result<FilledLines> {
    let path = filled_lines_path(working_dir);
    match read_optional(&path).await? {
        Some(bytes) => serde_json::from_slice(&bytes)
            .map_err(|source| CoreError::JsonFileInvalid { path, source }),
        None => Ok(FilledLines::default()),
    }
}

/// 把这次补进各份模组默认设置的行（`filled`：相对路径与补上的行）并进记账，已记的不重复记；没有新记的不写。
/// 读不懂的旧记账不覆盖：里面记着的行导出时还要靠它去掉。
async fn record_filled_lines(working_dir: &Path, filled: &[(&str, Vec<String>)]) -> Result<()> {
    if filled.is_empty() {
        return Ok(());
    }
    let _turn = FILLED_LINES_LOCK.lock().await;
    let mut record = read_filled_lines(working_dir).await?;
    let mut changed = false;
    for (relative, lines) in filled {
        let recorded = record.files.entry((*relative).to_owned()).or_default();
        for line in lines {
            if !recorded.contains(line) {
                recorded.push(line.clone());
                changed = true;
            }
        }
    }
    if !changed {
        return Ok(());
    }
    let bytes = serde_json::to_vec_pretty(&record).map_err(CoreError::JsonSerialize)?;
    petalfall_base::fs::atomic_write(filled_lines_path(working_dir), &bytes).await?;
    Ok(())
}

/// 去掉 `lines` 里此刻仍原样是完整一行的那几行（连同行尾）后的内容，其余逐字节保留；一行都没去掉时返回 `None`。
///
/// 每条最多去掉一行：记账之后文件可能被人改过，记着的行不在了就不去，也不拿别的行顶替。补的行都追加在末尾，
/// 同样的一行有几处时去掉最后一处。行的切法与 [`first_line_len`] 一致。
pub(crate) fn without_lines(content: &[u8], lines: &[String]) -> Option<Vec<u8>> {
    let mut spans = Vec::new();
    let mut start = 0;
    while start < content.len() {
        let end = start + first_line_len(&content[start..]);
        spans.push(start..end);
        start = end;
    }
    let mut dropped = vec![false; spans.len()];
    for line in lines {
        let last = (0..spans.len())
            .rev()
            .find(|&at| !dropped[at] && line_text(&content[spans[at].clone()]) == line.as_bytes());
        if let Some(at) = last {
            dropped[at] = true;
        }
    }
    if !dropped.contains(&true) {
        return None;
    }
    let mut kept = Vec::with_capacity(content.len());
    for (span, dropped) in spans.into_iter().zip(dropped) {
        if !dropped {
            kept.extend_from_slice(&content[span]);
        }
    }
    Some(kept)
}

/// 一行去掉行尾后的内容。
fn line_text(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use petalfall_version::AssetObject;
    use std::collections::BTreeMap;

    /// 下面几组键节选自官方资源索引，覆盖了语言键写法变化的每一处。
    /// 1.5.2（pre-1.6）：索引里没有语言文件，语言在客户端 jar 里。
    const V1_5_2: &[&str] = &["sound/step/grass1.ogg", "music/calm1.ogg"];
    /// 1.6.4（legacy）：没有 `minecraft/` 前缀。
    const V1_6_4: &[&str] = &[
        "lang/de_DE.lang",
        "lang/en_GB.lang",
        "lang/nn_NO.lang",
        "lang/no_NO.lang",
        "lang/pt_BR.lang",
        "lang/pt_PT.lang",
        "lang/zh_CN.lang",
        "lang/zh_TW.lang",
    ];
    /// 1.7.10：`minecraft/` 与 `realms/` 各有一份，大小写是 `zh_CN`。
    const V1_7_10: &[&str] = &[
        "minecraft/lang/de_DE.lang",
        "minecraft/lang/en_GB.lang",
        "minecraft/lang/fr_FR.lang",
        "minecraft/lang/zh_CN.lang",
        "minecraft/lang/zh_TW.lang",
        "realms/lang/de_DE.lang",
        "realms/lang/zh_CN.lang",
    ];
    /// 1.8.9 写法同 1.7.10。
    const V1_8_9: &[&str] = &["minecraft/lang/zh_CN.lang", "minecraft/lang/zh_TW.lang"];
    /// 1.10.2：大写区域的最后一个大版本。
    const V1_10_2: &[&str] = &[
        "minecraft/lang/zh_CN.lang",
        "minecraft/lang/zh_TW.lang",
        "realms/lang/zh_CN.lang",
    ];
    /// 1.11.2：大小写换成 `zh_cn`，扩展名还是 `.lang`。
    const V1_11_2: &[&str] = &[
        "minecraft/lang/de_at.lang",
        "minecraft/lang/de_de.lang",
        "minecraft/lang/zh_cn.lang",
        "minecraft/lang/zh_tw.lang",
        "realms/lang/zh_cn.lang",
    ];
    /// 1.12.2 写法同 1.11.2。
    const V1_12_2: &[&str] = &["minecraft/lang/zh_cn.lang", "minecraft/lang/zh_tw.lang"];
    /// 1.13.2：扩展名换成 `.json`，还没有 `zh_hk`。
    const V1_13_2: &[&str] = &[
        "minecraft/lang/zh_cn.json",
        "minecraft/lang/zh_tw.json",
        "realms/lang/zh_cn.json",
    ];
    /// 1.20.1：有 `zh_hk`，`en_us` 在客户端 jar 里，索引里没有。
    const V1_20_1: &[&str] = &[
        "minecraft/lang/ca_es.json",
        "minecraft/lang/de_at.json",
        "minecraft/lang/de_de.json",
        "minecraft/lang/en_gb.json",
        "minecraft/lang/es_es.json",
        "minecraft/lang/es_mx.json",
        "minecraft/lang/fr_fr.json",
        "minecraft/lang/ja_jp.json",
        "minecraft/lang/ko_kr.json",
        "minecraft/lang/lzh.json",
        "minecraft/lang/nn_no.json",
        "minecraft/lang/no_no.json",
        "minecraft/lang/pt_br.json",
        "minecraft/lang/pt_pt.json",
        "minecraft/lang/ru_ru.json",
        "minecraft/lang/zh_cn.json",
        "minecraft/lang/zh_hk.json",
        "minecraft/lang/zh_tw.json",
    ];
    /// 1.21.1 的 `realms/` 下只有一个 `en_us`，它不是游戏语言；26.3 也是这样。
    const V1_21_1: &[&str] = &[
        "minecraft/lang/zh_cn.json",
        "minecraft/lang/zh_hk.json",
        "minecraft/lang/zh_tw.json",
        "realms/lang/en_us.json",
    ];

    fn index(keys: &[&str]) -> AssetObjectsIndex {
        let objects: BTreeMap<String, AssetObject> = keys
            .iter()
            .map(|key| {
                let object = AssetObject {
                    hash: "ab".repeat(20),
                    size: 1,
                };
                ((*key).to_owned(), object)
            })
            .collect();
        AssetObjectsIndex {
            objects,
            ..AssetObjectsIndex::default()
        }
    }

    fn code(code: &str) -> GameLanguage {
        GameLanguage::Code(code.to_owned())
    }

    #[test]
    fn system_language_resolves_against_the_index_of_each_version() {
        use GameLanguage::{GameDefault, Unavailable};
        let cases: &[(&str, &[&str], GameLanguage)] = &[
            // 简体：各种写法归 zh_cn，大小写跟着索引走。
            ("zh-CN", V1_21_1, code("zh_cn")),
            ("zh-CN", V1_20_1, code("zh_cn")),
            ("zh-CN", V1_13_2, code("zh_cn")),
            ("zh-CN", V1_12_2, code("zh_cn")),
            ("zh-CN", V1_11_2, code("zh_cn")),
            ("zh-CN", V1_10_2, code("zh_CN")),
            ("zh-CN", V1_8_9, code("zh_CN")),
            ("zh-CN", V1_7_10, code("zh_CN")),
            ("zh-CN", V1_6_4, code("zh_CN")),
            ("zh-Hans", V1_20_1, code("zh_cn")),
            ("zh-Hans-CN", V1_20_1, code("zh_cn")),
            ("zh-Hans-HK", V1_20_1, code("zh_cn")),
            ("zh-SG", V1_20_1, code("zh_cn")),
            ("zh-Hans-SG", V1_7_10, code("zh_CN")),
            ("zh", V1_20_1, code("zh_cn")),
            ("zh_CN", V1_20_1, code("zh_cn")),
            // 繁体。
            ("zh-TW", V1_20_1, code("zh_tw")),
            ("zh-TW", V1_7_10, code("zh_TW")),
            ("zh-Hant", V1_20_1, code("zh_tw")),
            ("zh-Hant-TW", V1_6_4, code("zh_TW")),
            // 香港优先 zh_hk，老版本没有时退到 zh_tw；澳门同理。
            ("zh-HK", V1_20_1, code("zh_hk")),
            ("zh-Hant-HK", V1_21_1, code("zh_hk")),
            ("zh-MO", V1_20_1, code("zh_hk")),
            ("zh-HK", V1_13_2, code("zh_tw")),
            ("zh-HK", V1_11_2, code("zh_tw")),
            ("zh-HK", V1_7_10, code("zh_TW")),
            // 索引里没有 en_us，但它就是游戏默认语言，不算找不到。
            ("en-US", V1_21_1, GameDefault),
            ("en-US", V1_7_10, GameDefault),
            ("en-US", V1_5_2, GameDefault),
            ("en", V1_20_1, GameDefault),
            ("en-IN", V1_20_1, GameDefault),
            ("en-NZ", V1_7_10, GameDefault),
            ("en-GB", V1_20_1, code("en_gb")),
            ("en-GB", V1_7_10, code("en_GB")),
            // 其它语言先精确匹配，没有就退到默认区域。
            ("de-AT", V1_20_1, code("de_at")),
            ("de-AT", V1_7_10, code("de_DE")),
            ("de-LU", V1_20_1, code("de_de")),
            ("de-DE-u-co-phonebk", V1_20_1, code("de_de")),
            ("fr-BE", V1_20_1, code("fr_fr")),
            ("es-MX", V1_20_1, code("es_mx")),
            ("es-419", V1_20_1, code("es_es")),
            ("pt-PT", V1_20_1, code("pt_pt")),
            ("pt-AO", V1_20_1, code("pt_br")),
            ("pt", V1_6_4, code("pt_BR")),
            ("ja", V1_20_1, code("ja_jp")),
            ("ko-KR", V1_20_1, code("ko_kr")),
            ("ru-BY", V1_20_1, code("ru_ru")),
            ("ca-ES-valencia", V1_20_1, code("ca_es")),
            // 书面挪威语在游戏里叫 no_no，索引里没有 nb_no。
            ("nb-NO", V1_20_1, code("no_no")),
            ("nb-NO", V1_6_4, code("no_NO")),
            ("nn-NO", V1_20_1, code("nn_no")),
            // 表外的语言没有精确匹配就不写，索引里没有的语言也不写。
            ("sw-KE", V1_20_1, Unavailable),
            ("xx", V1_20_1, Unavailable),
            ("", V1_20_1, Unavailable),
            ("zh-CN", V1_5_2, Unavailable),
            ("zh-CN", &[], Unavailable),
            ("ko-KR", &["minecraft/lang/zh_cn.json"], Unavailable),
            ("zh-HK", &["minecraft/lang/zh_cn.json"], Unavailable),
            // realms 的语言文件不是游戏语言。
            ("fr-FR", &["realms/lang/fr_fr.json"], Unavailable),
            ("zh-CN", &["realms/lang/zh_cn.lang"], Unavailable),
            // 语言目录外面的同名文件也不是。
            ("zh-CN", &["minecraft/texts/zh_cn.json"], Unavailable),
            ("zh-CN", &["minecraft/lang/zh_cn.png"], Unavailable),
        ];
        for (system, keys, expected) in cases {
            assert_eq!(
                resolve_game_language(system, &index(keys)),
                *expected,
                "系统语言 {system}，索引 {keys:?}"
            );
        }
    }

    /// 与 [`fill`] 同一条路：系统语言先查索引，查到的语言码再和现有内容合成要写的内容。
    fn filled(
        system: &str,
        keys: &[&str],
        existing: Option<&[u8]>,
        skip_onboarding: bool,
    ) -> Option<Vec<u8>> {
        let language = match resolve_game_language(system, &index(keys)) {
            GameLanguage::Code(code) => Some(code),
            GameLanguage::GameDefault | GameLanguage::Unavailable => None,
        };
        fill_options(existing, language.as_deref(), skip_onboarding, false)
            .map(|(content, _)| content)
    }

    fn text(content: &str) -> Option<&[u8]> {
        Some(content.as_bytes())
    }

    #[test]
    fn missing_lines_are_filled_in_one_write_and_existing_ones_stay() {
        type Case<'a> = (
            &'a str,
            &'a [&'a str],
            Option<&'a [u8]>,
            bool,
            Option<&'a [u8]>,
        );
        const SKIP: &str = "onboardAccessibility:false\n";
        let cases: &[Case] = &[
            // 文件不存在：语言与引导写进同一份内容，语言码的大小写跟着索引。
            (
                "zh-CN",
                V1_21_1,
                None,
                true,
                text("lang:zh_cn\nonboardAccessibility:false\n"),
            ),
            (
                "zh-CN",
                V1_7_10,
                None,
                true,
                text("lang:zh_CN\nonboardAccessibility:false\n"),
            ),
            (
                "zh-CN",
                V1_6_4,
                None,
                true,
                text("lang:zh_CN\nonboardAccessibility:false\n"),
            ),
            (
                "zh-TW",
                V1_20_1,
                None,
                true,
                text("lang:zh_tw\nonboardAccessibility:false\n"),
            ),
            (
                "zh-HK",
                V1_21_1,
                None,
                true,
                text("lang:zh_hk\nonboardAccessibility:false\n"),
            ),
            (
                "zh-HK",
                V1_13_2,
                None,
                true,
                text("lang:zh_tw\nonboardAccessibility:false\n"),
            ),
            // 不写语言时引导照写。
            ("en-US", V1_21_1, None, true, text(SKIP)),
            ("sw-KE", V1_21_1, None, true, text(SKIP)),
            ("zh-CN", V1_5_2, None, true, text(SKIP)),
            // 开关关掉：不写引导。新文件只有语言时带上数据版本，否则游戏会把引导标成走过。
            (
                "zh-CN",
                V1_21_1,
                None,
                false,
                text("version:3319\nlang:zh_cn\n"),
            ),
            (
                "zh-CN",
                V1_7_10,
                None,
                false,
                text("version:3319\nlang:zh_CN\n"),
            ),
            ("en-US", V1_21_1, None, false, None),
            ("sw-KE", V1_21_1, None, false, None),
            // 已有文件（有内容）：缺哪行补哪行，换行风格跟随原文件，不额外声明数据版本。
            (
                "zh-CN",
                V1_21_1,
                text("version:3955\r\nfov:0.5\r\n"),
                true,
                text("version:3955\r\nfov:0.5\r\nlang:zh_cn\r\nonboardAccessibility:false\r\n"),
            ),
            (
                "zh-CN",
                V1_21_1,
                text("fov:0.5\n"),
                true,
                text("fov:0.5\nlang:zh_cn\nonboardAccessibility:false\n"),
            ),
            (
                "zh-CN",
                V1_21_1,
                text("fov:0.5\n"),
                false,
                text("fov:0.5\nlang:zh_cn\n"),
            ),
            (
                "en-US",
                V1_21_1,
                text("fov:0.5\r\n"),
                true,
                text("fov:0.5\r\nonboardAccessibility:false\r\n"),
            ),
            ("en-US", V1_21_1, text("fov:0.5\r\n"), false, None),
            // 已有文件是空的（只有空白）：里面没有要迁移的内容，开关关掉时带上数据版本，没有语言可写也带，
            // 不然游戏读到这份空文件就把引导标成走过；原有的空白逐字节保留，换行跟着它走。
            (
                "zh-CN",
                V1_21_1,
                text(""),
                false,
                text("version:3319\nlang:zh_cn\n"),
            ),
            (
                "zh-CN",
                V1_21_1,
                text("\r\n \t\r\n"),
                false,
                text("\r\n \t\r\nversion:3319\r\nlang:zh_cn\r\n"),
            ),
            (
                "zh-CN",
                V1_21_1,
                text(""),
                true,
                text("lang:zh_cn\nonboardAccessibility:false\n"),
            ),
            ("en-US", V1_21_1, text(""), false, text("version:3319\n")),
            (
                "en-US",
                V1_21_1,
                text("\r\n"),
                false,
                text("\r\nversion:3319\r\n"),
            ),
            (
                "en-US",
                V1_21_1,
                text("\n"),
                true,
                text("\nonboardAccessibility:false\n"),
            ),
            // 已有 lang 不动，哪怕和系统语言不一样。
            (
                "zh-CN",
                V1_21_1,
                text("lang:en_us\r\nfov:0.5\r\n"),
                true,
                text("lang:en_us\r\nfov:0.5\r\nonboardAccessibility:false\r\n"),
            ),
            (
                "zh-CN",
                V1_21_1,
                text("lang:en_us\r\nfov:0.5\r\n"),
                false,
                None,
            ),
            // 已有引导行不动，不管是开是关、开关怎么设。
            (
                "zh-CN",
                V1_21_1,
                text("onboardAccessibility:true\n"),
                true,
                text("onboardAccessibility:true\nlang:zh_cn\n"),
            ),
            (
                "zh-CN",
                V1_21_1,
                text("onboardAccessibility:false\n"),
                false,
                text("onboardAccessibility:false\nlang:zh_cn\n"),
            ),
            // 两行都有时什么都不写。
            (
                "zh-CN",
                V1_21_1,
                text("lang:de_de\nonboardAccessibility:true\n"),
                true,
                None,
            ),
            (
                "zh-CN",
                V1_21_1,
                text("lang:de_de\nonboardAccessibility:true\n"),
                false,
                None,
            ),
        ];
        for (system, keys, existing, skip, expected) in cases {
            assert_eq!(
                filled(system, keys, *existing, *skip).as_deref(),
                *expected,
                "系统语言 {system}，原内容 {:?}，开关 {skip}",
                existing.map(String::from_utf8_lossy),
            );
        }
    }

    #[test]
    fn only_a_real_line_counts_as_already_set() {
        let cases: &[(&[u8], &[u8], bool)] = &[
            (b"lang:en_us\n", LANG_KEY, true),
            (b"fov:90\nlang:de_de\nguiScale:2\n", LANG_KEY, true),
            (b"fov:90\r\nlang:zh_CN\r\nguiScale:2\r\n", LANG_KEY, true),
            (b"fov:90\nlang:ja_jp", LANG_KEY, true),
            // 值是空的也是一行 lang，不替玩家判断它对不对。
            (b"lang:\n", LANG_KEY, true),
            (b"\xff\xfe:\xd6\xd0\nlang:zh_cn\n", LANG_KEY, true),
            (b"", LANG_KEY, false),
            (b"fov:90\n", LANG_KEY, false),
            (b"fov:90\r\nguiScale:2\r\n", LANG_KEY, false),
            (b"language:zh_cn\n", LANG_KEY, false),
            (b"xlang:zh_cn\n", LANG_KEY, false),
            (b" lang:zh_cn\n", LANG_KEY, false),
            (b"Lang:zh_cn\n", LANG_KEY, false),
            (b"key_key.lang:key.keyboard.l\n", LANG_KEY, false),
            (b"lang\n", LANG_KEY, false),
            (b"onboardAccessibility:true\n", ONBOARDING_KEY, true),
            (
                b"fov:90\r\nonboardAccessibility:false\r\n",
                ONBOARDING_KEY,
                true,
            ),
            (b"onboardAccessibility:", ONBOARDING_KEY, true),
            (b"fov:90\n", ONBOARDING_KEY, false),
            (b"onboardAccessibility\n", ONBOARDING_KEY, false),
            (b"xonboardAccessibility:false\n", ONBOARDING_KEY, false),
        ];
        for (options, key, expected) in cases {
            assert_eq!(
                has_line(options, key),
                *expected,
                "{:?}",
                String::from_utf8_lossy(options)
            );
        }
    }

    #[test]
    fn existing_language_takes_the_spelling_of_the_version_being_launched() {
        type Case<'a> = (&'a [u8], &'a [&'a str], Option<&'a [u8]>);
        let cases: &[Case] = &[
            // 新版写下的小写，老版不认，会退回英文。
            (
                b"lang:zh_cn\nonboardAccessibility:false\n",
                V1_8_9,
                Some(b"lang:zh_CN\nonboardAccessibility:false\n"),
            ),
            (b"lang:zh_cn\n", V1_7_10, Some(b"lang:zh_CN\n")),
            (b"lang:zh_cn\n", V1_6_4, Some(b"lang:zh_CN\n")),
            (
                b"lang:zh_tw\r\nfov:0.5\r\n",
                V1_7_10,
                Some(b"lang:zh_TW\r\nfov:0.5\r\n"),
            ),
            (b"lang:de_de\n", V1_7_10, Some(b"lang:de_DE\n")),
            // 反过来：带着 version: 行的大写，新版不走自己的小写迁移，同样退回英文。
            (
                b"version:2586\nlang:zh_CN\n",
                V1_20_1,
                Some(b"version:2586\nlang:zh_cn\n"),
            ),
            (
                b"lang:zh_CN\nfov:0.5",
                V1_12_2,
                Some(b"lang:zh_cn\nfov:0.5"),
            ),
            // 其余内容逐字节留着，换行也不动。
            (
                b"\xff\xfe:1\r\nlang:zh_cn\r\nfov:0.5",
                V1_8_9,
                Some(b"\xff\xfe:1\r\nlang:zh_CN\r\nfov:0.5"),
            ),
            // 写法已经是这个版本的：不用写。
            (b"lang:zh_CN\n", V1_8_9, None),
            (b"lang:zh_cn\n", V1_20_1, None),
            // 不是同一门语言，或这个版本没有这门语言：不替玩家换语言。
            (b"lang:ja_jp\n", V1_8_9, None),
            (b"lang:zh_hk\n", V1_7_10, None),
            (b"lang:en_us\n", V1_7_10, None),
            (b"lang:\n", V1_8_9, None),
            (b"lang:\xd6\xd0\n", V1_8_9, None),
            // 索引里没有语言（1.5.2）：无从判断写法。
            (b"lang:zh_cn\n", V1_5_2, None),
            // realms 的语言文件不算。
            (b"lang:zh_cn\n", &["realms/lang/zh_CN.lang"], None),
            // 名字里带 lang 的别的键。
            (b"xlang:zh_cn\nkey_key.lang:zh_cn\n", V1_8_9, None),
            (b"fov:0.5\n", V1_8_9, None),
            (b"", V1_8_9, None),
        ];
        for (options, keys, expected) in cases {
            assert_eq!(
                respell_language(options, &index_languages(&index(keys))).as_deref(),
                *expected,
                "原内容 {:?}，索引 {keys:?}",
                String::from_utf8_lossy(options),
            );
        }
    }

    /// 随机内容：改写法只动 `lang:` 行的值，文件长度不变，其它位置一个字节不差。
    #[test]
    fn respelling_only_touches_the_language_value() {
        let languages = index(V1_8_9);
        let languages = index_languages(&languages);
        for _ in 0..200 {
            let noise = || -> Vec<u8> {
                std::iter::repeat_with(|| fastrand::u8(..))
                    .filter(|byte| !matches!(byte, b'\n' | b'\r'))
                    .take(fastrand::usize(0..512))
                    .collect()
            };
            let (before, after) = (noise(), noise());
            let lang = b"\nlang:zh_cn\n";
            let options = [&before[..], lang, &after[..]].concat();

            let written = respell_language(&options, &languages).unwrap();

            let expected = [&before[..], b"\nlang:zh_CN\n", &after[..]].concat();
            assert_eq!(written, expected);
        }
    }

    #[test]
    fn added_lines_follow_the_newline_style_of_the_file() {
        let cases: &[(&[u8], &[u8])] = &[
            (b"", b"lang:zh_cn\n"),
            (b"fov:90\n", b"fov:90\nlang:zh_cn\n"),
            (b"fov:90", b"fov:90\nlang:zh_cn\n"),
            (
                b"fov:90\r\nguiScale:2\r\n",
                b"fov:90\r\nguiScale:2\r\nlang:zh_cn\r\n",
            ),
            (
                b"fov:90\r\nguiScale:2",
                b"fov:90\r\nguiScale:2\r\nlang:zh_cn\r\n",
            ),
            // 不是 UTF-8 的内容也要原样留着。
            (b"\xd6\xd0:1\r\n", b"\xd6\xd0:1\r\nlang:zh_cn\r\n"),
        ];
        for (options, expected) in cases {
            assert_eq!(
                with_lines(options, &["lang:zh_cn".to_owned()]),
                *expected,
                "{:?}",
                String::from_utf8_lossy(options)
            );
        }
    }

    /// 随机内容：原文件必须一个字节不差地留在开头，新增的只有末尾缺的行。
    #[test]
    fn appending_keeps_every_original_byte() {
        let lines = ["lang:zh_CN".to_owned(), SKIP_ONBOARDING_LINE.to_owned()];
        for _ in 0..200 {
            let options: Vec<u8> = std::iter::repeat_with(|| fastrand::u8(..))
                .take(fastrand::usize(0..2048))
                .collect();
            let written = with_lines(&options, &lines);
            assert!(written.starts_with(&options));
            let added = &written[options.len()..];
            assert!(
                [
                    &b"lang:zh_CN\nonboardAccessibility:false\n"[..],
                    b"lang:zh_CN\r\nonboardAccessibility:false\r\n",
                    b"\nlang:zh_CN\nonboardAccessibility:false\n",
                    b"\r\nlang:zh_CN\r\nonboardAccessibility:false\r\n",
                ]
                .contains(&added),
                "{added:?}"
            );
            assert!(has_line(&written, LANG_KEY));
            assert!(has_line(&written, ONBOARDING_KEY));
        }
    }

    struct Scene {
        _tmp: tempfile::TempDir,
        layout: GameLayout,
        working_dir: std::path::PathBuf,
    }

    /// 索引 `lang-test` 在本地，内容是给定的键。
    fn scene(keys: &[&str]) -> Scene {
        let tmp = tempfile::tempdir().unwrap();
        let layout = GameLayout::new(tmp.path().join(".minecraft"));
        let path = layout.asset_index_json("lang-test").unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_vec(&index(keys)).unwrap()).unwrap();
        let working_dir = tmp.path().join("instance");
        std::fs::create_dir_all(&working_dir).unwrap();
        Scene {
            _tmp: tmp,
            layout,
            working_dir,
        }
    }

    impl Scene {
        async fn fill_with(
            &self,
            system: Option<&str>,
            asset_index_id: Option<&str>,
            skip_onboarding: bool,
        ) -> OptionsReport {
            let inputs = OptionsInputs {
                system,
                asset_index_id,
                skip_onboarding,
                follow_system: true,
            };
            fill_missing_options(&inputs, &self.layout, &self.working_dir).await
        }

        /// 关掉「游戏语言跟随系统语言」的一次补全；系统语言是中文，索引也在，只差这个开关。
        async fn fill_without_following(&self, skip_onboarding: bool) -> OptionsReport {
            let inputs = OptionsInputs {
                system: Some("zh-CN"),
                asset_index_id: Some("lang-test"),
                skip_onboarding,
                follow_system: false,
            };
            fill_missing_options(&inputs, &self.layout, &self.working_dir).await
        }

        /// 默认配置下的一次补全，不该有任何失败。
        async fn fill(&self, system: Option<&str>) -> Vec<String> {
            let report = self.fill_with(system, Some("lang-test"), true).await;
            assert!(report.failures.is_empty(), "{:?}", report.failures);
            report.notes
        }

        fn write(&self, relative: &str, bytes: &[u8]) {
            let path = self.working_dir.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }

        fn options(&self) -> Option<Vec<u8>> {
            self.read(OPTIONS_FILE)
        }

        fn read(&self, relative: &str) -> Option<Vec<u8>> {
            match std::fs::read(self.working_dir.join(relative)) {
                Ok(bytes) => Some(bytes),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
                Err(err) => panic!("读 {relative} 失败：{err}"),
            }
        }
    }

    const SKIPPED: &str = "已跳过游戏的首次无障碍引导";
    const RESPELLED: &str = "游戏语言的写法换成这个版本的大小写";

    #[tokio::test]
    async fn missing_options_file_is_created_with_both_lines() {
        let scene = scene(V1_7_10);
        let notes = scene.fill(Some("zh-CN")).await;
        assert_eq!(
            scene.options().unwrap(),
            b"lang:zh_CN\nonboardAccessibility:false\n"
        );
        assert_eq!(notes, ["游戏语言设为 zh_CN，跟随系统语言 zh-CN", SKIPPED]);
    }

    #[tokio::test]
    async fn options_file_gets_the_missing_lines_appended() {
        let scene = scene(V1_21_1);
        let original = b"version:3955\r\nfov:0.5\r\n\xff\xfe:1\r\nguiScale:2";
        scene.write(OPTIONS_FILE, original);

        scene.fill(Some("zh-HK")).await;

        let mut expected = original.to_vec();
        expected.extend_from_slice(b"\r\nlang:zh_hk\r\nonboardAccessibility:false\r\n");
        assert_eq!(scene.options().unwrap(), expected);
    }

    /// 共用 options.txt 的版本之间换着玩：语言不变，写法换成这一局的版本认的。
    #[tokio::test]
    async fn existing_language_is_respelled_for_the_version_being_launched() {
        let scene = scene(V1_8_9);
        scene.write(OPTIONS_FILE, b"lang:zh_cn\nonboardAccessibility:false\n");

        let notes = scene.fill(Some("en-US")).await;

        assert_eq!(
            scene.options().unwrap(),
            b"lang:zh_CN\nonboardAccessibility:false\n"
        );
        assert_eq!(notes, [RESPELLED]);
    }

    /// 改写法和补引导是同一次写入。
    #[tokio::test]
    async fn respelling_and_the_missing_onboarding_line_land_together() {
        let scene = scene(V1_7_10);
        scene.write(OPTIONS_FILE, b"lang:zh_tw\r\nfov:0.5\r\n");

        let notes = scene.fill(Some("zh-CN")).await;

        assert_eq!(
            scene.options().unwrap(),
            b"lang:zh_TW\r\nfov:0.5\r\nonboardAccessibility:false\r\n"
        );
        assert_eq!(notes, [RESPELLED, SKIPPED]);
    }

    /// 开关关掉也要改写法：它管的是引导，这里也不额外声明数据版本。
    #[tokio::test]
    async fn switch_off_still_respells_the_existing_language() {
        let scene = scene(V1_8_9);
        scene.write(OPTIONS_FILE, b"lang:zh_cn\n");

        let report = scene
            .fill_with(Some("zh-CN"), Some("lang-test"), false)
            .await;

        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert_eq!(report.notes, [RESPELLED]);
        assert_eq!(scene.options().unwrap(), b"lang:zh_CN\n");
    }

    /// 写法只看文件和索引，读不出系统语言也一样改。
    #[tokio::test]
    async fn respelling_does_not_need_the_system_language() {
        let scene = scene(V1_8_9);
        scene.write(OPTIONS_FILE, b"lang:zh_cn\nonboardAccessibility:false\n");

        let notes = scene.fill(None).await;

        assert_eq!(
            scene.options().unwrap(),
            b"lang:zh_CN\nonboardAccessibility:false\n"
        );
        assert_eq!(notes, [RESPELLED]);
    }

    /// 写法对的文件一个字节都不写，换着玩的老玩家不会每局都重写一次。
    #[tokio::test]
    async fn language_already_spelled_for_this_version_leaves_the_file_alone() {
        let scene = scene(V1_8_9);
        let original = b"lang:zh_CN\r\nonboardAccessibility:false\r\nfov:0.5";
        scene.write(OPTIONS_FILE, original);

        let notes = scene.fill(Some("zh-CN")).await;

        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(scene.options().unwrap(), original);
    }

    /// 版本没有资源索引时无从判断写法，语言行原样留着，也不算失败。
    #[tokio::test]
    async fn existing_language_stays_without_an_asset_index() {
        let scene = scene(V1_8_9);
        let original = b"lang:zh_cn\nonboardAccessibility:false\n";
        scene.write(OPTIONS_FILE, original);

        let report = scene.fill_with(Some("zh-CN"), None, true).await;

        assert!(report.notes.is_empty(), "{:?}", report.notes);
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert_eq!(scene.options().unwrap(), original);
    }

    /// 索引读不出来只算写法这一项没做成：报给调用方，语言行原样，引导那一行不受牵连。
    #[tokio::test]
    async fn damaged_asset_index_keeps_the_existing_language_and_still_adds_onboarding() {
        let scene = scene(V1_8_9);
        let path = scene.layout.asset_index_json("lang-test").unwrap();
        std::fs::write(path, b"{not json").unwrap();
        scene.write(OPTIONS_FILE, b"lang:zh_cn\n");

        let report = scene
            .fill_with(Some("zh-CN"), Some("lang-test"), true)
            .await;

        assert_eq!(report.failures.len(), 1, "{:?}", report.failures);
        assert_eq!(report.notes, [SKIPPED]);
        assert_eq!(
            scene.options().unwrap(),
            b"lang:zh_cn\nonboardAccessibility:false\n"
        );
    }

    /// 默认语言不用查写法，英文玩家每局启动都走这里，不该为它读索引。
    #[tokio::test]
    async fn default_language_is_left_alone_without_reading_the_index() {
        let scene = scene(V1_21_1);
        let original = b"lang:en_us\nonboardAccessibility:true\nfov:0.5\n";
        scene.write(OPTIONS_FILE, original);

        for skip_onboarding in [true, false] {
            let report = scene
                .fill_with(
                    Some("zh-CN"),
                    Some("index-that-does-not-exist"),
                    skip_onboarding,
                )
                .await;

            assert!(report.notes.is_empty(), "{:?}", report.notes);
            assert!(report.failures.is_empty(), "{:?}", report.failures);
            assert_eq!(scene.options().unwrap(), original);
        }
    }

    #[tokio::test]
    async fn game_default_language_only_gets_the_onboarding_line() {
        let scene = scene(V1_21_1);
        assert_eq!(scene.fill(Some("en-US")).await, [SKIPPED]);
        assert_eq!(scene.options().unwrap(), b"onboardAccessibility:false\n");
    }

    /// 开关关掉、又不用写语言时不创建文件：引导留给游戏，游戏第一次启动照常弹。
    #[tokio::test]
    async fn switch_off_and_game_default_language_write_nothing() {
        let scene = scene(V1_21_1);
        let report = scene
            .fill_with(Some("en-US"), Some("lang-test"), false)
            .await;
        assert!(report.notes.is_empty(), "{:?}", report.notes);
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert_eq!(scene.options(), None);
    }

    /// 开关关掉时语言照写，文件里不出现引导那一行，靠数据版本让游戏别把引导标成走过。
    #[tokio::test]
    async fn switch_off_still_follows_the_system_language() {
        let scene = scene(V1_21_1);
        let report = scene
            .fill_with(Some("zh-CN"), Some("lang-test"), false)
            .await;
        assert_eq!(report.notes, ["游戏语言设为 zh_cn，跟随系统语言 zh-CN"]);
        assert_eq!(scene.options().unwrap(), b"version:3319\nlang:zh_cn\n");
    }

    /// 开关关掉时，空的已有文件与新文件一样带上数据版本：空的 options.txt，和与有内容的那份并存、因而也要
    /// 补的空的模组默认设置。有内容的那份照旧不加，它的迁移由它自己的版本决定。
    #[tokio::test]
    async fn switch_off_declares_the_data_version_in_blank_files_only() {
        let blank_options = scene(V1_21_1);
        blank_options.write(OPTIONS_FILE, b"");
        let report = blank_options
            .fill_with(Some("zh-CN"), Some("lang-test"), false)
            .await;
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert_eq!(
            blank_options.options().unwrap(),
            b"version:3319\nlang:zh_cn\n"
        );

        let mixed = scene(V1_21_1);
        mixed.write("config/yosbr/options.txt", b"");
        mixed.write("config/defaultoptions/options.txt", b"fov:0.6\n");
        let report = mixed
            .fill_with(Some("zh-CN"), Some("lang-test"), false)
            .await;
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert_eq!(
            mixed.read("config/yosbr/options.txt").unwrap(),
            b"version:3319\nlang:zh_cn\n"
        );
        assert_eq!(
            mixed.read("config/defaultoptions/options.txt").unwrap(),
            b"fov:0.6\nlang:zh_cn\n"
        );
        assert_eq!(mixed.options(), None);
    }

    #[tokio::test]
    async fn language_missing_from_the_index_only_says_why() {
        let scene = scene(V1_5_2);
        let notes = scene.fill(Some("zh-CN")).await;
        assert_eq!(scene.options().unwrap(), b"onboardAccessibility:false\n");
        assert_eq!(
            notes,
            [
                "这个版本的资源索引里没有系统语言 zh-CN，游戏语言保持默认",
                SKIPPED
            ]
        );
    }

    #[tokio::test]
    async fn unreadable_system_language_only_says_why() {
        let scene = scene(V1_21_1);
        let notes = scene.fill(None).await;
        assert_eq!(scene.options().unwrap(), b"onboardAccessibility:false\n");
        assert_eq!(notes, ["读不出系统语言，游戏语言保持默认", SKIPPED]);
    }

    #[tokio::test]
    async fn version_without_an_asset_index_only_says_why() {
        let scene = scene(V1_21_1);
        let report = scene.fill_with(Some("zh-CN"), None, true).await;
        assert_eq!(scene.options().unwrap(), b"onboardAccessibility:false\n");
        assert_eq!(
            report.notes,
            ["这个版本没有资源索引，游戏语言保持默认", SKIPPED]
        );
        assert!(report.failures.is_empty(), "{:?}", report.failures);
    }

    /// 套默认设置按目录判断整合包有没有带同名文件，这里按完整路径补 options.txt：两张表说的是同一批模组。
    #[test]
    fn mod_defaults_dirs_are_where_the_mod_default_options_live() {
        assert_eq!(
            MOD_DEFAULT_OPTIONS.map(str::to_owned),
            MOD_DEFAULTS_DIRS.map(|dir| format!("{dir}/{OPTIONS_FILE}"))
        );
    }

    fn pending_note(pending: &str) -> String {
        format!("options.txt 还没有，{pending} 会在第一次启动时铺过去，缺的行补进这份文件")
    }

    const FOLLOWING_OFF: &str = "「游戏语言跟随系统语言」已在设置里关闭，游戏语言保持默认";

    /// 这两个模组只在 options.txt 不存在时铺整合包作者调好的默认设置，抢先写一份就把它们顶掉了。所以缺的行
    /// 补进模组那份文件，options.txt 留给模组去铺。
    #[tokio::test]
    async fn mod_defaults_waiting_to_be_copied_get_the_missing_lines() {
        for pending in MOD_DEFAULT_OPTIONS {
            let scene = scene(V1_21_1);
            scene.write(pending, b"version:3955\r\nfov:0.8\r\n\xff\xfe:1\r\n");

            let notes = scene.fill(Some("zh-CN")).await;

            assert_eq!(scene.options(), None, "{pending}");
            assert_eq!(
                scene.read(pending).unwrap(),
                b"version:3955\r\nfov:0.8\r\n\xff\xfe:1\r\nlang:zh_cn\r\nonboardAccessibility:false\r\n",
                "{pending}"
            );
            assert_eq!(
                notes,
                [
                    pending_note(pending).as_str(),
                    "游戏语言设为 zh_cn，跟随系统语言 zh-CN",
                    SKIPPED
                ]
            );
        }
    }

    /// 模组那份文件里已有的语言与引导设置是作者定的，一个字节都不动；写着游戏默认语言的也一样，启动前不改
    /// 已有的行，那一行只在导入时去掉。
    #[tokio::test]
    async fn mod_defaults_that_already_say_everything_stay_untouched() {
        for original in [
            &b"version:3955\nlang:ja_jp\nonboardAccessibility:true\nfov:0.8\n"[..],
            &b"version:3955\nlang:en_us\nonboardAccessibility:false\n"[..],
        ] {
            let scene = scene(V1_21_1);
            let pending = "config/defaultoptions/options.txt";
            scene.write(pending, original);

            let notes = scene.fill(Some("zh-CN")).await;

            assert_eq!(scene.read(pending).unwrap(), original);
            assert_eq!(scene.options(), None);
            assert_eq!(notes, [pending_note(pending)]);
        }
    }

    /// 模组那份文件里的语言同样跟着这一局的版本改大小写，铺过去之后游戏才认。
    #[tokio::test]
    async fn language_in_mod_defaults_is_respelled_for_the_version_being_launched() {
        let scene = scene(V1_21_1);
        let pending = "config/yosbr/options.txt";
        scene.write(pending, b"lang:zh_CN\nonboardAccessibility:false\n");

        let notes = scene.fill(Some("zh-CN")).await;

        assert_eq!(
            scene.read(pending).unwrap(),
            b"lang:zh_cn\nonboardAccessibility:false\n"
        );
        assert_eq!(scene.options(), None);
        assert_eq!(notes, [pending_note(pending).as_str(), RESPELLED]);
    }

    /// 两个模组的默认设置都在时各补各的。
    #[tokio::test]
    async fn every_pending_mod_defaults_file_is_filled() {
        let scene = scene(V1_21_1);
        scene.write("config/yosbr/options.txt", b"fov:0.8\n");
        scene.write(
            "config/defaultoptions/options.txt",
            b"lang:zh_tw\nfov:0.6\n",
        );

        scene.fill(Some("zh-CN")).await;

        assert_eq!(
            scene.read("config/yosbr/options.txt").unwrap(),
            b"fov:0.8\nlang:zh_cn\nonboardAccessibility:false\n"
        );
        assert_eq!(
            scene.read("config/defaultoptions/options.txt").unwrap(),
            b"lang:zh_tw\nfov:0.6\nonboardAccessibility:false\n"
        );
        assert_eq!(scene.options(), None);
    }

    /// Default Options 的 `extra/` 里只有 options.txt 时也算模组的默认设置：模组会把它整份铺成 options.txt，
    /// 抢先新建一份就把它顶掉了。
    #[tokio::test]
    async fn options_in_the_default_options_extra_folder_are_filled_in_place() {
        let scene = scene(V1_21_1);
        let pending = "config/defaultoptions/extra/options.txt";
        scene.write(
            pending,
            b"version:3955\nfov:0.8\nkey_key.jump:key.keyboard.space\n",
        );

        let notes = scene.fill(Some("zh-CN")).await;

        assert_eq!(scene.options(), None);
        assert_eq!(
            scene.read(pending).unwrap(),
            b"version:3955\nfov:0.8\nkey_key.jump:key.keyboard.space\nlang:zh_cn\nonboardAccessibility:false\n"
        );
        assert_eq!(
            notes,
            [
                pending_note(pending).as_str(),
                "游戏语言设为 zh_cn，跟随系统语言 zh-CN",
                SKIPPED
            ]
        );
    }

    /// Default Options 自己的 options.txt 与 `extra/` 里的那份谁先铺随模组版本而变，两份都补才不会漏。
    #[tokio::test]
    async fn both_default_options_files_are_filled_when_the_pack_ships_both() {
        let scene = scene(V1_21_1);
        scene.write("config/defaultoptions/options.txt", b"fov:0.6\n");
        scene.write("config/defaultoptions/extra/options.txt", b"fov:0.8\n");

        scene.fill(Some("zh-CN")).await;

        assert_eq!(
            scene.read("config/defaultoptions/options.txt").unwrap(),
            b"fov:0.6\nlang:zh_cn\nonboardAccessibility:false\n"
        );
        assert_eq!(
            scene
                .read("config/defaultoptions/extra/options.txt")
                .unwrap(),
            b"fov:0.8\nlang:zh_cn\nonboardAccessibility:false\n"
        );
        assert_eq!(scene.options(), None);
    }

    /// YOSBR 每次启动都会建一份空的默认设置，整合包常把它原样带进来。里面没有作者的设置，不为它让路：照常
    /// 新建 options.txt（模组见它已经在就不铺），那份空文件一个字节都不动。只有空白的同样算空。
    #[tokio::test]
    async fn blank_mod_defaults_are_ignored_and_the_options_file_is_created() {
        let pending = "config/yosbr/options.txt";
        for blank in [&b""[..], &b"\r\n \t\n"[..]] {
            let skipping = scene(V1_21_1);
            skipping.write(pending, blank);
            let notes = skipping.fill(Some("zh-CN")).await;
            assert_eq!(
                skipping.options().unwrap(),
                b"lang:zh_cn\nonboardAccessibility:false\n"
            );
            assert_eq!(skipping.read(pending).unwrap(), blank);
            assert_eq!(notes, ["游戏语言设为 zh_cn，跟随系统语言 zh-CN", SKIPPED]);

            let showing = scene(V1_21_1);
            showing.write(pending, blank);
            let report = showing
                .fill_with(Some("zh-CN"), Some("lang-test"), false)
                .await;
            assert!(report.failures.is_empty(), "{:?}", report.failures);
            assert_eq!(showing.options().unwrap(), b"version:3319\nlang:zh_cn\n");
            assert_eq!(showing.read(pending).unwrap(), blank);
        }
    }

    /// 空的那份与有内容的那份同在时两份都补：先被模组铺成 options.txt 的可能恰是空的那份（YOSBR 早于
    /// Default Options；Default Options 自己的那份与 `extra/` 里的谁先铺随模组版本而变），漏了它语言就
    /// 落空。只有空白的那份原内容照样逐字节保留，换行跟着它走。
    #[tokio::test]
    async fn a_blank_mod_defaults_file_next_to_a_real_one_is_filled_too() {
        const YOSBR: &str = "config/yosbr/options.txt";
        const OWN: &str = "config/defaultoptions/options.txt";
        const EXTRA: &str = "config/defaultoptions/extra/options.txt";
        const FILLED: &[u8] = b"lang:zh_cn\nonboardAccessibility:false\n";
        const FILLED_CRLF: &[u8] = b"\r\n \t\r\nlang:zh_cn\r\nonboardAccessibility:false\r\n";
        let cases: [(&str, &[u8], &[u8], &str); 4] = [
            (YOSBR, b"", FILLED, OWN),
            (OWN, b"", FILLED, EXTRA),
            (EXTRA, b"", FILLED, YOSBR),
            (YOSBR, b"\r\n \t\r\n", FILLED_CRLF, EXTRA),
        ];
        for (blank, blank_bytes, filled, real) in cases {
            let scene = scene(V1_21_1);
            scene.write(blank, blank_bytes);
            scene.write(real, b"fov:0.6\n");

            let notes = scene.fill(Some("zh-CN")).await;

            assert_eq!(scene.read(blank).unwrap(), filled, "{blank}");
            assert_eq!(
                scene.read(real).unwrap(),
                b"fov:0.6\nlang:zh_cn\nonboardAccessibility:false\n",
                "{real}"
            );
            assert_eq!(scene.options(), None, "{blank}");
            let expected: Vec<String> = MOD_DEFAULT_OPTIONS
                .into_iter()
                .filter(|pending| [blank, real].contains(pending))
                .flat_map(|pending| {
                    [
                        pending_note(pending),
                        "游戏语言设为 zh_cn，跟随系统语言 zh-CN".to_owned(),
                        SKIPPED.to_owned(),
                    ]
                })
                .collect();
            assert_eq!(notes, expected, "{blank}");
        }
    }

    /// 在场的几份全是空的才算没有：照常新建 options.txt，几份空文件都不动。
    #[tokio::test]
    async fn several_blank_mod_defaults_files_still_count_as_none() {
        let scene = scene(V1_21_1);
        for pending in MOD_DEFAULT_OPTIONS {
            scene.write(pending, b"\n");
        }

        let notes = scene.fill(Some("zh-CN")).await;

        assert_eq!(
            scene.options().unwrap(),
            b"lang:zh_cn\nonboardAccessibility:false\n"
        );
        for pending in MOD_DEFAULT_OPTIONS {
            assert_eq!(scene.read(pending).unwrap(), b"\n", "{pending}");
        }
        assert_eq!(notes, ["游戏语言设为 zh_cn，跟随系统语言 zh-CN", SKIPPED]);
    }

    /// 整合包的模组默认设置里只有一行默认语言：导入时去掉后成了空文件，启动前就按没有它来，新建 options.txt。
    #[tokio::test]
    async fn mod_defaults_holding_only_the_default_language_end_up_ignored() {
        let scene = scene(V1_21_1);
        let pending = "config/yosbr/options.txt";
        scene.write(pending, b"lang:en_us\n");

        drop_pack_default_language(&scene.working_dir)
            .await
            .unwrap();
        scene.fill(Some("zh-CN")).await;

        assert_eq!(scene.read(pending).unwrap(), b"");
        assert_eq!(
            scene.options().unwrap(),
            b"lang:zh_cn\nonboardAccessibility:false\n"
        );
    }

    #[test]
    fn only_non_whitespace_bytes_count_as_content() {
        let cases: [(&[u8], bool); 7] = [
            (b"", false),
            (b"\n", false),
            (b" \t\r\n\x0c", false),
            (b"fov:0.8", true),
            (b"\n\nlang:zh_cn\n", true),
            // 服务器列表是二进制的，带零字节的最小内容也算有。
            (b"\x0a\x00\x00\x00", true),
            (b"\xff", true),
        ];
        for (bytes, expected) in cases {
            assert_eq!(has_content(bytes), expected, "{bytes:?}");
        }
    }

    /// 关掉「游戏语言跟随系统语言」：不补语言，引导那一行照补，原因记进启动日志。
    #[tokio::test]
    async fn language_switch_off_only_adds_the_onboarding_line() {
        let fresh = scene(V1_21_1);
        let report = fresh.fill_without_following(true).await;
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert_eq!(fresh.options().unwrap(), b"onboardAccessibility:false\n");
        assert_eq!(report.notes, [FOLLOWING_OFF, SKIPPED]);

        let existing = scene(V1_21_1);
        existing.write(OPTIONS_FILE, b"fov:0.5\n");
        existing.fill_without_following(true).await;
        assert_eq!(
            existing.options().unwrap(),
            b"fov:0.5\nonboardAccessibility:false\n"
        );

        let pending = scene(V1_21_1);
        pending.write("config/defaultoptions/options.txt", b"fov:0.8\n");
        pending.fill_without_following(true).await;
        assert_eq!(
            pending.read("config/defaultoptions/options.txt").unwrap(),
            b"fov:0.8\nonboardAccessibility:false\n"
        );
        assert_eq!(pending.options(), None);
    }

    /// 两个开关都关掉时什么都不写，文件也不创建。
    #[tokio::test]
    async fn both_switches_off_write_nothing() {
        let scene = scene(V1_21_1);
        let report = scene.fill_without_following(false).await;
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert_eq!(report.notes, [FOLLOWING_OFF]);
        assert_eq!(scene.options(), None);
    }

    /// 关掉「游戏语言跟随系统语言」也照样改大小写：那是让文件里已有的语言在这个版本继续生效。
    #[tokio::test]
    async fn language_switch_off_still_respells_the_existing_language() {
        let scene = scene(V1_8_9);
        scene.write(OPTIONS_FILE, b"lang:zh_cn\nonboardAccessibility:false\n");

        let report = scene.fill_without_following(true).await;

        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert_eq!(report.notes, [RESPELLED]);
        assert_eq!(
            scene.options().unwrap(),
            b"lang:zh_CN\nonboardAccessibility:false\n"
        );
    }

    #[test]
    fn default_language_lines_are_dropped_and_every_other_byte_stays() {
        let cases: [(&[u8], &[u8]); 6] = [
            (
                b"version:3955\nlang:en_us\nfov:0.5\n",
                b"version:3955\nfov:0.5\n",
            ),
            (
                b"version:3955\r\nlang:en_us\r\n\xff\xfe:1\r\nfov:0.5",
                b"version:3955\r\n\xff\xfe:1\r\nfov:0.5",
            ),
            // 1.11 之前的写法
            (b"lang:en_US\nfov:0.5\n", b"fov:0.5\n"),
            // 末行没有换行
            (b"fov:0.5\nlang:en_us", b"fov:0.5\n"),
            // 只有这一行
            (b"lang:en_us\n", b""),
            // 重复的行一起去掉，单独的 \r 也是行尾
            (b"lang:en_us\rfov:0.5\rlang:EN_US\r", b"fov:0.5\r"),
        ];
        for (options, expected) in cases {
            assert_eq!(
                without_default_language(options).as_deref(),
                Some(expected),
                "{:?}",
                String::from_utf8_lossy(options)
            );
        }
    }

    /// 没有语言行、写着别的语言、默认语言与别的语言并存（游戏取后一行，去掉哪行都可能改变结果）、键名只是
    /// 以 lang 开头，都不动。
    #[test]
    fn files_without_a_plain_default_language_are_left_alone() {
        let cases: [&[u8]; 6] = [
            b"",
            b"version:3955\nfov:0.5\n",
            b"lang:zh_cn\nfov:0.5\n",
            b"lang:ja_jp",
            b"lang:en_us\nlang:zh_cn\n",
            b"language:en_us\n# lang:en_us\n",
        ];
        for options in cases {
            assert_eq!(
                without_default_language(options),
                None,
                "{:?}",
                String::from_utf8_lossy(options)
            );
        }
    }

    /// 导入时四处一起看：写着默认语言的去掉那一行，写着别的语言的、没有的都不动，返回的是改过的文件。
    #[tokio::test]
    async fn pack_default_language_is_dropped_from_every_options_file() {
        let scene = scene(V1_21_1);
        scene.write(OPTIONS_FILE, b"version:3955\nlang:en_us\nfov:0.5\n");
        scene.write("config/yosbr/options.txt", b"lang:ja_jp\nfov:0.8\n");
        scene.write(
            "config/defaultoptions/options.txt",
            b"version:3955\r\nfov:0.6\r\nlang:en_us\r\nonboardAccessibility:false\r\n",
        );
        scene.write(
            "config/defaultoptions/extra/options.txt",
            b"lang:en_us\nfov:0.7\n",
        );
        scene.write("config/fancymenu/options.txt", b"lang:en_us\n");

        let changed = drop_pack_default_language(&scene.working_dir)
            .await
            .unwrap();

        assert_eq!(
            changed,
            [
                OPTIONS_FILE,
                "config/defaultoptions/options.txt",
                "config/defaultoptions/extra/options.txt",
            ]
        );
        assert_eq!(scene.options().unwrap(), b"version:3955\nfov:0.5\n");
        assert_eq!(
            scene
                .read("config/defaultoptions/extra/options.txt")
                .unwrap(),
            b"fov:0.7\n"
        );
        assert_eq!(
            scene.read("config/yosbr/options.txt").unwrap(),
            b"lang:ja_jp\nfov:0.8\n"
        );
        assert_eq!(
            scene.read("config/defaultoptions/options.txt").unwrap(),
            b"version:3955\r\nfov:0.6\r\nonboardAccessibility:false\r\n"
        );
        assert_eq!(
            scene.read("config/fancymenu/options.txt").unwrap(),
            b"lang:en_us\n",
            "别的模组自己的 options.txt 不是游戏设置"
        );

        // 再来一次没有可去掉的
        assert!(
            drop_pack_default_language(&scene.working_dir)
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// options.txt 里只有默认语言那一行时，去掉后成了空文件，接着整份删掉：空文件会让游戏把引导标成走过，
    /// 还挡住模组的默认设置与套默认设置。模组那几份照旧留成空文件。
    #[tokio::test]
    async fn an_options_file_emptied_of_the_default_language_is_removed() {
        for (original, emptied) in [
            (&b"lang:en_us\n"[..], &b""[..]),
            (&b"\r\nlang:en_US\r\n \r\n"[..], &b"\r\n \r\n"[..]),
        ] {
            let scene = scene(V1_21_1);
            scene.write(OPTIONS_FILE, original);
            scene.write("config/yosbr/options.txt", b"lang:en_us\n");

            let changed = drop_pack_default_language(&scene.working_dir)
                .await
                .unwrap();
            assert_eq!(changed, [OPTIONS_FILE, "config/yosbr/options.txt"]);
            assert_eq!(scene.options().unwrap(), emptied);

            assert!(remove_blank_options_file(&scene.working_dir).await.unwrap());
            assert_eq!(scene.options(), None);
            assert_eq!(scene.read("config/yosbr/options.txt").unwrap(), b"");
        }
    }

    /// 只删空的 options.txt：有内容的、不存在的、位置上是目录的都不动，模组那几份空的默认设置也不删。
    #[tokio::test]
    async fn only_a_blank_options_file_is_removed() {
        let blank = scene(V1_21_1);
        blank.write(OPTIONS_FILE, b" \r\n\t\n");
        for pending in MOD_DEFAULT_OPTIONS {
            blank.write(pending, b"");
        }
        assert!(remove_blank_options_file(&blank.working_dir).await.unwrap());
        assert_eq!(blank.options(), None);
        for pending in MOD_DEFAULT_OPTIONS {
            assert_eq!(blank.read(pending).unwrap(), b"", "{pending}");
        }

        let kept = scene(V1_21_1);
        kept.write(OPTIONS_FILE, b"lang:en_us\n");
        assert!(!remove_blank_options_file(&kept.working_dir).await.unwrap());
        assert_eq!(kept.options().unwrap(), b"lang:en_us\n");

        let absent = scene(V1_21_1);
        assert!(
            !remove_blank_options_file(&absent.working_dir)
                .await
                .unwrap()
        );

        let directory = scene(V1_21_1);
        std::fs::create_dir_all(directory.working_dir.join(OPTIONS_FILE)).unwrap();
        assert!(
            !remove_blank_options_file(&directory.working_dir)
                .await
                .unwrap()
        );
        assert!(directory.working_dir.join(OPTIONS_FILE).is_dir());
    }

    /// 关着「跳过引导」又没有语言可写时，凡是会让游戏读到一份空设置文件的情形都写上数据版本：空的
    /// options.txt；在场的模组默认设置全是空的（不建 options.txt 的话模组会把空文件铺过去）；与有内容的那份
    /// 并存的空的模组默认设置。没有设置文件、也没有东西会被铺过来时不建文件。
    #[tokio::test]
    async fn switch_off_declares_the_data_version_wherever_a_blank_file_would_be_read() {
        const VERSION_ONLY: &str =
            "设置文件是空的，已写上数据版本，免得游戏把首次无障碍引导标成走过";

        let blank_options = scene(V1_21_1);
        blank_options.write(OPTIONS_FILE, b"");
        let report = blank_options
            .fill_with(Some("en-US"), Some("lang-test"), false)
            .await;
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert_eq!(blank_options.options().unwrap(), b"version:3319\n");
        assert_eq!(report.notes, [VERSION_ONLY]);

        let blank_defaults = scene(V1_21_1);
        blank_defaults.write("config/yosbr/options.txt", b"");
        let report = blank_defaults
            .fill_with(Some("en-US"), Some("lang-test"), false)
            .await;
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert_eq!(blank_defaults.options().unwrap(), b"version:3319\n");
        assert_eq!(
            blank_defaults.read("config/yosbr/options.txt").unwrap(),
            b""
        );
        assert_eq!(report.notes, [VERSION_ONLY]);

        let mixed = scene(V1_21_1);
        mixed.write("config/yosbr/options.txt", b"");
        mixed.write("config/defaultoptions/options.txt", b"fov:0.6\n");
        let report = mixed
            .fill_with(Some("en-US"), Some("lang-test"), false)
            .await;
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert_eq!(
            mixed.read("config/yosbr/options.txt").unwrap(),
            b"version:3319\n"
        );
        assert_eq!(
            mixed.read("config/defaultoptions/options.txt").unwrap(),
            b"fov:0.6\n"
        );
        assert_eq!(mixed.options(), None);

        // 开关开着时照旧写引导那一行，不声明版本。
        let skipping = scene(V1_21_1);
        skipping.write("config/yosbr/options.txt", b"");
        skipping.fill(Some("en-US")).await;
        assert_eq!(skipping.options().unwrap(), b"onboardAccessibility:false\n");
    }

    #[test]
    fn data_version_is_declared_only_when_the_game_would_read_a_blank_file() {
        // (现有内容, 要写语言, 开关, 空的模组默认设置在等, 期望)
        type Case<'a> = (Option<&'a [u8]>, bool, bool, bool, bool);
        let cases: [Case; 10] = [
            (None, true, false, false, true),
            (None, false, false, false, false),
            (None, false, false, true, true),
            (None, true, true, true, false),
            (Some(b""), false, false, false, true),
            (Some(b" \r\n"), true, false, false, true),
            (Some(b""), false, true, false, false),
            (Some(b"fov:0.5\n"), true, false, false, false),
            (Some(b"fov:0.5\n"), false, false, true, false),
            (Some(b"\xef\xbb\xbf"), true, false, false, false),
        ];
        for (existing, writes_language, skip_onboarding, pending, expected) in cases {
            assert_eq!(
                declares_data_version(existing, writes_language, skip_onboarding, pending),
                expected,
                "{existing:?} {writes_language} {skip_onboarding} {pending}"
            );
        }
    }

    /// 整合包在模组默认设置的位置上放了个目录时没有行可去：其余几份照常处理，不为它让导入失败。
    #[tokio::test]
    async fn a_directory_where_an_options_file_belongs_is_skipped_when_dropping() {
        let scene = scene(V1_21_1);
        let directory = scene.working_dir.join("config/yosbr/options.txt");
        std::fs::create_dir_all(&directory).unwrap();
        scene.write(
            "config/defaultoptions/options.txt",
            b"lang:en_us\nfov:0.6\n",
        );

        let changed = drop_pack_default_language(&scene.working_dir)
            .await
            .unwrap();

        assert_eq!(changed, ["config/defaultoptions/options.txt"]);
        assert_eq!(
            scene.read("config/defaultoptions/options.txt").unwrap(),
            b"fov:0.6\n"
        );
        assert!(directory.is_dir());
    }

    /// 一份模组默认设置写不进去（被别的进程占着）时只记这一份的失败，别的照补；写不进去的那份不记说明。
    #[cfg(windows)]
    #[tokio::test]
    async fn a_mod_defaults_file_that_cannot_be_written_does_not_hold_the_others_back() {
        use std::os::windows::fs::OpenOptionsExt;
        let scene = scene(V1_21_1);
        let locked = "config/yosbr/options.txt";
        let free = "config/defaultoptions/options.txt";
        scene.write(locked, b"fov:0.8\n");
        scene.write(free, b"fov:0.6\n");
        // 只放行读（FILE_SHARE_READ）：读得到，换不掉。
        let held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(scene.working_dir.join(locked))
            .unwrap();

        let report = scene
            .fill_with(Some("zh-CN"), Some("lang-test"), true)
            .await;
        drop(held);

        assert_eq!(report.failures.len(), 1, "{:?}", report.failures);
        assert_eq!(scene.read(locked).unwrap(), b"fov:0.8\n");
        assert_eq!(
            scene.read(free).unwrap(),
            b"fov:0.6\nlang:zh_cn\nonboardAccessibility:false\n"
        );
        assert_eq!(scene.options(), None);
        assert_eq!(
            report.notes,
            [
                pending_note(free).as_str(),
                "游戏语言设为 zh_cn，跟随系统语言 zh-CN",
                SKIPPED
            ]
        );
    }

    /// 导入时去掉默认语言行之后，启动前的补全把语言补进同一份文件：两步合起来就是整合包的英文换成系统语言，
    /// 作者的其他设置原样铺过去。
    #[tokio::test]
    async fn dropped_pack_language_is_filled_from_the_system_at_launch() {
        let scene = scene(V1_21_1);
        let pending = "config/defaultoptions/options.txt";
        scene.write(
            pending,
            b"version:3955\nfov:0.8\nlang:en_us\nonboardAccessibility:false\n",
        );

        drop_pack_default_language(&scene.working_dir)
            .await
            .unwrap();
        scene.fill(Some("zh-CN")).await;

        assert_eq!(
            scene.read(pending).unwrap(),
            b"version:3955\nfov:0.8\nonboardAccessibility:false\nlang:zh_cn\n"
        );
        assert_eq!(scene.options(), None);
    }

    /// 模组的默认设置读不了（这里是个目录）时照实报错，不当成没有而抢先建 options.txt。
    #[tokio::test]
    async fn unreadable_mod_defaults_are_reported_and_no_options_file_is_created() {
        let scene = scene(V1_21_1);
        std::fs::create_dir_all(scene.working_dir.join("config/defaultoptions/options.txt"))
            .unwrap();

        let report = scene
            .fill_with(Some("zh-CN"), Some("lang-test"), true)
            .await;

        assert_eq!(report.failures.len(), 1, "{:?}", report.failures);
        assert!(
            matches!(&report.failures[0], CoreError::Base(petalfall_base::Error::Io { path, .. }) if path.ends_with("config/defaultoptions/options.txt")),
            "{:?}",
            report.failures
        );
        assert_eq!(scene.options(), None);
    }

    /// 一份读不了不耽误读得了的：说不清它有没有内容，按有算，不新建 options.txt，读得了的每一份（空的也
    /// 在内，排在它前面后面都一样）照补。
    #[tokio::test]
    async fn readable_mod_defaults_are_still_filled_when_another_one_is_unreadable() {
        const UNREADABLE: &str = "config/defaultoptions/options.txt";
        let cases: [(&str, &[u8], &[u8]); 3] = [
            (
                "config/yosbr/options.txt",
                b"fov:0.8\n",
                b"fov:0.8\nlang:zh_cn\nonboardAccessibility:false\n",
            ),
            (
                "config/yosbr/options.txt",
                b"",
                b"lang:zh_cn\nonboardAccessibility:false\n",
            ),
            (
                "config/defaultoptions/extra/options.txt",
                b"fov:0.8\n",
                b"fov:0.8\nlang:zh_cn\nonboardAccessibility:false\n",
            ),
        ];
        for (readable, original, filled) in cases {
            let scene = scene(V1_21_1);
            scene.write(readable, original);
            std::fs::create_dir_all(scene.working_dir.join(UNREADABLE)).unwrap();

            let report = scene
                .fill_with(Some("zh-CN"), Some("lang-test"), true)
                .await;

            assert_eq!(report.failures.len(), 1, "{:?}", report.failures);
            assert!(
                matches!(&report.failures[0], CoreError::Base(petalfall_base::Error::Io { path, .. }) if path.ends_with(UNREADABLE)),
                "{:?}",
                report.failures
            );
            assert_eq!(scene.read(readable).unwrap(), filled, "{readable}");
            assert_eq!(scene.options(), None, "{readable}");
            assert_eq!(
                report.notes,
                [
                    pending_note(readable).as_str(),
                    "游戏语言设为 zh_cn，跟随系统语言 zh-CN",
                    SKIPPED
                ],
                "{readable}"
            );
        }
    }

    /// options.txt 已经在了，模组不会再铺默认设置，缺的行照常补进 options.txt，模组那份一个字节都不动。
    #[tokio::test]
    async fn mod_defaults_do_not_matter_once_the_options_file_exists() {
        let scene = scene(V1_21_1);
        for pending in MOD_DEFAULT_OPTIONS {
            scene.write(pending, b"fov:0.8\n");
        }
        scene.write(OPTIONS_FILE, b"fov:0.5\n");

        let notes = scene.fill(Some("zh-CN")).await;

        assert_eq!(
            scene.options().unwrap(),
            b"fov:0.5\nlang:zh_cn\nonboardAccessibility:false\n"
        );
        for pending in MOD_DEFAULT_OPTIONS {
            assert_eq!(scene.read(pending).unwrap(), b"fov:0.8\n", "{pending}");
        }
        assert_eq!(notes, ["游戏语言设为 zh_cn，跟随系统语言 zh-CN", SKIPPED]);
    }

    /// 索引读不出来只算语言这一项没做成：报给调用方，引导那一行不受牵连。
    #[tokio::test]
    async fn damaged_asset_index_is_reported_and_onboarding_is_still_written() {
        let scene = scene(V1_21_1);
        let path = scene.layout.asset_index_json("lang-test").unwrap();
        std::fs::write(path, b"{not json").unwrap();

        let report = scene
            .fill_with(Some("zh-CN"), Some("lang-test"), true)
            .await;

        assert_eq!(report.failures.len(), 1, "{:?}", report.failures);
        assert_eq!(report.notes, [SKIPPED]);
        assert_eq!(scene.options().unwrap(), b"onboardAccessibility:false\n");
    }

    #[tokio::test]
    async fn damaged_asset_index_with_the_switch_off_writes_nothing() {
        let scene = scene(V1_21_1);
        let path = scene.layout.asset_index_json("lang-test").unwrap();
        std::fs::write(path, b"{not json").unwrap();

        let report = scene
            .fill_with(Some("zh-CN"), Some("lang-test"), false)
            .await;

        assert_eq!(report.failures.len(), 1, "{:?}", report.failures);
        assert!(report.notes.is_empty(), "{:?}", report.notes);
        assert_eq!(scene.options(), None);
    }

    // ---- 记账 ----

    fn record_path() -> String {
        format!("{PETALFALL_META_DIR}/{FILLED_LINES_FILE}")
    }

    impl Scene {
        /// 记账文件的原始字节，没有时为 `None`。
        fn record_bytes(&self) -> Option<Vec<u8>> {
            self.read(&record_path())
        }

        /// 记账文件按 JSON 读出的内容。
        fn record(&self) -> serde_json::Value {
            serde_json::from_slice(&self.record_bytes().expect("应有记账文件")).unwrap()
        }
    }

    /// 两份模组默认设置各补各的，各记各的：记的是逐字的整行，不带行尾，CRLF 文件也一样；读回来与写的一致。
    #[tokio::test]
    async fn lines_filled_into_mod_defaults_are_recorded_per_file() {
        let scene = scene(V1_21_1);
        scene.write("config/yosbr/options.txt", b"fov:0.8\r\n\xff\xfe:1\r\n");
        scene.write(
            "config/defaultoptions/options.txt",
            b"lang:ja_jp\nfov:0.6\n",
        );

        scene.fill(Some("zh-CN")).await;

        assert_eq!(
            scene.read("config/yosbr/options.txt").unwrap(),
            b"fov:0.8\r\n\xff\xfe:1\r\nlang:zh_cn\r\nonboardAccessibility:false\r\n"
        );
        assert_eq!(
            scene.record(),
            serde_json::json!({"files": {
                "config/defaultoptions/options.txt": ["onboardAccessibility:false"],
                "config/yosbr/options.txt": ["lang:zh_cn", "onboardAccessibility:false"],
            }})
        );
        let read = read_filled_lines(&scene.working_dir).await.unwrap();
        assert_eq!(
            read.lines_for("config/yosbr/options.txt").unwrap(),
            ["lang:zh_cn", "onboardAccessibility:false"]
        );
        assert_eq!(
            read.lines_for("config/defaultoptions/options.txt").unwrap(),
            ["onboardAccessibility:false"]
        );
        assert_eq!(
            read.lines_for("config/defaultoptions/extra/options.txt"),
            None
        );
    }

    /// 关掉跳过引导时空文件里补的 `version:3319` 也是启动器补的，一起记；有内容的那份没补它，也就不记。
    #[tokio::test]
    async fn the_data_version_line_is_recorded_with_the_language() {
        let scene = scene(V1_21_1);
        scene.write("config/yosbr/options.txt", b"\r\n");
        scene.write("config/defaultoptions/extra/options.txt", b"fov:0.6\n");

        let report = scene
            .fill_with(Some("zh-CN"), Some("lang-test"), false)
            .await;

        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert_eq!(
            scene.read("config/yosbr/options.txt").unwrap(),
            b"\r\nversion:3319\r\nlang:zh_cn\r\n"
        );
        assert_eq!(
            scene.record(),
            serde_json::json!({"files": {
                "config/defaultoptions/extra/options.txt": ["lang:zh_cn"],
                "config/yosbr/options.txt": ["version:3319", "lang:zh_cn"],
            }})
        );
    }

    /// 同一份文件以后又补了别的行就并进去，按补上的先后排；已记过的行再补一次（有人把它删了）不重复记。
    #[tokio::test]
    async fn a_later_fill_of_the_same_file_merges_into_the_record() {
        let scene = scene(V1_21_1);
        let pending = "config/defaultoptions/options.txt";
        scene.write(pending, b"fov:0.6\n");
        scene.write("config/yosbr/options.txt", b"fov:0.8\n");

        let report = scene.fill_without_following(true).await;
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        let onboarding_only = serde_json::json!({"files": {
            "config/defaultoptions/options.txt": ["onboardAccessibility:false"],
            "config/yosbr/options.txt": ["onboardAccessibility:false"],
        }});
        assert_eq!(scene.record(), onboarding_only);

        // 只有一份又补了行：另一份的记录原样。
        scene.write(
            "config/yosbr/options.txt",
            b"lang:de_de\nfov:0.8\nonboardAccessibility:false\n",
        );
        scene.fill(Some("zh-CN")).await;
        assert_eq!(
            scene.read(pending).unwrap(),
            b"fov:0.6\nonboardAccessibility:false\nlang:zh_cn\n"
        );
        let merged = serde_json::json!({"files": {
            "config/defaultoptions/options.txt": ["onboardAccessibility:false", "lang:zh_cn"],
            "config/yosbr/options.txt": ["onboardAccessibility:false"],
        }});
        assert_eq!(scene.record(), merged);

        scene.write(pending, b"fov:0.6\nonboardAccessibility:false\n");
        scene.fill(Some("zh-CN")).await;
        assert_eq!(
            scene.read(pending).unwrap(),
            b"fov:0.6\nonboardAccessibility:false\nlang:zh_cn\n"
        );
        assert_eq!(scene.record(), merged);
    }

    /// 什么都没补的一局不产生记账文件，已有的也一个字节都不改写：补的是 options.txt、模组那份已经什么都有、
    /// 只改了语言的写法、两个开关都关掉。
    #[tokio::test]
    async fn a_fill_that_adds_nothing_to_mod_defaults_leaves_the_record_alone() {
        let into_options = scene(V1_21_1);
        into_options.write(OPTIONS_FILE, b"fov:0.5\n");
        into_options.write("config/yosbr/options.txt", b"fov:0.8\n");
        into_options.fill(Some("zh-CN")).await;
        assert_eq!(
            into_options.options().unwrap(),
            b"fov:0.5\nlang:zh_cn\nonboardAccessibility:false\n"
        );
        assert_eq!(into_options.record_bytes(), None);

        let fresh = scene(V1_21_1);
        fresh.fill(Some("zh-CN")).await;
        assert!(fresh.options().is_some());
        assert_eq!(fresh.record_bytes(), None);

        let earlier = br#"{"files":{"config/yosbr/options.txt":["lang:zh_cn"]}}"#;
        let cases: [(&[u8], bool, bool); 3] = [
            (b"lang:ja_jp\nonboardAccessibility:true\n", true, true),
            (b"lang:zh_CN\nonboardAccessibility:false\n", true, true),
            (b"fov:0.8\n", false, false),
        ];
        for (original, follow, skip) in cases {
            for existing_record in [None, Some(&earlier[..])] {
                let scene = scene(V1_21_1);
                scene.write("config/defaultoptions/options.txt", original);
                if let Some(bytes) = existing_record {
                    scene.write(&record_path(), bytes);
                }
                let report = if follow {
                    scene
                        .fill_with(Some("zh-CN"), Some("lang-test"), skip)
                        .await
                } else {
                    scene.fill_without_following(skip).await
                };
                assert!(report.failures.is_empty(), "{:?}", report.failures);
                assert_eq!(
                    scene.record_bytes().as_deref(),
                    existing_record,
                    "{:?}",
                    String::from_utf8_lossy(original)
                );
            }
        }
    }

    /// 记账读不懂时照实报错，不覆盖它（里面记着的行导出时还要靠它去掉）；补行本身照做，不撤回。
    #[tokio::test]
    async fn an_unreadable_record_is_reported_and_kept_while_the_lines_stay_filled() {
        let scene = scene(V1_21_1);
        let pending = "config/yosbr/options.txt";
        scene.write(pending, b"fov:0.8\n");
        scene.write(&record_path(), b"{not json");

        let report = scene
            .fill_with(Some("zh-CN"), Some("lang-test"), true)
            .await;

        assert_eq!(report.failures.len(), 1, "{:?}", report.failures);
        assert!(
            matches!(&report.failures[0], CoreError::JsonFileInvalid { path, .. } if path.ends_with(FILLED_LINES_FILE)),
            "{:?}",
            report.failures
        );
        assert_eq!(
            scene.read(pending).unwrap(),
            b"fov:0.8\nlang:zh_cn\nonboardAccessibility:false\n"
        );
        assert_eq!(scene.record_bytes().unwrap(), b"{not json");
        assert_eq!(
            report.notes,
            [
                pending_note(pending).as_str(),
                "游戏语言设为 zh_cn，跟随系统语言 zh-CN",
                SKIPPED
            ]
        );
    }

    fn lines(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|line| (*line).to_owned()).collect()
    }

    #[test]
    fn recorded_lines_are_dropped_with_their_line_ends_and_every_other_byte_stays() {
        let ours = lines(&["lang:zh_cn", "onboardAccessibility:false"]);
        let cases: [(&[u8], &[u8]); 7] = [
            (
                b"version:3955\r\nfov:0.8\r\n\xff\xfe:1\r\nlang:zh_cn\r\nonboardAccessibility:false\r\n",
                b"version:3955\r\nfov:0.8\r\n\xff\xfe:1\r\n",
            ),
            (
                b"fov:0.8\nlang:zh_cn\nonboardAccessibility:false\n",
                b"fov:0.8\n",
            ),
            // 末行没有行尾，单独的 \r 也是行尾
            (b"fov:0.8\nonboardAccessibility:false\nlang:zh_cn", b"fov:0.8\n"),
            (b"fov:0.8\rlang:zh_cn\ronboardAccessibility:false\r", b"fov:0.8\r"),
            // 只剩一条还在的，就只去掉这一条
            (b"fov:0.8\nonboardAccessibility:false\n", b"fov:0.8\n"),
            // 同样的行有两处，只去掉最后一处
            (
                b"lang:zh_cn\nfov:0.8\nlang:zh_cn\nonboardAccessibility:false\n",
                b"lang:zh_cn\nfov:0.8\n",
            ),
            // 原本是空文件，补的行去掉后又是空的
            (b"lang:zh_cn\nonboardAccessibility:false\n", b""),
        ];
        for (content, expected) in cases {
            assert_eq!(
                without_lines(content, &ours).as_deref(),
                Some(expected),
                "{:?}",
                String::from_utf8_lossy(content)
            );
        }
        assert_eq!(
            without_lines(
                b"\nversion:3319\nlang:zh_cn\n",
                &lines(&["version:3319", "lang:zh_cn"])
            )
            .as_deref(),
            Some(&b"\n"[..])
        );
    }

    /// 记着的行被人改掉或删掉了：不去掉任何别的行，只差大小写、多了前后缀的不算同一行。
    #[test]
    fn recorded_lines_that_are_gone_take_nothing_else_with_them() {
        let ours = lines(&["lang:zh_cn", "onboardAccessibility:false"]);
        let cases: [&[u8]; 8] = [
            b"",
            b"fov:0.8\n",
            b"fov:0.8\nlang:ja_jp\nonboardAccessibility:true\n",
            b"lang:zh_CN\n",
            b"lang:zh_cn_x\n",
            b" lang:zh_cn\n",
            b"xlang:zh_cn\r\n",
            b"lang:zh_cnonboardAccessibility:false\n",
        ];
        for content in cases {
            assert_eq!(
                without_lines(content, &ours),
                None,
                "{:?}",
                String::from_utf8_lossy(content)
            );
        }
        // 一条已经不在了：只去掉还在的那条，别的照留。
        assert_eq!(
            without_lines(b"lang:ja_jp\nfov:0.8\nonboardAccessibility:false\n", &ours).as_deref(),
            Some(&b"lang:ja_jp\nfov:0.8\n"[..])
        );
    }

    /// 随机内容：补行之后再按同样的行去掉，剩下的就是原内容（加上补行时为隔开而补的那个换行），一个字节不差。
    #[test]
    fn dropping_the_filled_lines_restores_the_original_bytes() {
        let added = lines(&["version:3319", "lang:zh_cn", SKIP_ONBOARDING_LINE]);
        for _ in 0..200 {
            let original: Vec<u8> = std::iter::repeat_with(|| fastrand::u8(..))
                .take(fastrand::usize(0..2048))
                .collect();
            let count = fastrand::usize(1..=added.len());
            let filled = with_lines(&original, &added[..count]);

            let kept = without_lines(&filled, &added[..count]).unwrap();

            assert_eq!(kept, with_lines(&original, &[]));
            assert!(kept.starts_with(&original));
        }
    }

    /// 只认那三份模组默认设置，不分大小写；记账里混进别的路径（被人改过）也不认。
    #[test]
    fn only_the_mod_default_options_are_looked_up_in_the_record() {
        let record = FilledLines {
            files: BTreeMap::from([
                (
                    "config/yosbr/options.txt".to_owned(),
                    lines(&["lang:zh_cn"]),
                ),
                (
                    "config/fancymenu/options.txt".to_owned(),
                    lines(&["lang:zh_cn"]),
                ),
            ]),
        };
        for relative in ["config/yosbr/options.txt", "Config/YOSBR/Options.txt"] {
            assert_eq!(
                record.lines_for(relative).unwrap(),
                ["lang:zh_cn"],
                "{relative}"
            );
        }
        for relative in [
            "config/fancymenu/options.txt",
            "config/defaultoptions/options.txt",
            "options.txt",
            "config/yosbr/options.txt.old",
        ] {
            assert_eq!(record.lines_for(relative), None, "{relative}");
        }
    }
}
