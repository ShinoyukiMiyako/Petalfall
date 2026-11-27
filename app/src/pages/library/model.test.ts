import { describe, expect, it } from "vitest";
import { GAME_DIR_SWITCH_COPY, REPAIR_EXIT_COPY } from "../../copy/flows";
import type { InstanceSummary } from "../../domain/adapt";
import { REFLOW_MAX_ITEMS } from "../../lib/reflow";
import type { GameDirectoryDto } from "../../state/queries/catalog";
import { switchSuggestion } from "../shared/switch-suggestion";
import { LIBRARY_COPY } from "./copy";
import {
  brokenInstances,
  CREATE_CELL_KEY,
  crowdedChange,
  DEFAULT_VIEW,
  directoryOptions,
  isFiltered,
  loaderCounts,
  matchesView,
  repairBannerTitle,
  sortInstances,
  totalPlaytime,
  visibleInstances,
} from "./model";

function inst(id: string, overrides: Partial<InstanceSummary> = {}): InstanceSummary {
  return {
    id,
    name: id,
    mcVersion: "1.21.1",
    loader: "vanilla",
    loaderVersion: null,
    extraLoaders: [],
    releaseType: "release",
    javaMajor: 21,
    quickPlay: { singleplayer: true, multiplayer: true },
    description: null,
    cover: null,
    coverImage: null,
    coverImageHue: null,
    background: "follow",
    backgroundImage: null,
    pinned: false,
    status: "ready",
    brokenReason: null,
    unrepairableReason: null,
    unrepairableKind: null,
    renameTo: null,
    createdAt: 1_000,
    lastPlayedAt: null,
    playtimeMin: 0,
    playError: null,
    dirError: null,
    modCount: 0,
    worldCount: 0,
    resourcePackCount: 0,
    isolated: false,
    memoryMb: null,
    modpack: null,
    modpackError: null,
    lastCrashAt: null,
    lastCrashError: null,
    activity: null,
    usedBy: null,
    baseBusy: null,
    settingsError: null,
    incomplete: null,
    placeholder: false,
    ...overrides,
  };
}

const ids = (list: readonly InstanceSummary[]) => list.map((i) => i.id);

describe("loaderCounts", () => {
  it("按主加载器计数，没有的加载器为 0", () => {
    const list = [inst("a", { loader: "fabric" }), inst("b", { loader: "fabric" }), inst("c", { loader: "forge" }), inst("d")];
    expect(loaderCounts(list)).toEqual({ vanilla: 1, fabric: 2, quilt: 0, forge: 1, neoforge: 0 });
    expect(loaderCounts([])).toEqual({ vanilla: 0, fabric: 0, quilt: 0, forge: 0, neoforge: 0 });
  });
});

describe("筛选与搜索", () => {
  const list = [inst("机械动力", { loader: "forge" }), inst("Fabric 生存", { loader: "fabric" }), inst("原版空岛")];

  it("加载器筛选只留同加载器的实例", () => {
    expect(ids(visibleInstances(list, { ...DEFAULT_VIEW, loader: "forge" }))).toEqual(["机械动力"]);
    expect(ids(visibleInstances(list, { ...DEFAULT_VIEW, loader: "quilt" }))).toEqual([]);
  });

  it("搜索不分大小写、去首尾空白、按子串匹配，并与加载器筛选同时生效", () => {
    expect(ids(visibleInstances(list, { ...DEFAULT_VIEW, query: "  fabric " }))).toEqual(["Fabric 生存"]);
    expect(ids(visibleInstances(list, { ...DEFAULT_VIEW, query: "生存", loader: "vanilla" }))).toEqual([]);
    expect(matchesView(list[2], { ...DEFAULT_VIEW, query: "空" })).toBe(true);
  });

  it("只有空白的搜索词不算在搜索", () => {
    expect(isFiltered({ ...DEFAULT_VIEW, query: "   " })).toBe(false);
    expect(isFiltered({ ...DEFAULT_VIEW, query: "a" })).toBe(true);
    expect(isFiltered({ ...DEFAULT_VIEW, loader: "vanilla" })).toBe(true);
    expect(isFiltered(DEFAULT_VIEW)).toBe(false);
  });
});

describe("sortInstances", () => {
  it("最近游玩：按上次游玩倒序，没玩过的按创建时间算，两者都不知道的排最后", () => {
    const list = [
      inst("旧的", { lastPlayedAt: 100, createdAt: 10 }),
      inst("不知道", { lastPlayedAt: null, createdAt: null }),
      inst("刚新建", { lastPlayedAt: null, createdAt: 5_000 }),
      inst("昨天玩过", { lastPlayedAt: 4_000, createdAt: 20 }),
    ];
    expect(ids(sortInstances(list, "recent"))).toEqual(["刚新建", "昨天玩过", "旧的", "不知道"]);
  });

  it("名称：按中文排序规则升序；游玩时长：倒序", () => {
    const list = [inst("b", { playtimeMin: 5 }), inst("a", { playtimeMin: 0 }), inst("c", { playtimeMin: 300 })];
    expect(ids(sortInstances(list, "name"))).toEqual(["a", "b", "c"]);
    expect(ids(sortInstances(list, "playtime"))).toEqual(["c", "b", "a"]);
  });

  it("游玩时长：读不到的实例排在最后，彼此保持原序；不按 0 算", () => {
    const list = [inst("读不到甲", { playtimeMin: null }), inst("没玩过", { playtimeMin: 0 }), inst("读不到乙", { playtimeMin: null }), inst("玩过", { playtimeMin: 300 })];
    expect(ids(sortInstances(list, "playtime"))).toEqual(["玩过", "没玩过", "读不到甲", "读不到乙"]);
  });

  it("不改动传入的数组", () => {
    const list = [inst("b"), inst("a")];
    sortInstances(list, "name");
    expect(ids(list)).toEqual(["b", "a"]);
  });
});

describe("统计与签名", () => {
  it("累计时长求和、损坏实例按原顺序列出", () => {
    const list = [inst("a", { playtimeMin: 90 }), inst("b", { status: "broken", brokenReason: "缺少版本 JSON" }), inst("c", { playtimeMin: 30 })];
    expect(totalPlaytime(list)).toBe(120);
    expect(ids(brokenInstances(list).repairable)).toEqual(["b"]);
    expect(brokenInstances(list).unrepairable).toEqual([]);
    expect(LIBRARY_COPY.subtitle(3, 2, totalPlaytime(list))).toBe("共 3 个实例 · 2 个固定在侧栏 · 累计 2 小时");
  });

  it("有实例的游玩记录读不出时加不出总数，页头明说读不到，不写 0 也不只加读得出的部分", () => {
    const list = [inst("a", { playtimeMin: 90 }), inst("b", { playtimeMin: null })];
    expect(totalPlaytime(list)).toBeNull();
    expect(LIBRARY_COPY.subtitle(2, 1, totalPlaytime(list))).toBe("共 2 个实例 · 1 个固定在侧栏 · 累计时长读不到");
    expect(totalPlaytime([])).toBe(0);
  });

  it("修复横幅不收整合包没导入完的实例：补全补不回来；其他种类的未完成照收", () => {
    const marker = (kind: "install" | "repair" | "modpack") => ({ kind, outcome: "failed" as const, startedAt: null, spec: null, error: null });
    const list = [
      inst("pack", { status: "broken", brokenReason: "整合包没有导入完", incomplete: marker("modpack") }),
      inst("install", { status: "broken", brokenReason: "安装没有完成", incomplete: marker("install") }),
      inst("repair", { status: "broken", brokenReason: "补全没有完成", incomplete: marker("repair") }),
      // 正在重新补全的不算损坏
      inst("busy", { status: "installing", incomplete: marker("modpack") }),
    ];
    expect(ids(brokenInstances(list).repairable)).toEqual(["install", "repair"]);
    expect(brokenInstances(list).unrepairable).toEqual([]);
  });

  it("修复横幅把补全修不好的另列一拨，带上后端给的原因；修得好的、整合包没导入完的、没坏的都不进这一拨", () => {
    const reason = "缺少版本 JSON，也没有记下当初装的是什么";
    const marker = { kind: "modpack" as const, outcome: "failed" as const, startedAt: null, spec: null, error: null };
    const list = [
      inst("旧电脑拷来的", { status: "broken", brokenReason: "缺少版本 JSON", unrepairableReason: reason, mcVersion: null }),
      inst("坏掉的", { status: "broken", brokenReason: "缺少客户端 jar" }),
      inst("pack", { status: "broken", brokenReason: "整合包没有导入完", incomplete: marker }),
      inst("好的"),
    ];
    const broken = brokenInstances(list);
    expect(ids(broken.repairable)).toEqual(["坏掉的"]);
    expect(broken.unrepairable.map((u) => [u.inst.id, u.reason, u.exit])).toEqual([["旧电脑拷来的", reason, { kind: "delete" }]]);
  });

  it("修不好的一拨各带出路：读不出来的重新扫描（有安装记录也一样），带改名目标的改名，其余删除", () => {
    const unreadable = "版本 JSON 读不出来：文件 IO 失败: D:\\v\\网盘\\网盘.json：另一个程序正在使用此文件，进程无法访问。 (os error 32)";
    const lost = "缺少版本 JSON，也没有记下当初装的是什么";
    const hinted = `${lost}；目录里有一份叫 1.20.1-NeoForge.json 的版本文件，把实例改名成 1.20.1-NeoForge 就能认出来`;
    const install = { kind: "install" as const, outcome: "pending" as const, startedAt: null, spec: null, error: null };
    const list = [
      inst("网盘同步的", { status: "broken", brokenReason: unreadable, unrepairableReason: unreadable, unrepairableKind: "unreadable", incomplete: install, mcVersion: null }),
      inst("朋友发来的", { status: "broken", brokenReason: "缺少版本 JSON", unrepairableReason: hinted, unrepairableKind: "no_record", renameTo: "1.20.1-NeoForge", mcVersion: null }),
      inst("旧电脑拷来的", { status: "broken", brokenReason: "缺少版本 JSON", unrepairableReason: lost, unrepairableKind: "no_record", mcVersion: null }),
    ];
    expect(brokenInstances(list).unrepairable.map((u) => [u.inst.id, u.exit])).toEqual([
      ["网盘同步的", { kind: "rescan" }],
      ["朋友发来的", { kind: "rename", to: "1.20.1-NeoForge" }],
      ["旧电脑拷来的", { kind: "delete" }],
    ]);
  });

  it("修复横幅的标题：有修得好的只数修得好的，此刻被锁着、修复按钮这一轮跳过的也算", () => {
    const lost = "缺少版本 JSON，也没有记下当初装的是什么";
    // 第一个正在备份存档：「全部修复」这一轮不排它，标题照样数它
    const fixable = Array.from({ length: 1 + Math.floor(Math.random() * 4) }, (_, i) =>
      inst(`修得好${i}`, { status: "broken", brokenReason: "缺少客户端 jar", activity: i === 0 ? "backing_up" : null }),
    );
    const list = [...fixable, inst("修不好", { status: "broken", brokenReason: "缺少版本 JSON", unrepairableReason: lost, unrepairableKind: "no_record", mcVersion: null })];
    expect(repairBannerTitle(brokenInstances(list))).toBe(`${fixable.length} 个实例需要修复`);
  });

  it("修复横幅的标题：一个都修不好时，全是读不出来的说暂时无法启动，没有读不出来的说补全修不好，两样掺着说无法启动", () => {
    const unreadable = (id: string) => {
      const reason = `版本 JSON 读不出来：文件 IO 失败: D:\\v\\${id}\\${id}.json：拒绝访问。 (os error 5)`;
      return inst(id, { status: "broken", brokenReason: reason, unrepairableReason: reason, unrepairableKind: "unreadable", mcVersion: null });
    };
    const lost = inst("旧档", { status: "broken", brokenReason: "缺少版本 JSON", unrepairableReason: "缺少版本 JSON，也没有记下当初装的是什么", unrepairableKind: "no_record", mcVersion: null });
    const hinted = "缺少版本 JSON，也没有记下当初装的是什么；目录里有一份叫 1.20.1.json 的版本文件，把实例改名成 1.20.1 就能认出来";
    const renamable = inst("拷来的", { status: "broken", brokenReason: "缺少版本 JSON", unrepairableReason: hinted, unrepairableKind: "no_record", renameTo: "1.20.1", mcVersion: null });
    expect(repairBannerTitle(brokenInstances([unreadable("甲")]))).toBe("1 个实例暂时无法启动");
    expect(repairBannerTitle(brokenInstances([unreadable("甲"), inst("好的"), unreadable("乙")]))).toBe("2 个实例暂时无法启动");
    expect(repairBannerTitle(brokenInstances([unreadable("甲"), lost]))).toBe("2 个实例无法启动");
    expect(repairBannerTitle(brokenInstances([lost, unreadable("甲"), renamable, unreadable("乙")]))).toBe("4 个实例无法启动");
    expect(repairBannerTitle(brokenInstances([lost, renamable]))).toBe("2 个实例补全修不好");
    expect(repairBannerTitle(brokenInstances([lost]))).toBe("1 个实例补全修不好");
    expect(() => repairBannerTitle(brokenInstances([inst("好的")]))).toThrow("修复横幅没有要列的实例");
  });

  it("两拨各自保持列表里的顺序，不论修得好的与修不好的怎么交错", () => {
    const n = 3 + Math.floor(Math.random() * 6);
    const list = Array.from({ length: n }, (_, i) =>
      Math.random() < 0.5 ? inst(`修不好${i}`, { status: "broken", brokenReason: "缺少版本 JSON", unrepairableReason: `原因${i}`, mcVersion: null }) : inst(`修得好${i}`, { status: "broken", brokenReason: "缺少客户端 jar" }),
    );
    const broken = brokenInstances(list);
    expect(ids(broken.repairable)).toEqual(ids(list.filter((i) => i.unrepairableReason === null)));
    expect(broken.unrepairable.map((u) => [u.inst.id, u.reason])).toEqual(list.filter((i) => i.unrepairableReason !== null).map((i) => [i.id, i.unrepairableReason]));
    expect(broken.repairable.length + broken.unrepairable.length).toBe(n);
  });
});

describe("crowdedChange", () => {
  const range = (n: number, from = 0) => Array.from({ length: n }, (_, i) => `i${from + i}`);

  it("格子没有增删换序时不算，哪怕格子很多", () => {
    expect(crowdedChange({ ids: range(60), create: true }, { ids: range(60), create: true })).toBe(false);
  });

  it("换排序：换了位置的格子超过让位上限才整片换上", () => {
    const n = REFLOW_MAX_ITEMS;
    expect(crowdedChange({ ids: range(n), create: true }, { ids: [...range(n)].reverse(), create: true })).toBe(false);
    expect(crowdedChange({ ids: range(n + 2), create: true }, { ids: [...range(n + 2)].reverse(), create: true })).toBe(true);
    // 只有一张从后面挪到最前（刚玩过的实例按「最近」排上来）：换了位置的 30 格，没超过上限
    expect(crowdedChange({ ids: range(30), create: true }, { ids: ["i29", ...range(29)], create: true })).toBe(false);
  });

  it("增删：新来的或走掉的有一边超过让位上限才整片换上，新建卡也算一格；实例再多，删一张也逐个播", () => {
    const n = REFLOW_MAX_ITEMS;
    // 很多个里删掉靠前的一张：其后几十张被挤动，交给让位
    expect(crowdedChange({ ids: range(n + 20), create: true }, { ids: range(n + 19, 1), create: true })).toBe(false);
    // 从很多筛到只剩一个别的：走掉的连新建卡超过上限
    expect(crowdedChange({ ids: range(n), create: true }, { ids: range(1, n), create: false })).toBe(true);
    // 走掉的连新建卡恰好在上限上：逐个播
    expect(crowdedChange({ ids: range(n - 1), create: true }, { ids: range(1, n), create: false })).toBe(false);
    // 从很少到很多：新来的超过上限
    expect(crowdedChange({ ids: range(1), create: false }, { ids: range(n + 1), create: true })).toBe(true);
    // 只有新建卡出没
    expect(crowdedChange({ ids: range(n), create: false }, { ids: range(n), create: true })).toBe(false);
  });

  it("整段换搜索词：前后各不超过上限、只是并集超过的不整片换上，进出各自逐个播，共同的卡片交给让位", () => {
    const n = REFLOW_MAX_ITEMS;
    // 前后各 25 格、只有 5 格相同：并集 45
    expect(crowdedChange({ ids: range(25), create: false }, { ids: range(25, 20), create: false })).toBe(false);
    // 前后各恰好上限、一格也不相同：并集是上限的两倍
    expect(crowdedChange({ ids: range(n), create: false }, { ids: range(n, n), create: false })).toBe(false);
    expect(crowdedChange({ ids: range(n + 1), create: false }, { ids: range(n + 1, n + 1), create: false })).toBe(true);
  });

  it("新建卡的格子键带「/」：任何系统的目录名里都不能有它，撞不上实例 id", () => {
    expect(CREATE_CELL_KEY).toContain("/");
  });
});

function dir(name: string, version_count: number | null, overrides: Partial<GameDirectoryDto> = {}): GameDirectoryDto {
  return { name, path: `D:\\${name}\\.minecraft`, is_current: false, available: true, version_count, ...overrides };
}

const current = dir("默认目录", 0, { is_current: true });

describe("switchSuggestion", () => {
  it("取其它目录里版本最多的，一样多时取记录在前的", () => {
    const list = [current, dir("官方", 3), dir("PCL2", 12), dir("HMCL", 12)];
    expect(switchSuggestion(list)).toEqual({ dir: list[2], versions: 12 });
  });

  it("当前目录再多也不算：已经在这里了", () => {
    expect(switchSuggestion([dir("默认目录", 40, { is_current: true }), dir("官方", 1)])?.dir.name).toBe("官方");
    expect(switchSuggestion([dir("默认目录", 40, { is_current: true })])).toBeNull();
  });

  it("访问不到的目录切不过去，版本数读不到与 0 个版本都不算有东西", () => {
    expect(switchSuggestion([current, dir("移动硬盘", 30, { available: false })])).toBeNull();
    expect(switchSuggestion([current, dir("读不到", null)])).toBeNull();
    expect(switchSuggestion([current, dir("空的", 0)])).toBeNull();
    expect(switchSuggestion([current, dir("移动硬盘", 30, { available: false }), dir("读不到", null), dir("官方", 1)])?.dir.name).toBe("官方");
  });

  it("没有目录记录时没有可推荐的", () => {
    expect(switchSuggestion([])).toBeNull();
  });
});

describe("directoryOptions", () => {
  const list = [dir("默认目录", 7, { is_current: true }), dir("PCL2", 12), dir("读不到", null), dir("移动硬盘", null, { available: false })];

  it("标签只写目录名，版本数放第二行，读不到时明说，不写 0", () => {
    expect(directoryOptions(list, false, null).map((o) => [o.label, o.hint, o.disabled])).toEqual([
      ["默认目录", "7 个版本", undefined],
      ["PCL2", "12 个版本", undefined],
      ["读不到", "版本数读不到", undefined],
      ["移动硬盘", "位置访问不到，无法切过去", true],
    ]);
  });

  it("值是路径：两个目录重名时也分得开", () => {
    const twins = [dir("HMCL", 1, { is_current: true, path: "D:\\a" }), dir("HMCL", 2, { path: "D:\\b" })];
    expect(directoryOptions(twins, false, null).map((o) => o.value)).toEqual(["D:\\a", "D:\\b"]);
  });

  it("当前目录的版本数跟实例列表走，别的目录不受影响；0 个实例不是读不到", () => {
    expect(directoryOptions(list, false, 9).slice(0, 2).map((o) => o.hint)).toEqual(["9 个版本", "12 个版本"]);
    expect(directoryOptions(list, false, 0)[0].hint).toBe("0 个版本");
  });

  it("有游戏或任务在进行时别的目录都切不了，原因优先于访问不到；当前目录不受影响", () => {
    expect(directoryOptions(list, true, 7).map((o) => [o.hint, o.disabled])).toEqual([
      ["7 个版本", undefined],
      ["有游戏或下载在进行，结束后再切换", true],
      ["有游戏或下载在进行，结束后再切换", true],
      ["有游戏或下载在进行，结束后再切换", true],
    ]);
  });

  it("当前目录访问不到也不禁用：它不是切过去的目标", () => {
    const [option] = directoryOptions([dir("默认目录", null, { is_current: true, available: false })], false, null);
    expect([option.hint, option.disabled]).toEqual(["版本数读不到", undefined]);
  });

  it("当前目录的版本数读不到时不跟实例列表走：盘拔掉时实例列表读到的是空，不等于 0 个版本", () => {
    const [option] = directoryOptions([dir("移动硬盘", null, { is_current: true, available: false }), dir("PCL2", 12)], false, 0);
    expect(option.hint).toBe("版本数读不到");
  });
});

describe("文案", () => {
  it("目录空状态的主按钮：中西文之间留空格，中文名不留", () => {
    expect(GAME_DIR_SWITCH_COPY.emptyAction("PCL2")).toBe("切到 PCL2");
    expect(GAME_DIR_SWITCH_COPY.emptyAction("官方启动器")).toBe("切到官方启动器");
    expect(GAME_DIR_SWITCH_COPY.emptyBody("PCL2", 12)).toBe("「PCL2」里有 12 个版本，切过去就能玩，文件不会被移动。");
  });

  it("筛选无结果的正文：带加载器时中西文之间留空格，原版不留", () => {
    expect(LIBRARY_COPY.noMatchQuery("abc", "fabric")).toBe("没有名字里带「abc」的 Fabric 实例。");
    expect(LIBRARY_COPY.noMatchQuery("abc", null)).toBe("没有名字里带「abc」的实例。");
    expect(LIBRARY_COPY.noMatchLoader("quilt")).toBe("还没有 Quilt 实例。");
    expect(LIBRARY_COPY.noMatchLoader("vanilla")).toBe("还没有原版实例。");
  });

  it("修复横幅逐个列出实例名", () => {
    expect(LIBRARY_COPY.repairBody(["GT: New Horizons", "RLCraft"], [])).toBe("「GT: New Horizons」「RLCraft」缺少游戏文件，补全后就能启动。");
  });

  it("修复横幅：修不好的另起一句、各带自己的原因，不说补全后就能启动；只有修不好的时只出这一句", () => {
    const reason = "缺少版本 JSON，也没有记下当初装的是什么";
    expect(LIBRARY_COPY.repairBody(["坏掉的"], [{ name: "旧电脑拷来的", reason, exit: "delete" }])).toBe("「坏掉的」缺少游戏文件，补全后就能启动。「旧电脑拷来的」补全修不好：缺少版本 JSON，也没有记下当初装的是什么。");
    const both = [
      { name: "甲", reason, exit: "delete" as const },
      { name: "乙", reason: "版本 JSON 损坏，也没有记下当初装的是什么", exit: "delete" as const },
    ];
    expect(LIBRARY_COPY.repairBody([], both)).toBe("「甲」补全修不好：缺少版本 JSON，也没有记下当初装的是什么。「乙」补全修不好：版本 JSON 损坏，也没有记下当初装的是什么。");
  });

  it("修复横幅：原因相同的修不好的实例并成一句，按各原因第一次出现的先后排，不逐个重复", () => {
    const lost = "缺少版本 JSON，也没有记下当初装的是什么";
    const broken = "版本 JSON 损坏，也没有记下当初装的是什么";
    const list = [
      { name: "甲", reason: lost, exit: "delete" as const },
      { name: "乙", reason: broken, exit: "delete" as const },
      { name: "丙", reason: lost, exit: "delete" as const },
    ];
    expect(LIBRARY_COPY.repairBody([], list)).toBe(`「甲」「丙」补全修不好：${lost}。「乙」补全修不好：${broken}。`);
    expect(LIBRARY_COPY.repairBody(["坏掉的"], list.slice(0, 1).concat(list.slice(2)))).toBe(`「坏掉的」缺少游戏文件，补全后就能启动。「甲」「丙」补全修不好：${lost}。`);
    // 随机多少个同一原因的实例：原因只出现一次，名字按列表顺序一个不少
    const names = Array.from({ length: 2 + Math.floor(Math.random() * 6) }, (_, i) => `旧档${i}`);
    const merged = LIBRARY_COPY.repairBody([], names.map((name) => ({ name, reason: lost, exit: "delete" as const })));
    expect(merged).toBe(`${names.map((n) => `「${n}」`).join("")}补全修不好：${lost}。`);
    expect(merged.split(lost)).toHaveLength(2);
  });

  it("修复横幅：同一句带改名提示的原因不合并，按后端真实的全角分号格式各自成句；只能删除的照旧合并，先后不乱", () => {
    const lost = "缺少版本 JSON，也没有记下当初装的是什么";
    // 两份拷来的副本目录里都只有 1.20.1.json：后端给两者同一句原因，但实例名唯一，最多一个改得成
    const hinted = `${lost}；目录里有一份叫 1.20.1.json 的版本文件，把实例改名成 1.20.1 就能认出来`;
    const body = LIBRARY_COPY.repairBody(
      [],
      [
        { name: "生存", reason: hinted, exit: "rename" },
        { name: "旧档甲", reason: lost, exit: "delete" },
        { name: "创造", reason: hinted, exit: "rename" },
        { name: "旧档乙", reason: lost, exit: "delete" },
      ],
    );
    expect(body).toBe(`「生存」补全修不好：${hinted}。「旧档甲」「旧档乙」补全修不好：${lost}。「创造」补全修不好：${hinted}。`);
    expect(body.split("把实例改名成 1.20.1 就能认出来")).toHaveLength(3);
  });

  it("修复横幅：读不出来的说暂时无法启动，原因句末补上句号后接怎么恢复；同一句原因也不合并", () => {
    const reason = "版本 JSON 读不出来：文件 IO 失败: D:\\Games\\Launcher\\.minecraft\\versions\\网盘同步的\\网盘同步的.json：另一个程序正在使用此文件，进程无法访问。 (os error 32)";
    expect(LIBRARY_COPY.repairBody([], [{ name: "网盘同步的", reason, exit: "rescan" }])).toBe(
      `「网盘同步的」暂时无法启动：${reason}。关掉占用它的程序或修好读取权限后，重新扫描就能恢复。`,
    );
    const same = "版本 JSON 读不出来：拒绝访问。";
    expect(
      LIBRARY_COPY.repairBody(
        ["坏掉的"],
        [
          { name: "甲", reason: same, exit: "rescan" },
          { name: "乙", reason: same, exit: "rescan" },
        ],
      ),
    ).toBe(`「坏掉的」缺少游戏文件，补全后就能启动。「甲」暂时无法启动：${same}「乙」暂时无法启动：${same}${REPAIR_EXIT_COPY.unreadableHint}`);
  });

  it("修复横幅：几个读不出来的各自说原因、排在一起，怎么恢复只在末尾说一次；中间夹着的只能删除的排到它们后面", () => {
    const lost = "缺少版本 JSON，也没有记下当初装的是什么";
    const locked = (id: string) => `版本 JSON 读不出来：文件 IO 失败: D:\\v\\${id}\\${id}.json：另一个程序正在使用此文件，进程无法访问。 (os error 32)`;
    const names = Array.from({ length: 2 + Math.floor(Math.random() * 4) }, (_, i) => `网盘${i}`);
    // 每个读不出来的后面都夹一个只能删除的：读不出来的仍按第一次出现的位置排成一段
    const lines = names.flatMap((name, i) => [
      { name, reason: locked(name), exit: "rescan" as const },
      { name: `旧档${i}`, reason: lost, exit: "delete" as const },
    ]);
    const body = LIBRARY_COPY.repairBody([], lines);
    const unreadable = names.map((name) => `「${name}」暂时无法启动：${locked(name)}。`).join("");
    expect(body).toBe(`${unreadable}${REPAIR_EXIT_COPY.unreadableHint}${names.map((_, i) => `「旧档${i}」`).join("")}补全修不好：${lost}。`);
    expect(body.split(REPAIR_EXIT_COPY.unreadableHint)).toHaveLength(2);
  });

  it("修复横幅的标题：一个都修不好时说补全修不好，全是读不出来的说暂时无法启动，两样掺着说无法启动，不说需要修复；有修得好的照旧", () => {
    expect(LIBRARY_COPY.unrepairableTitle(1)).toBe("1 个实例补全修不好");
    expect(LIBRARY_COPY.unrepairableTitle(3)).toBe("3 个实例补全修不好");
    expect(LIBRARY_COPY.unreadableTitle(2)).toBe("2 个实例暂时无法启动");
    expect(LIBRARY_COPY.mixedTitle(2)).toBe("2 个实例无法启动");
    expect(LIBRARY_COPY.repairTitle(2)).toBe("2 个实例需要修复");
  });

  it("修复横幅：原因已带句末标点或后跟一句提示时照原文接上，不再补句号", () => {
    const withHint = "缺少版本 JSON，也没有记下当初装的是什么。目录里还有一份别的版本文件，可能是改名时留下的。";
    expect(LIBRARY_COPY.repairBody([], [{ name: "甲", reason: withHint, exit: "delete" }])).toBe(`「甲」补全修不好：${withHint}`);
    expect(LIBRARY_COPY.repairBody([], [{ name: "乙", reason: "Version JSON is missing.", exit: "delete" }])).toBe("「乙」补全修不好：Version JSON is missing.");
  });

  it("修复横幅两拨都空时是调用错了：横幅不该出现", () => {
    expect(() => LIBRARY_COPY.repairBody([], [])).toThrow("修复横幅没有要列的实例");
  });
});
