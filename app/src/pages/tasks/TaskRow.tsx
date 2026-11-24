// 任务行：类型图标（带结局角标）、标题与类别标、详情、进度条（仅活动任务；分步任务两条，上面整体、下面这一步）、元信息行、右侧操作。

import { Icon, type IconName } from "../../art/Icon";
import { TASK_KIND_LABEL, taskStatusLabel } from "../../copy/flows";
import type { Task, TaskStatus } from "../../domain/types";
import { useLoadingGate } from "../../lib/loading-gate";
import { useDisplayCount, useDisplaySpeed } from "../../lib/progress-display";
import { cx } from "../../lib/util";
import { isActiveTask, isPhasedTask, progressTrack, settledOnRestart, stageProgress } from "../../state/tasks";
import { Button } from "../../ui/Button";
import { IconButton } from "../../ui/IconButton";
import { ListRow } from "../../ui/ListRow";
import { ProgressBar } from "../../ui/ProgressBar";
import { Tag } from "../../ui/Tag";
import { TASKS_COPY } from "./copy";
import { usePacedEtaSpeed } from "./hooks";
import { KIND_ICON, rowMeta, type RowMetaContext } from "./model";
import { SmoothPercent } from "../../ui/SmoothPercent";
import styles from "./tasks.module.css";

type BadgeTone = "ok" | "danger" | "muted";

const OUTCOME_BADGE: Readonly<Partial<Record<TaskStatus, { tone: BadgeTone; icon: IconName }>>> = {
  done: { tone: "ok", icon: "check" },
  failed: { tone: "danger", icon: "alert" },
  cancelled: { tone: "muted", icon: "x" },
};

/** 列表里任务行的标记，值是任务 id：页面靠它找到换组前后的同一行，让新行从旧行的位置飞过去。 */
export const TASK_ROW_ATTR = "data-task-row";

function KindTile({ task, active }: { task: Task; active: boolean }) {
  const badge = OUTCOME_BADGE[task.status];
  return (
    <span
      className={styles.tile}
      data-active={active ? "" : undefined}
      role="img"
      aria-label={`${TASK_KIND_LABEL[task.kind]}，${taskStatusLabel(task)}`}
    >
      <Icon name={KIND_ICON[task.kind]} size={20} />
      {badge !== undefined && (
        <span className={styles.badge} data-tone={badge.tone}>
          <Icon name={badge.icon} size={12} />
        </span>
      )}
    </span>
  );
}

/** 元信息行的输入里由行外给的那些；已下字节与速度由元信息段自己按节拍取。 */
export type TaskMetaContext = Omit<RowMetaContext, "bytes" | "speed">;

/**
 * 元信息行。运行中的已下大小与速度按节拍换字（lib/progress-display 的 useDisplayCount、useDisplaySpeed），剩余时间跟已下大小同一拍：
 * 节拍放在这一小段里，到点补字时只重画这一段，整行不跟着重渲染。
 */
function TaskMeta({ task, context }: { task: Task; context: TaskMetaContext }) {
  const bytes = useDisplayCount(task.bytes, task.totalBytes);
  const speed = useDisplaySpeed(task.speedBps);
  const etaSpeed = usePacedEtaSpeed(bytes, context.etaSpeed);
  const meta = rowMeta(task, { ...context, etaSpeed, bytes, speed });
  return (
    <span className={styles.rowMeta}>
      <span className={styles.rowMetaLeft} data-danger={meta.danger ? "" : undefined} title={meta.left}>
        {meta.left}
      </span>
      <span className={styles.rowMetaRight}>{meta.right}</span>
    </span>
  );
}

/**
 * 分步任务这一步走了多少。整体进度条在一步里只挪一小段，装到「执行加载器安装器」这类给不出进度的步骤还会停住不动，
 * 单看它分不清是在干活还是卡住了：这一条照当前这一步的那批进度走，这一步给不出进度时画不定进度。
 * 排队中与整体进度条一样是静态空轨道。
 *
 * 给不出进度不马上换成不定进度：多数步骤进来几十毫秒就有第一条快照，换过去再换回来只是闪一下（减少动画时不定进度是
 * 整条满宽的脉冲，闪得更显眼）。用全站加载态的那个门：等够 LOADING_DELAY_MS 仍给不出才换，换了至少停留同样长；
 * 接连几步都给不出时一直是不定进度，不逐步重新等。
 */
function StageBar({ task }: { task: Task }) {
  const stage = stageProgress(task);
  const gate = useLoadingGate(task.status === "running" && stage.value === null);
  const value = task.status === "queued" ? 0 : gate.view === "skeleton" ? null : stage.value === null ? 0 : stage.value;
  return <ProgressBar className={styles.stageBar} value={value} size="sm" track={stage.track} label={TASKS_COPY.row.stageLabel(task.title)} />;
}

export interface TaskRowProps {
  task: Task;
  /**
   * 刚完成、还在收尾（lib/progress-display 的 useFinishing）：仍按进行中的样子画，进度条与百分数在原处推到满格，
   * 尾部不变形（取消键照旧占位，但已不可用：任务已经完成，没有可取消的）；收尾结束后页面才把这一行换到已结束。
   */
  finishing: boolean;
  meta: TaskMetaContext;
  /** 关联的实例还在实例列表里（新建被取消后实例已撤掉，就不给「查看实例」）。 */
  hasInstance: boolean;
  /** 重试不行时的原因（发起于别的游戏目录，或断网时必然失败）：按钮置灰并在悬停时说明；能重试为 undefined。 */
  retryBlocked: string | undefined;
  onCancel(): void;
  onRetry(): void;
  /** 打开需要手动下载的文件清单；只有以这个原因失败的任务才有入口。 */
  onViewBlocked(): void;
  onViewInstance(): void;
}

export function TaskRow({ task, finishing, meta, hasInstance, retryBlocked, onCancel, onRetry, onViewBlocked, onViewInstance }: TaskRowProps) {
  const active = isActiveTask(task) || finishing;
  const viewInstance = hasInstance && (
    <Button variant="ghost" size="sm" iconRight="arrow-right" onClick={onViewInstance}>
      {TASKS_COPY.row.viewInstance}
    </Button>
  );

  return (
    <ListRow
      size="lg"
      className={cx(styles.rowItem, !active && (task.status === "done" || task.status === "cancelled") && styles.rowMuted)}
      leading={<KindTile task={task} active={active} />}
      title={
        <span className={styles.titleLine} {...{ [TASK_ROW_ATTR]: task.id }}>
          <span className={styles.titleText} title={task.title}>
            {task.title}
          </span>
          <Tag size="sm" tone="outline">
            {TASK_KIND_LABEL[task.kind]}
          </Tag>
          {task.status === "failed" && (
            <Tag size="sm" tone="danger">
              {TASKS_COPY.row.failedTag}
            </Tag>
          )}
        </span>
      }
      meta={task.detail === "" ? undefined : <span title={task.detail}>{task.detail}</span>}
      progress={
        <span className={styles.rowFoot}>
          {active && (
            // 排队中给静态空轨道，不用不定进度：一屏几十条循环光带会让同屏动画失控
            <ProgressBar
              value={task.status === "queued" ? 0 : task.progress}
              tone={task.status === "queued" ? "muted" : "accent"}
              size="sm"
              track={progressTrack(task)}
              label={TASKS_COPY.row.progressLabel(task.title)}
            />
          )}
          {/* 按任务的命令给，不等第一条步骤事件：槽位从排队起就占着，轮到运行、步骤报来时行高不变，下面的行不被顶开 */}
          {active && isPhasedTask(task) && <StageBar task={task} />}
          <TaskMeta task={task} context={meta} />
        </span>
      }
      trailing={
        <span className={styles.side}>
          {/* 百分数槽从排队起就占着：轮到运行时尾部不变宽，进度条与元信息不被挤短 */}
          {active &&
            (task.status === "queued" ? (
              <span className={styles.pct} aria-hidden="true" />
            ) : (
              <SmoothPercent className={styles.pct} value={task.progress} track={progressTrack(task)} />
            ))}
          {/* 收尾期任务已经完成：取消键留在原位、尾部不变宽，但不可用，读屏不会听到一个点了没反应的操作；
              焦点可能正停在它上面，所以保持可聚焦，不用原生禁用把焦点丢回页面开头 */}
          {active && <IconButton icon="x" label={TASKS_COPY.row.cancel} size="sm" disabled={finishing} focusableWhenDisabled onClick={onCancel} />}
          {/* 带手动下载清单的失败不给重试，重排走清单：「自动下载受限的 CurseForge 文件」关着时原样重跑只会再失败，文件找齐后
              由清单重排；开着时清单里随时能点「重新安装」。关闭时中断的移动已由后端收尾，没有可重试的 */}
          {task.status === "failed" &&
            !settledOnRestart(task) &&
            (task.error?.blocked_files === undefined ? (
              <Button variant="secondary" size="sm" disabledReason={retryBlocked} onClick={onRetry}>
                {TASKS_COPY.row.retry}
              </Button>
            ) : (
              <Button variant="secondary" size="sm" onClick={onViewBlocked}>
                {TASKS_COPY.row.viewBlocked}
              </Button>
            ))}
          {!active && viewInstance}
        </span>
      }
    />
  );
}
