// 实例库的筛选、搜索、排序与统计：纯函数，页面与测试共用。

import { gameDirSwitchReason } from "../../copy/flows";
import type { InstanceSummary } from "../../domain/adapt";
import type { LoaderKind } from "../../domain/types";
import { repairExit, type RepairExit } from "../../domain/rules";
import { REFLOW_MAX_ITEMS } from "../../lib/reflow";
import type { GameDirectoryDto } from "../../state/queries/catalog";
import type { SelectOption } from "../../ui/Select";
import { LIBRARY_COPY } from "./copy";

export type LoaderFilter = "all" | LoaderKind;
export type SortKey = "recent" | "name" | "playtime";

export const SORT_KEYS: readonly SortKey[] = ["recent", "name", "playtime"];

/** 分段筛选的顺序：原版在前，其后按加载器的常见程度。 */
export const FILTER_LOADERS: readonly LoaderKind[] = ["vanilla", "fabric", "forge", "neoforge", "quilt"];

export interface LibraryView {
  loader: LoaderFilter;
  query: string;
  sort: SortKey;
}

export const DEFAULT_VIEW: LibraryView = { loader: "all", query: "", sort: "recent" };

export function loaderCounts(list: readonly InstanceSummary[]): Readonly<Record<LoaderKind, number>> {
  const counts: Record<LoaderKind, number> = { vanilla: 0, fabric: 0, quilt: 0, forge: 0, neoforge: 0 };
  for (const inst of list) counts[inst.loader] += 1;
  return counts;
}

/** 搜索词去首尾空白后的小写形式；空串表示没有在搜索。 */
export function normalizedQuery(query: string): string {
  return query.trim().toLowerCase();
}

export function isFiltered(view: LibraryView): boolean {
  return view.loader !== "all" || normalizedQuery(view.query) !== "";
}

/** 搜索同时看实例名与整合包自己的名字：导入后实例可能被改了名，玩家记得的往往是整合包名。 */
export function matchesView(inst: InstanceSummary, view: LibraryView): boolean {
  if (view.loader !== "all" && inst.loader !== view.loader) return false;
  const q = normalizedQuery(view.query);
  if (q === "") return true;
  return inst.name.toLowerCase().includes(q) || (inst.modpack !== null && inst.modpack.name.toLowerCase().includes(q));
}

/**
 * 「最近」按最近一次活动排：没玩过的实例按创建时间算，刚新建、正在安装的实例因此排在最前面，能直接看到进度。
 * 两个时间都不知道的实例（文件系统不记录创建时间）排在最后。
 */
function activityOf(inst: InstanceSummary): number | null {
  if (inst.lastPlayedAt !== null) return inst.lastPlayedAt;
  return inst.createdAt;
}

function byRecent(a: InstanceSummary, b: InstanceSummary): number {
  const x = activityOf(a);
  const y = activityOf(b);
  if (x === null && y === null) return 0;
  if (x === null) return 1;
  if (y === null) return -1;
  return y - x;
}

/** 按累计时长倒序；游玩记录读不出的实例说不上多少，排在最后。 */
function byPlaytime(a: InstanceSummary, b: InstanceSummary): number {
  if (a.playtimeMin === null && b.playtimeMin === null) return 0;
  if (a.playtimeMin === null) return 1;
  if (b.playtimeMin === null) return -1;
  return b.playtimeMin - a.playtimeMin;
}

const nameCollator = new Intl.Collator("zh-CN");

export function sortInstances(list: readonly InstanceSummary[], key: SortKey): InstanceSummary[] {
  const out = [...list];
  if (key === "recent") out.sort(byRecent);
  else if (key === "name") out.sort((a, b) => nameCollator.compare(a.name, b.name));
  else out.sort(byPlaytime);
  return out;
}

export function visibleInstances(list: readonly InstanceSummary[], view: LibraryView): InstanceSummary[] {
  return sortInstances(
    list.filter((inst) => matchesView(inst, view)),
    view.sort,
  );
}

/** 全部实例的累计分钟；有实例的游玩记录读不出时加不出总数，为 null。 */
export function totalPlaytime(list: readonly InstanceSummary[]): number | null {
  let total = 0;
  for (const inst of list) {
    if (inst.playtimeMin === null) return null;
    total += inst.playtimeMin;
  }
  return total;
}

/** 进修复横幅的修不好的实例剩下的出路：整合包没导入完的（重新导入）不进横幅。 */
export type UnrepairableExit = Exclude<RepairExit, { kind: "reimport" }>;

/** 修复横幅里补全修不好的一个实例，连同后端给的原因（原文照写）与它剩下的出路。 */
export interface UnrepairableInstance {
  inst: InstanceSummary;
  reason: string;
  exit: UnrepairableExit;
  /**
   * 实例还带着未完成标记（装到一半，或上次补全没做完）。读不出来的重新扫描只让版本 JSON 重新读得出来，标记还在，
   * 扫完还要再修复一次，正文的提示不能说扫描就恢复。
   */
  pending: boolean;
}

/** 修复横幅列出的实例，按补全修不修得好分两拨，各自保持列表顺序；横幅在不在看两拨之和。 */
export interface BrokenInstances {
  /** 补全能修好的：横幅的按钮只为它们排补全。 */
  repairable: InstanceSummary[];
  /** 补全修不好的：补全一律被拒，横幅写明原因；只列着一个时按它的出路给按钮。 */
  unrepairable: UnrepairableInstance[];
}

/** 修复横幅列出的实例。整合包没导入完的补全补不回来，两拨都不进，各自在卡片上给重新导入。 */
export function brokenInstances(list: readonly InstanceSummary[]): BrokenInstances {
  const repairable: InstanceSummary[] = [];
  const unrepairable: UnrepairableInstance[] = [];
  for (const inst of list) {
    if (inst.status !== "broken") continue;
    const exit = repairExit(inst);
    if (exit === null) {
      repairable.push(inst);
      continue;
    }
    if (exit.kind === "reimport") continue;
    // 整合包没导入完的上面已排掉，剩下的修不好只能是后端报的；没有原因就是两边对修不修得好的判定错开了
    const reason = inst.unrepairableReason;
    if (reason === null) throw new Error(`实例 ${inst.id} 补全修不好，却没有原因`);
    unrepairable.push({ inst, reason, exit, pending: inst.incomplete !== null });
  }
  return { repairable, unrepairable };
}

/**
 * 修复横幅的标题。有修得好的时只数它们，修不好的留给正文各说各的。数的是需要修复的实例：此刻被锁着（被当作基底、在备份、
 * 在移动）的也算，「全部修复」这一轮跳过它们，它们留在横幅上，锁放开后照样能修。
 * 一个都修不好时不说需要修复；全是版本 JSON 读不出来的，文件可能完好、放开占用就恢复，说暂时无法启动而不说修不好；
 * 没有读不出来的说补全修不好；两样掺着时哪种说法都只对一部分，换成中性的无法启动。
 */
export function repairBannerTitle(broken: BrokenInstances): string {
  if (broken.repairable.length > 0) return LIBRARY_COPY.repairTitle(broken.repairable.length);
  const count = broken.unrepairable.length;
  if (count === 0) throw new Error("修复横幅没有要列的实例");
  const unreadable = broken.unrepairable.filter((u) => u.exit.kind === "rescan").length;
  if (unreadable === count) return LIBRARY_COPY.unreadableTitle(count);
  return unreadable === 0 ? LIBRARY_COPY.unrepairableTitle(count) : LIBRARY_COPY.mixedTitle(count);
}

/**
 * 目录选择的选项：标签只写目录名（触发器上显示的就是它），版本数放第二行；切不过去的禁用，第二行换成原因。
 * 当前目录的数取实例列表（instanceCount，读到之前为 null）：目录列表只在切换和回到本页时重读，新建、删除实例后还是旧数。
 * 版本数读不到时不取：盘拔掉时实例列表读到的是空，那不是 0 个版本。
 */
export function directoryOptions(dirs: readonly GameDirectoryDto[], busy: boolean, instanceCount: number | null): SelectOption<string>[] {
  return dirs.map((dir) => {
    const option = { value: dir.path, label: dir.name };
    const blocked = dir.is_current ? undefined : gameDirSwitchReason(busy, dir.available);
    if (blocked !== undefined) return { ...option, hint: blocked, disabled: true };
    const versions = dir.is_current && dir.version_count !== null && instanceCount !== null ? instanceCount : dir.version_count;
    return { ...option, hint: versions === null ? LIBRARY_COPY.dirVersionsUnknown : LIBRARY_COPY.dirVersions(versions) };
  });
}

/** 网格里排着什么：实例按显示顺序，外加末尾有没有新建卡。 */
export interface GridShape {
  ids: readonly string[];
  create: boolean;
}

/** 新建卡的格子键（React key 与比对都用它）：实例 id 是版本目录名，任何系统的文件名里都不能有「/」，撞不上实例。 */
export const CREATE_CELL_KEY = "/create";

function cellKeys(shape: GridShape): readonly string[] {
  return shape.create ? [...shape.ids, CREATE_CELL_KEY] : shape.ids;
}

/**
 * 这次变化是否整片换上：格子不逐个淡入淡出，网格整片淡入一次，这一次提交里让位暂停。
 * - 新来的格子或走掉的格子有一边超过让位上限（新建卡也算一格）：逐个播的入场或退场太多。两边分开数：
 *   前后各不超过上限、只是并集超过的（整段换了搜索词），进出各自逐个播，共同的卡片交给让位滑过去。
 * - 格子一个没增删、换了位置的超过上限（换排序）。
 * 被挤动的格子不计：删掉靠前的一张时其后几十张都被挤动，让位只给可见范围附近的起动画，量由视口封顶，不随实例数增长。
 */
export function crowdedChange(before: GridShape, after: GridShape): boolean {
  const previous = cellKeys(before);
  const next = cellKeys(after);
  const had = new Set(previous);
  const has = new Set(next);
  const entering = next.filter((key) => !had.has(key)).length;
  const leaving = previous.filter((key) => !has.has(key)).length;
  if (entering > 0 || leaving > 0) return Math.max(entering, leaving) > REFLOW_MAX_ITEMS;
  return next.filter((key, i) => previous[i] !== key).length > REFLOW_MAX_ITEMS;
}
