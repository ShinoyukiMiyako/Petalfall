// 「下载原版材质」部件：本机没有材质源、或只有 1.20 之前的版本（降级集）时说明材质从哪来、多大，给出下载按钮，
// 下载中显示阶段与字节进度。用不着下载（见 useArtDownloadOffered）时什么都不渲染：下完后材质绑定载入目录，部件随之消失。
// 下载、下完后等目录与失败都在状态层，全应用同一份；下载失败经统一错误处理报出，目录载入失败由材质绑定报出，按钮都换成重试。
// 封面方块选择框与外观页为方块材质放它；账户页、向导的账户步与设备码登录的成功区为默认皮肤放紧凑档（见 useSkinArtMissing）。

import { useIsPresent } from "framer-motion";
import { useId, useState } from "react";
import { Spinner } from "../../../art/Spinner";
import { useArtStatus, useBlockCatalog } from "../../../art/iso/catalog";
import { ART_DOWNLOAD_COPY } from "../../../copy/cover";
import { useDisplayCount } from "../../../lib/progress-display";
import { cx } from "../../../lib/util";
import { useRunner } from "../../../shell/useRunner";
import { useArtDownload, type ArtDownloadProgress } from "../../../state/art-download";
import { useOnline } from "../../../state/online";
import { useDrawsDefaultSkin } from "../../../state/queries/home";
import { Button } from "../../../ui/Button";
import { Inset } from "../../../ui/Inset";
import { ProgressBar } from "../../../ui/ProgressBar";
import styles from "./ArtDownload.module.css";

export interface ArtDownloadProps {
  /**
   * 紧凑档：按钮放在右侧，给设置行这类窄处用。没有材质源时不要标题、说明缩成一句，所在设置行的说明已经写了没有材质；
   * 材质不全时标题与说明照完整档的那一套，设置行的说明看不出不全。
   */
  compact?: boolean;
  /**
   * 紧凑档没有材质源时左侧的那句说明。默认只说从哪下、多大，所在设置行的说明已经交代了没有材质；
   * 账户页、向导与登录成功区周围没有这样的字，换成默认皮肤画不出来的那一句。
   */
  note?: string;
  /** 下载与重试用幽灵按钮：放在已有主按钮的向导步骤与弹窗正文里，入口只是顺带一提，不和主按钮抢。 */
  quiet?: boolean;
}

/**
 * 这一处画的账户（accountUuids）要不要配一个下载入口：其中有画默认皮肤的（离线账户、没有自定义皮肤的正版与外置账户），
 * 而本机没有材质源、默认皮肤读不到。画的都是自己的远程皮肤（不经材质源）、或选不出默认皮肤只能画人偶时，下了材质也换不出
 * 任何东西，不给入口；皮肤信息还在读时先不给，读到是默认皮肤再出现。
 * 只看材质源、不看方块目录：降级集里也有默认皮肤，方块目录没载入也不影响读皮肤。下完后材质源一换上入口就退场，
 * 默认皮肤随材质状态就地换上（useSkin 订阅着材质状态，换材质源时取图缓存整体作废）。
 */
export function useSkinArtMissing(accountUuids: readonly string[]): boolean {
  const missing = useArtStatus().source === null;
  const drawsDefault = useDrawsDefaultSkin(accountUuids);
  return missing && drawsDefault;
}

/**
 * 要不要给下载入口：本机没有材质源（方块目录为 null），或材质源是本机 1.20 之前的降级集（推荐里较新的方块缺材质，
 * 下载来的最新正式版会盖过它）。上次下载失败、可以重试时也算：降级时下完后材质状态已经换成下载来的版本，新目录却
 * 没能载入，留着的还是旧目录，不算的话重试无处可点。
 */
export function useArtDownloadOffered(): boolean {
  const catalog = useBlockCatalog();
  const source = useArtStatus().source;
  const { state } = useArtDownload();
  return catalog === null || (source !== null && source.degraded) || state.phase === "failed";
}

export function ArtDownload({ compact = false, note = ART_DOWNLOAD_COPY.compactBody, quiet = false }: ArtDownloadProps) {
  const offered = useArtDownloadOffered();
  const online = useOnline();
  // 已经有方块目录、只是不全（降级集，或下完后新目录没能载入、留着旧目录）：说明换成「不全」的那一版
  const incomplete = useBlockCatalog() !== null;
  const { state: live, download } = useArtDownload();
  const run = useRunner();
  // 放在出入场容器（Appear）里时，用不着下载了它就开始退场：退场期间留着最后一帧（多半是「正在载入材质…」）原地淡出，
  // 而不是先变空、再淡出一个空框。不在出入场容器里时恒为在场，照旧什么都不渲染。
  // 最后一帧按用不着之前最后看到的下载状态与说明重画：目录载入的同时下载状态已经回到空闲，照它画就成了「下载」按钮；
  // 没有材质时的说明也不能在载入目录的那一刻换成「不全」的那一版。
  // 记在状态里、渲染期按需更新，不在渲染期读写 ref：被丢弃的那次渲染不会把没提交过的画面记成最后一帧
  const present = useIsPresent();
  const [held, setHeld] = useState(live);
  const [heldIncomplete, setHeldIncomplete] = useState(incomplete);
  if (offered && held !== live) setHeld(live);
  if (offered && heldIncomplete !== incomplete) setHeldIncomplete(incomplete);
  const state = offered ? live : held;
  const partial = offered ? incomplete : heldIncomplete;
  // 挂载时的状态跟着所在弹窗或页面出现；之后每换一个阶段（开始下载、下完等载入、失败），换上来的状态淡入。
  // 按画出来的状态算：退场期间下载状态回到空闲也不算换阶段，正在淡出的那一帧不会重播入场
  const [firstPhase] = useState(state.phase);
  const [phaseChanged, setPhaseChanged] = useState(false);
  if (!phaseChanged && state.phase !== firstPhase) setPhaseChanged(true);
  if (!offered && present) return null;

  const failed = state.phase === "failed";
  // 下载与重下都要联网；下好了只是目录没载入时，重试只请材质绑定重查本机，断网照样能点
  const offlineReason = !online && !(state.phase === "failed" && state.stage === "catalog") ? ART_DOWNLOAD_COPY.offlineReason : undefined;
  const enter = phaseChanged || undefined;
  // 各阶段的状态按阶段换 key：同一种外形（空闲与失败都是按钮）之间换也重新挂上，入场淡入才会重播
  const status = state.phase === "running" ? (
    <DownloadProgress key="running" progress={state.progress} enter={phaseChanged} />
  ) : state.phase === "finishing" ? (
    <p key="finishing" className={styles.finishing} role="status" data-enter={enter}>
      <Spinner size={16} decorative />
      {ART_DOWNLOAD_COPY.finishing}
    </p>
  ) : (
    <div key={state.phase} className={styles.actions} data-enter={enter}>
      <Button
        size="sm"
        variant={quiet ? "ghost" : "secondary"}
        icon={failed ? "refresh" : "download"}
        disabledReason={offlineReason}
        onClick={() => void run(download, ART_DOWNLOAD_COPY.failed)}
      >
        {failed ? ART_DOWNLOAD_COPY.retry : ART_DOWNLOAD_COPY.download}
      </Button>
      {state.phase === "failed" && (
        <span className={styles.failed}>{state.stage === "download" ? ART_DOWNLOAD_COPY.failedNote : ART_DOWNLOAD_COPY.catalogFailedNote}</span>
      )}
    </div>
  );

  const progressing = state.phase === "running" || state.phase === "finishing";
  return compact ? (
    <div className={styles.compact}>
      {!progressing &&
        (partial ? (
          <div className={styles.compactText}>
            <p className={styles.title}>{ART_DOWNLOAD_COPY.degradedTitle}</p>
            <p className={styles.body}>{ART_DOWNLOAD_COPY.degradedBody}</p>
          </div>
        ) : (
          <p className={cx(styles.body, styles.compactBody)}>{note}</p>
        ))}
      {status}
    </div>
  ) : (
    <Inset className={styles.full}>
      <p className={styles.title}>{partial ? ART_DOWNLOAD_COPY.degradedTitle : ART_DOWNLOAD_COPY.title}</p>
      <p className={styles.body}>{partial ? ART_DOWNLOAD_COPY.degradedBody : ART_DOWNLOAD_COPY.body}</p>
      <div className={styles.status}>{status}</div>
    </Inset>
  );
}

function DownloadProgress({ progress, enter }: { progress: ArtDownloadProgress | null; enter: boolean }) {
  const stageId = useId();
  const stage = progress === null ? ART_DOWNLOAD_COPY.starting : ART_DOWNLOAD_COPY.stage[progress.stage];
  // 查清单与抽取阶段只有起止，没有字节可数：显示不定进度
  const counted = progress !== null && progress.downloaded > 0;
  const value = progress !== null && progress.total !== null && progress.total > 0 ? progress.downloaded / progress.total : null;
  return (
    <div className={styles.progress} data-enter={enter || undefined}>
      <div className={styles.progressText}>
        <span id={stageId} role="status">
          {stage}
        </span>
        {counted && <DownloadedBytes downloaded={progress.downloaded} total={progress.total} />}
      </div>
      <ProgressBar value={value} size="sm" track={progress?.stage} labelledBy={stageId} />
    </div>
  );
}

/**
 * 已下大小：进度条按每条进度事件走，这里的字照事件原样换一秒跳十次、读不清，按共用的节拍换。
 * 单独成一个组件：节拍到点补上最新值时只重画这一小段，进度条与阶段文字不跟着多画一次。
 */
function DownloadedBytes({ downloaded, total }: { downloaded: number; total: number | null }) {
  const shown = useDisplayCount(downloaded, total);
  return <span className={styles.bytes}>{ART_DOWNLOAD_COPY.bytes(shown, total)}</span>;
}
