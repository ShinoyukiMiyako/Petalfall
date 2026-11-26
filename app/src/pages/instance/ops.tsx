// 头图菜单、设置页「实例管理」与崩溃页共用的一组实例操作：重命名、固定、打开文件夹、导出整合包、移到别的游戏目录、补全与删除。
// 几处入口的锁定判断与反馈都从这里出，保证完全一致（删除确认与实例库共用 delete.tsx）。动作一律经外层统一错误处理执行。

import { ACTION_ERROR, lockReason, MODPACK_REIMPORT_COPY } from "../../copy/flows";
import type { InstanceSummary } from "../../domain/adapt";
import { locksOf, type InstanceLocks } from "../../domain/instance-locks";
import { canRepair, instanceNameError, needsReimport } from "../../domain/rules";
import { useRunner } from "../../shell/useRunner";
import { shallowArrayEqual, useLauncherActions, useLauncherSelector } from "../../state/launcher";
import { busyInstanceId } from "../../state/selectors";
import type { LauncherActions } from "../../state/store";
import { HERO_COPY, LOCK_REASON, OVERVIEW_COPY } from "./copy";
import { useDeleteInstance } from "./delete";
import { workLock, type WorkLock } from "./model";

/** 打开文件夹的目标，与状态层动作的入参同一类型。 */
export type FolderTarget = Parameters<LauncherActions["openFolder"]>[0];

/** 打开前后端要先占住实例（缺了要建出来）的工作子目录；实例文件夹本身与单个存档只是定位，不占。 */
const WORK_FOLDERS: ReadonlySet<FolderTarget> = new Set<FolderTarget>(["mods", "saves", "resourcepacks", "shaderpacks", "logs", "session_logs", "screenshots"]);

export interface InstanceOps {
  locks: InstanceLocks;
  /** 打开工作子目录与写实例设置的锁：安装、补全、改名、删除进行中；游戏运行不挡。 */
  work: WorkLock | null;
  busy: boolean;
  installing: boolean;
  /** 校验新名字；与当前名字相同（只改大小写也算不同）时为 null。 */
  nameError(name: string): string | null;
  /** 成功返回 true；失败已由统一错误处理报告。 */
  rename(name: string): Promise<boolean>;
  togglePin(): void;
  openFolder(target: FolderTarget, world?: string): void;
  /** 这个目标此刻打不开的原因；能打开为 undefined。 */
  folderReason(target: FolderTarget): string | undefined;
  /** 实例设置此刻改不了的原因；能改为 undefined，可以直接交给 disabledReason。 */
  settingsReason: string | undefined;
  /** 此刻不能导出整合包的原因；能导出为 undefined。菜单项与导出弹窗的主按钮共用它。 */
  exportReason: string | undefined;
  /** 此刻不能把存档带进整合包的原因；能带为 undefined。 */
  exportSavesReason: string | undefined;
  /** 此刻不能移到别的游戏目录的原因；能移为 undefined。菜单项与移动弹窗的主按钮共用它。 */
  moveReason: string | undefined;
  /** 此刻不能补全的原因；能补全为 undefined。头图菜单、概览横幅、设置页与崩溃页的补全入口共用它。 */
  repairReason: string | undefined;
  /** 更新检查设置此刻改不了的原因：只写卷宗不碰 jar，游戏运行不挡；但要占住实例，安装、补全、改名、删除挡着。能改为 undefined。 */
  updateHoldReason: string | undefined;
  /** 此刻不能改动 Mod 的原因（启停、更新、移除、回滚）：既要换 jar（游戏运行、补全挡着），也要占住实例（改名、删除挡着）。能改动为 undefined。 */
  modsReason: string | undefined;
  repair(): void;
  reinstallLoader(): void;
  remove(): void;
}

/**
 * 损坏实例上导出、移动与进入存档被挡住的原因。修得好的照旧说修复后才能做（repairable）；整合包没导入完的补全补不回来，
 * 指向重新导入；后端说补全修不好的不承诺修复，写明后端给的原因。整合包半成品先判：头图菜单、概览与设置页给它的入口都是重新导入。
 */
export function brokenBlockReason(inst: Pick<InstanceSummary, "status" | "incomplete" | "unrepairableReason">, repairable: string): string {
  if (needsReimport(inst)) return MODPACK_REIMPORT_COPY.note;
  if (inst.unrepairableReason !== null) return LOCK_REASON.unrepairable(inst.unrepairableReason);
  return repairable;
}

/**
 * 导出读的是此刻的文件，与「改动中」同属一类：游戏运行、存档备份、装 Mod 都不挡；安装、补全、改名、删除会挪走或重写目录，挡住。
 * 占位项的目录还没落盘，损坏的实例写不出可用的整合包。
 */
export function exportReason(inst: InstanceSummary, work: WorkLock | null): string | undefined {
  if (inst.placeholder) return LOCK_REASON.export.placeholder;
  if (work !== null) return LOCK_REASON.export[work];
  if (inst.status === "broken") return brokenBlockReason(inst, LOCK_REASON.export.broken);
  return undefined;
}

/**
 * 带上存档要把存档整份读走，后端在存档会变的时候以 instance_busy 拒绝：游戏启动或运行中，或者正在备份、恢复。
 * 这里提前禁用那一项；共用存档时别的实例挡住的情况要看隔离，这里不判，由后端报。
 */
export function exportSavesReason(inst: Pick<InstanceSummary, "activity">, busy: boolean): string | undefined {
  if (inst.activity === "backing_up") return LOCK_REASON.export.savesBackingUp;
  return busy || inst.activity === "launching" || inst.activity === "running" ? LOCK_REASON.export.savesInGame : undefined;
}

/**
 * 移到别的游戏目录：要整份读走实例目录、从基底带走客户端 jar，实例与基底上的活动都挡它（见 InstanceLocks.move）。
 * 占位项的目录还没落盘；损坏的实例读不出版本 JSON，算不出要带过去的共用文件。只有一个游戏目录时照样能开：可以选一个文件夹。
 */
export function moveReason(inst: InstanceSummary, locks: InstanceLocks): string | undefined {
  if (inst.placeholder) return LOCK_REASON.move.placeholder;
  const locked = lockReason(locks.move, "move");
  if (locked !== undefined) return locked;
  return inst.status === "broken" ? brokenBlockReason(inst, LOCK_REASON.move.broken) : undefined;
}

/**
 * 补全：修不好的实例后端一律拒绝，不管此刻排不排得上，原因照后端给的原文写（它可能已带句末标点或一句提示，不再改写）；
 * 修得好的才看实例与基底的锁。整合包半成品在头图、概览与设置页先给重新导入，只有崩溃页的补全按钮不分这一支；
 * 后端在它版本 JSON 还在时不报原因，用重新导入的说明垫上。
 */
export function repairReason(inst: InstanceSummary, locks: InstanceLocks): string | undefined {
  if (canRepair(inst)) return lockReason(locks.repair, "repair");
  if (inst.unrepairableReason !== null) return inst.unrepairableReason;
  if (needsReimport(inst)) return MODPACK_REIMPORT_COPY.note;
  throw new Error(`实例 ${inst.id} 补全修不好，却既没有原因也不是没导入完的整合包`);
}

export function useInstanceOps(inst: InstanceSummary): InstanceOps {
  const actions = useLauncherActions();
  const run = useRunner();
  const deleteInstance = useDeleteInstance();
  const id = inst.id;
  const busy = useLauncherSelector((s) => busyInstanceId(s) === id);
  // 改名校验要全部实例名；只订阅名字，别的实例的进度与状态变化不让页面重渲染
  const ids = useLauncherSelector((s) => s.instances.map((i) => i.id), shallowArrayEqual);
  const installing = inst.status === "installing";
  const locks = locksOf(inst, busy);
  const work = workLock(installing, inst.activity);
  const name = inst.name;

  const nameError = (next: string) => (next.trim() === name ? null : instanceNameError(next.trim(), ids, id));

  const rename = async (next: string): Promise<boolean> => {
    const trimmed = next.trim();
    if (trimmed === name) return true;
    const outcome = await run(() => actions.renameInstance(id, trimmed), ACTION_ERROR.renameInstance);
    if (outcome.ok) actions.toast("success", HERO_COPY.renamed(outcome.value));
    return outcome.ok;
  };

  const togglePin = () => {
    void run(() => actions.togglePin(id), ACTION_ERROR.pin);
  };

  const openFolder = (target: FolderTarget, world?: string) => {
    void run(() => actions.openFolder(target, id, world === undefined ? null : world), ACTION_ERROR.openFolder);
  };

  const folderReason = (target: FolderTarget) => {
    // 占位项的目录要等任务排到、导入要等读完整合包定下名字才建，前端分不清建了没有，一律先挡住
    if (inst.placeholder) return LOCK_REASON.placeholderFolder;
    return work === null || !WORK_FOLDERS.has(target) ? undefined : LOCK_REASON.work.folder[work];
  };
  const settingsReason = work === null ? undefined : LOCK_REASON.work.settings[work];
  const updateHoldReason = work === null ? undefined : LOCK_REASON.work.mods[work];
  const modsLock = lockReason(locks.mods, "mods");

  const repair = () => {
    void run(() => actions.repairInstance(id), ACTION_ERROR.repair).then((outcome) => {
      if (outcome.ok) actions.toast("info", OVERVIEW_COPY.repairStarted(name), OVERVIEW_COPY.repairStartedBody);
    });
  };

  const reinstallLoader = () => {
    void run(() => actions.repairInstance(id, { reinstallLoader: true }), ACTION_ERROR.reinstallLoader).then((outcome) => {
      if (outcome.ok) actions.toast("info", OVERVIEW_COPY.reinstallLoaderStarted(name), OVERVIEW_COPY.repairStartedBody);
    });
  };

  const remove = () => {
    void deleteInstance(inst);
  };

  return {
    locks,
    work,
    busy,
    installing,
    nameError,
    rename,
    togglePin,
    openFolder,
    folderReason,
    settingsReason,
    exportReason: exportReason(inst, work),
    exportSavesReason: exportSavesReason(inst, busy),
    moveReason: moveReason(inst, locks),
    repairReason: repairReason(inst, locks),
    updateHoldReason,
    modsReason: modsLock !== undefined ? modsLock : updateHoldReason,
    repair,
    reinstallLoader,
    remove,
  };
}
