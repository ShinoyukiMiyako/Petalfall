//! 没有自定义皮肤的玩家在原版客户端里显示的默认皮肤：由 UUID 的哈希选出。1.19.3 起在 9 套默认皮肤
//! × 细臂、宽臂两种体型里选一套；更早的客户端只有宽臂的 steve 与细臂的 alex 两套，按哈希的奇偶选。
//! 离线账户永远没有自定义皮肤，正版与外置账户拿不到远程皮肤时（档案里没有、档案没取到、下载失败）游戏里显示的
//! 也是这一套，所以界面按它画；皮肤图从材质源读，选取规则也跟着材质源的版本走，否则旧版材质源里读不到 18 款的图，
//! 只能画人偶。
//!
//! 前端添加离线账户的弹窗按同一规则预览（`flows/login/default-skin.ts`），两边用同一组测试向量钉住。

use std::collections::BTreeSet;

use petalfall_auth::SkinModel;

/// 一套默认皮肤：材质名单里的路径与体型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DefaultSkin {
    pub path: &'static str,
    pub model: SkinModel,
}

/// 原版客户端默认皮肤数组的顺序：先 9 套细臂，后 9 套宽臂，各自按名字字母序。选取结果依赖这个顺序，不能改。
const DEFAULT_SKINS: [DefaultSkin; 18] = [
    slim("entity/player/slim/alex"),
    slim("entity/player/slim/ari"),
    slim("entity/player/slim/efe"),
    slim("entity/player/slim/kai"),
    slim("entity/player/slim/makena"),
    slim("entity/player/slim/noor"),
    slim("entity/player/slim/steve"),
    slim("entity/player/slim/sunny"),
    slim("entity/player/slim/zuri"),
    wide("entity/player/wide/alex"),
    wide("entity/player/wide/ari"),
    wide("entity/player/wide/efe"),
    wide("entity/player/wide/kai"),
    wide("entity/player/wide/makena"),
    wide("entity/player/wide/noor"),
    wide("entity/player/wide/steve"),
    wide("entity/player/wide/sunny"),
    wide("entity/player/wide/zuri"),
];

/// 1.19.3 以前原版客户端的两套默认皮肤，下标是哈希的最低位：客户端在 `(uuid.hashCode() & 1) == 1` 时给细臂
/// alex，否则给宽臂 steve，体型与皮肤由同一个判断给出。
const LEGACY_SKINS: [DefaultSkin; 2] = [wide("entity/steve"), slim("entity/alex")];

/// 材质源里是哪一套默认皮肤。原版在 1.19.3 把两套换成 18 款，选取规则也一起换了。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DefaultSkinSet {
    /// 1.19.3 起的 18 款。没有材质源时也按它：界面不取图，下载来的最新正式版也是这一套。
    #[default]
    Modern,
    /// 1.13 到 1.19.2：只有 `entity/steve` 与 `entity/alex` 两张。
    Legacy,
}

impl DefaultSkinSet {
    /// 按材质源抽进缓存的文件认：有旧版两张中的任一张就是 1.19.3 以前的源。1.19.3 起的客户端 jar 里没有这两张，
    /// 更早的只有这两张；不看版本号，因为本机的版本目录可以随意命名（整合包常用中文名）。
    pub fn of_source(files: &BTreeSet<String>) -> Self {
        if LEGACY_SKINS.iter().any(|skin| files.contains(skin.path)) {
            DefaultSkinSet::Legacy
        } else {
            DefaultSkinSet::Modern
        }
    }
}

const fn slim(path: &'static str) -> DefaultSkin {
    DefaultSkin {
        path,
        model: SkinModel::Slim,
    }
}

const fn wide(path: &'static str) -> DefaultSkin {
    DefaultSkin {
        path,
        model: SkinModel::Wide,
    }
}

/// Java 的 `UUID.hashCode`：高 64 位与低 64 位异或，再把结果的高、低 32 位异或，按有符号 32 位整数解释。
/// 带不带连字符、大小写都认；不是 32 位十六进制时为 None。
pub fn java_uuid_hash(uuid: &str) -> Option<i32> {
    let hex: String = uuid.chars().filter(|c| *c != '-').collect();
    // from_str_radix 认前导 '+'，先逐字确认全是十六进制数字
    if hex.len() != 32 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let value = u128::from_str_radix(&hex, 16).ok()?;
    let folded = ((value >> 64) as u64) ^ (value as u64);
    Some((((folded >> 32) as u32) ^ (folded as u32)) as i32)
}

/// 这个 UUID 在原版客户端里的默认皮肤。1.19.3 起下标为哈希对 18 取非负余数（Java 的 `Math.floorMod`）；
/// 更早看哈希的最低位，负哈希按补码取位，与 Java 的 `&` 一致。
pub fn default_skin(uuid: &str, set: DefaultSkinSet) -> Option<DefaultSkin> {
    let hash = java_uuid_hash(uuid)?;
    Some(match set {
        DefaultSkinSet::Modern => {
            DEFAULT_SKINS[hash.rem_euclid(DEFAULT_SKINS.len() as i32) as usize]
        }
        DefaultSkinSet::Legacy => LEGACY_SKINS[(hash & 1) as usize],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::art::whitelist::SKIN_TEXTURES;

    // 期望值由 JDK 的 java.util.UUID.hashCode 与 Math.floorMod 独立算出，与前端测试同一组。
    #[test]
    fn hash_matches_java_uuid_hash_code() {
        for (uuid, hash) in [
            ("00000000-0000-0000-0000-000000000000", 0),
            ("00000000-0000-0000-0000-000000000011", 17),
            ("00000000-0000-0000-0000-00000000ffff", 65535),
            ("00000000-0000-0000-0000-0000ffffffff", -1),
            ("12345678-0000-0000-0000-000012345678", 0),
            ("ffffffff-ffff-ffff-ffff-ffffffffffff", 0),
            ("80000000-0000-0000-0000-000000000000", i32::MIN),
            ("069a79f4-44e9-4726-a5be-fca90e38aaf5", -369792882),
            ("853c80ef-3c37-49fd-aa49-938b674adae6", 1946714239),
            ("5627dd98-e6be-3c21-b8a8-e92344183641", 1277771483),
        ] {
            assert_eq!(java_uuid_hash(uuid), Some(hash), "{uuid}");
        }
        assert_eq!(
            java_uuid_hash("5627DD98E6BE3C21B8A8E92344183641"),
            java_uuid_hash("5627dd98-e6be-3c21-b8a8-e92344183641")
        );
    }

    #[test]
    fn non_uuid_has_no_hash() {
        for bad in [
            "",
            "5627dd98-e6be",
            "zz27dd98e6be3c21b8a8e92344183641",
            "+627dd98e6be3c21b8a8e92344183641",
            "5627dd98e6be3c21b8a8e923441836410",
        ] {
            assert_eq!(java_uuid_hash(bad), None, "{bad}");
            assert_eq!(default_skin(bad, DefaultSkinSet::Modern), None, "{bad}");
            assert_eq!(default_skin(bad, DefaultSkinSet::Legacy), None, "{bad}");
        }
    }

    #[test]
    fn hashes_zero_to_seventeen_walk_slim_then_wide() {
        let picks: Vec<&str> = (0..18)
            .map(|k| {
                default_skin(&format!("{k:032x}"), DefaultSkinSet::Modern)
                    .unwrap()
                    .path
            })
            .collect();
        assert_eq!(
            picks,
            [
                "entity/player/slim/alex",
                "entity/player/slim/ari",
                "entity/player/slim/efe",
                "entity/player/slim/kai",
                "entity/player/slim/makena",
                "entity/player/slim/noor",
                "entity/player/slim/steve",
                "entity/player/slim/sunny",
                "entity/player/slim/zuri",
                "entity/player/wide/alex",
                "entity/player/wide/ari",
                "entity/player/wide/efe",
                "entity/player/wide/kai",
                "entity/player/wide/makena",
                "entity/player/wide/noor",
                "entity/player/wide/steve",
                "entity/player/wide/sunny",
                "entity/player/wide/zuri",
            ]
        );
    }

    #[test]
    fn negative_hash_uses_floor_mod() {
        for (uuid, path, model) in [
            (
                "00000000-0000-0000-0000-0000ffffffff",
                "entity/player/wide/zuri",
                SkinModel::Wide,
            ),
            (
                "80000000-0000-0000-0000-000000000000",
                "entity/player/wide/sunny",
                SkinModel::Wide,
            ),
            (
                "069a79f4-44e9-4726-a5be-fca90e38aaf5",
                "entity/player/slim/alex",
                SkinModel::Slim,
            ),
        ] {
            assert_eq!(
                default_skin(uuid, DefaultSkinSet::Modern),
                Some(DefaultSkin { path, model }),
                "{uuid}"
            );
        }
    }

    // 原版客户端 1.20.1 的 DefaultPlayerSkin 对固定种子随机 UUID 的实际输出：每个下标一条，哈希正负交替。
    // 与上面的合成向量不同，这组不经过本实现的推导，直接钉住游戏里的结果；与前端测试同一组。
    #[test]
    fn matches_vanilla_client_output() {
        let expected = [
            (
                "1c8d7b0d-84b6-8b9a-0dfc-ba7b668da3e0",
                "entity/player/slim/alex",
            ),
            (
                "6eaa7b86-d4ae-e15e-8578-599326c3ee04",
                "entity/player/slim/ari",
            ),
            (
                "ca9b54bd-467b-8b6f-bd28-eb64ed924f74",
                "entity/player/slim/efe",
            ),
            (
                "a8567abc-11d3-421c-3360-e9dbd4909684",
                "entity/player/slim/kai",
            ),
            (
                "6a3ca7c8-6cbc-813a-8c75-9f7177b96927",
                "entity/player/slim/makena",
            ),
            (
                "68f480c2-db19-898b-f0d9-4a89341afa65",
                "entity/player/slim/noor",
            ),
            (
                "7e9cdd8a-9ecb-dbea-9144-4563f5c9c6c7",
                "entity/player/slim/steve",
            ),
            (
                "6ee6e6fe-8274-b03a-a6e8-02877d3129b2",
                "entity/player/slim/sunny",
            ),
            (
                "c2bcc84d-76ca-d69c-502e-882370ed7b24",
                "entity/player/slim/zuri",
            ),
            (
                "ba870b27-51f5-bfbf-af02-f02822262271",
                "entity/player/wide/alex",
            ),
            (
                "4637278c-8e1f-e7fa-5c81-fadd69fa1f29",
                "entity/player/wide/ari",
            ),
            (
                "ef59f59a-e647-9213-a0be-f8379ed25c95",
                "entity/player/wide/efe",
            ),
            (
                "2385e2d5-db30-9fad-d2da-b43fbf489539",
                "entity/player/wide/kai",
            ),
            (
                "cb1bdd27-c509-3706-cf89-4645fdda28c3",
                "entity/player/wide/makena",
            ),
            (
                "b5093f23-b04e-9fc2-8920-712f403ec050",
                "entity/player/wide/noor",
            ),
            (
                "4059ff13-38a1-0302-f365-2e739b0fd277",
                "entity/player/wide/steve",
            ),
            (
                "dd65f65c-9448-2c9f-1c44-7d2ed65c3369",
                "entity/player/wide/sunny",
            ),
            (
                "06f83a78-c14d-33c9-b17e-1bef3f6e7275",
                "entity/player/wide/zuri",
            ),
        ];
        for (index, (uuid, path)) in expected.into_iter().enumerate() {
            let model = if index < 9 {
                SkinModel::Slim
            } else {
                SkinModel::Wide
            };
            assert_eq!(
                default_skin(uuid, DefaultSkinSet::Modern),
                Some(DefaultSkin { path, model }),
                "{uuid}"
            );
        }
    }

    #[test]
    fn offline_players_get_their_in_game_default() {
        for (name, path) in [
            ("Steve", "entity/player/wide/efe"),
            ("Alex", "entity/player/slim/makena"),
            ("Notch", "entity/player/slim/makena"),
            ("jeb_", "entity/player/slim/ari"),
            ("樱花", "entity/player/wide/steve"),
        ] {
            let uuid = petalfall_auth::offline_uuid(name).simple().to_string();
            assert_eq!(
                default_skin(&uuid, DefaultSkinSet::Modern).unwrap().path,
                path,
                "{name}"
            );
        }
    }

    // 1.13 到 1.19.2 的客户端（反汇编 1.13、1.16.5、1.19.2 核对过）：哈希最低位为 1 给细臂 alex，否则宽臂 steve。
    // 期望值由 JDK 的 java.util.UUID.hashCode 独立算出，与前端测试同一组；负哈希与 i32::MIN 钉住按补码取位。
    #[test]
    fn legacy_set_picks_alex_on_odd_hash_and_steve_on_even() {
        let steve = DefaultSkin {
            path: "entity/steve",
            model: SkinModel::Wide,
        };
        let alex = DefaultSkin {
            path: "entity/alex",
            model: SkinModel::Slim,
        };
        for (uuid, expected) in [
            ("00000000-0000-0000-0000-000000000000", steve),
            ("00000000-0000-0000-0000-000000000001", alex),
            ("00000000-0000-0000-0000-000000000011", alex),
            ("00000000-0000-0000-0000-00000000ffff", alex),
            ("00000000-0000-0000-0000-0000ffffffff", alex),
            ("00000000-0000-0000-0000-0000fffffffe", steve),
            ("80000000-0000-0000-0000-000000000000", steve),
            ("069a79f4-44e9-4726-a5be-fca90e38aaf5", steve),
            ("853c80ef-3c37-49fd-aa49-938b674adae6", alex),
        ] {
            assert_eq!(
                default_skin(uuid, DefaultSkinSet::Legacy),
                Some(expected),
                "{uuid}"
            );
        }
    }

    #[test]
    fn offline_players_on_a_legacy_source_get_steve_or_alex() {
        for (name, path) in [
            ("Steve", "entity/alex"),
            ("Alex", "entity/steve"),
            ("Notch", "entity/steve"),
            ("jeb_", "entity/alex"),
            ("Hanami", "entity/steve"),
            ("樱花", "entity/alex"),
        ] {
            let uuid = petalfall_auth::offline_uuid(name).simple().to_string();
            assert_eq!(
                default_skin(&uuid, DefaultSkinSet::Legacy).unwrap().path,
                path,
                "{name}"
            );
        }
    }

    fn files(paths: &[&str]) -> BTreeSet<String> {
        paths.iter().map(|p| (*p).to_owned()).collect()
    }

    #[test]
    fn source_with_the_two_old_skins_is_legacy() {
        let legacy = files(&["block/stone", "entity/alex", "entity/steve"]);
        assert_eq!(DefaultSkinSet::of_source(&legacy), DefaultSkinSet::Legacy);
        // 只抽到一张（另一张没通过校验）也还是旧版源
        let one = files(&["block/stone", "entity/alex"]);
        assert_eq!(DefaultSkinSet::of_source(&one), DefaultSkinSet::Legacy);
        // 1.19.3、1.19.4 没有 1.20 的方块，是降级源，但皮肤已经是 18 款
        let modern: BTreeSet<String> = std::iter::once("block/stone")
            .chain(DEFAULT_SKINS.iter().map(|skin| skin.path))
            .map(str::to_owned)
            .collect();
        assert_eq!(DefaultSkinSet::of_source(&modern), DefaultSkinSet::Modern);
        assert_eq!(
            DefaultSkinSet::of_source(&files(&["block/stone"])),
            DefaultSkinSet::Modern
        );
    }

    #[test]
    fn every_default_is_on_the_art_whitelist() {
        for skin in DEFAULT_SKINS {
            assert!(SKIN_TEXTURES.contains(&skin.path), "{}", skin.path);
            let folder = match skin.model {
                SkinModel::Slim => "entity/player/slim/",
                SkinModel::Wide => "entity/player/wide/",
            };
            assert!(skin.path.starts_with(folder), "{}", skin.path);
        }
        for skin in LEGACY_SKINS {
            assert!(SKIN_TEXTURES.contains(&skin.path), "{}", skin.path);
        }
    }
}
