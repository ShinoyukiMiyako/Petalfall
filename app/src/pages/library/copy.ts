// 实例库页面自己的文案。跨页面共用的（加载器名、启动入口、删除确认、通知目录）从 copy/flows 引用，这里不重复。

import { joinMixed, LOADER_NAME, PAGE_LABEL, REPAIR_EXIT_COPY, sentence } from "../../copy/flows";
import { formatPlaytime } from "../../domain/format";
import type { LoaderKind } from "../../domain/types";
import type { SortKey, UnrepairableExit } from "./model";

/** 横幅正文里一个补全修不好的实例：名字、后端给的原因原文与它剩下的出路。 */
export interface UnrepairableLine {
  name: string;
  reason: string;
  exit: UnrepairableExit["kind"];
}

const quoted = (names: readonly string[]) => names.map((n) => `「${n}」`).join("");

/** 修不好的实例在正文里的一句。读不出来的文件可能完好，说暂时无法启动并接上怎么恢复，不说补全修不好。 */
function unrepairableSentence(names: readonly string[], reason: string, exit: UnrepairableExit["kind"]): string {
  if (exit === "rescan") return `${quoted(names)}暂时无法启动：${sentence(reason)}${REPAIR_EXIT_COPY.unreadableHint}`;
  return sentence(`${quoted(names)}补全修不好：${reason}`);
}

export const LIBRARY_COPY = {
  title: PAGE_LABEL.library,
  subtitle: (count: number, pinned: number, minutes: number | null) =>
    `共 ${count} 个实例 · ${pinned} 个固定在侧栏 · ${minutes === null ? "累计时长读不到" : `累计 ${formatPlaytime(minutes)}`}`,
  subtitleEmpty: "还没有实例",
  importModpack: "导入整合包",
  create: "新建实例",

  filterLabel: "按加载器筛选",
  filterAll: "全部",
  searchPlaceholder: "搜索实例或整合包",
  sortLabel: "排序方式",
  sort: { recent: "最近游玩", name: "名称", playtime: "游玩时长" } satisfies Readonly<Record<SortKey, string>>,
  directoryLabel: "游戏目录",
  dirVersions: (count: number) => `${count} 个版本`,
  dirVersionsUnknown: "版本数读不到",

  /** 有修得好的实例时的标题，count 只数修得好的：与修复按钮能兑现的个数一致。 */
  repairTitle: (count: number) => `${count} 个实例需要修复`,
  /** 横幅列出的实例一个都修不好时的标题：补全一律被拒，不说需要修复。 */
  unrepairableTitle: (count: number) => `${count} 个实例补全修不好`,
  /** 一个都修不好、而且全是版本 JSON 读不出来的：文件可能完好，放开占用或修好权限就恢复，不说修不好。 */
  unreadableTitle: (count: number) => `${count} 个实例暂时无法启动`,
  /**
   * 横幅正文：补全修得好的合成一句；修不好的另起，写明后端给的原因，不承诺补全后就能启动。只能删除的实例原因相同时并成一句
   * （从旧电脑拷来的一批实例往往是同一个原因）。带改名目标的不并：原因里那句改名提示只核对过这一个实例改不改得成，
   * 实例名不分大小写唯一，几个实例最多一个能照做；读不出来的各自带着自己的路径，也不并。按各句第一次出现的先后排，
   * 只有一拨时只出那一拨。
   */
  repairBody: (repairable: readonly string[], unrepairable: readonly UnrepairableLine[]) => {
    if (repairable.length === 0 && unrepairable.length === 0) throw new Error("修复横幅没有要列的实例");
    const fixable = repairable.length === 0 ? "" : `${quoted(repairable)}缺少游戏文件，补全后就能启动。`;
    const lines: { names: string[]; reason: string; exit: UnrepairableExit["kind"] }[] = [];
    const mergeable = new Map<string, string[]>();
    for (const { name, reason, exit } of unrepairable) {
      const names = exit === "delete" ? mergeable.get(reason) : undefined;
      if (names !== undefined) {
        names.push(name);
        continue;
      }
      const line = { names: [name], reason, exit };
      lines.push(line);
      if (exit === "delete") mergeable.set(reason, line.names);
    }
    return fixable + lines.map((line) => unrepairableSentence(line.names, line.reason, line.exit)).join("");
  },
  repairOne: "立即修复",
  repairAll: "全部修复",

  current: "当前",
  openCard: (name: string) => `打开「${name}」`,
  moreLabel: (name: string) => `「${name}」的更多操作`,
  noDescription: "自建实例，还没写介绍",
  statsLabel: "游玩概况",
  lastPlayed: "上次游玩",
  playtime: "累计时长",
  playUnknown: "读不到",
  modCount: "Mod 数",
  worldCount: "存档数",

  pin: (name: string) => `把「${name}」固定到侧栏`,
  unpin: (name: string) => `从侧栏取消固定「${name}」`,
  pinned: (name: string) => `已把「${name}」固定到侧栏`,
  unpinned: (name: string) => `已从侧栏取消固定「${name}」`,

  menu: {
    open: "打开详情",
    select: "设为当前实例",
    selected: "已是当前实例",
    pin: "固定到侧栏",
    unpin: "从侧栏取消固定",
    pinFull: "侧栏最多固定 5 个实例",
    openFolder: "打开实例文件夹",
    move: "移到其他游戏目录…",
    mods: "Mod 管理",
    settings: "实例设置",
    logs: "日志与历史",
    crash: "崩溃诊断",
    crashActive: "上次崩溃",
    remove: "删除实例",
  },

  createTileTitle: "新建实例",
  createTileBody: "选版本、加载器和封面，新实例自动固定到侧栏",

  emptyTitle: "实例库还是空的",
  emptyBody: "新建一个实例，选好版本、加载器和封面；或者去发现页把整合包导入成实例。",
  emptyAction: "去发现导入整合包",

  noMatchTitle: "没有符合条件的实例",
  noMatchQuery: (query: string, loader: LoaderKind | null) =>
    loader === null ? `没有名字里带「${query}」的实例。` : `没有名字里带「${query}」${joinMixed("的", LOADER_NAME[loader], "实例。")}`,
  noMatchLoader: (loader: LoaderKind) => joinMixed("还没有", LOADER_NAME[loader], "实例。"),
  clearFilter: "清除筛选",

  loadErrorTitle: "读不到实例列表",
  retry: "重试",
  openGameDir: "打开游戏目录",
  /** 游戏目录被更新的版本用过、或记数据格式的文件读不懂时的标题：正文说清原因，出路是换一个目录。 */
  blockedTitle: "这个游戏目录现在打不开",
  changeGameDir: "换一个游戏目录",

  gridLabel: "实例",
  loadingLabel: "正在读取实例列表",
} as const;
