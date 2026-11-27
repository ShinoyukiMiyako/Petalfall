import { useMemo } from "react";
import { cx } from "../../lib/util";
import { LruCache } from "../iso/render";
import { BitmapCanvas } from "./BitmapCanvas";
import { branchCell, paintBranch, rasterizeBranch } from "./branch-art";
import styles from "./BlossomBranch.module.css";
import { pixelsToCanvas, useDevicePixelRatio } from "./canvas";

export interface BlossomBranchProps {
  /** 插画框的 CSS 尺寸；构图按 meet 等比放进框内，主枝从左缘伸进来。 */
  width: number;
  height: number;
  /** 水平镜像：放在左侧时让枝条从画面外伸入。 */
  mirror?: boolean;
  /** 账户舞台底纹用半透明；头图与向导用不透明。 */
  dim?: boolean;
  className?: string;
}

/** 画好的一幅：格数（定画布的 CSS 尺寸）与一比一贴上的位图。 */
interface PaintedBranch {
  cols: number;
  rows: number;
  bitmap: OffscreenCanvas;
}

/**
 * 画好的花枝按 框宽 | 框高 | 每格设备像素 | dpr 共用：账户页每张账户卡垫的是同一尺寸的一幅，各画各的，一屏几张卡就要
 * 逐格画几遍、各占一份位图。镜像与半透明都在外框的样式上，不进键。
 * 各处的框都是写死的几档，算上窗口拖到缩放不同的屏幕后的另一套 dpr，也到不了上限；超出时丢最久没用的一幅。
 */
export const BRANCH_CACHE_LIMIT = 16;
// 被挤出的位图不主动释放：还挂着的插画照样拿着它，交给垃圾回收
const painted = new LruCache<string, PaintedBranch>(BRANCH_CACHE_LIMIT);

function paintedBranch(width: number, height: number, cellDevice: number, dpr: number): PaintedBranch {
  const key = `${width}|${height}|${cellDevice}|${dpr}`;
  const hit = painted.get(key);
  if (hit !== undefined) return hit;
  const art = paintBranch(width, height, cellDevice / dpr);
  const made = { cols: art.cols, rows: art.rows, bitmap: pixelsToCanvas(rasterizeBranch(art, cellDevice)) };
  painted.set(key, made);
  return made;
}

/**
 * 花枝插画：全站唯一的大插画，静态。账户页的聚光卡和每张账户卡各垫一幅，一屏可能有好几幅；同尺寸的共用一张位图。
 * 像素画按设备像素现画：每格取整数个设备像素，画布只占整格覆盖到的部分（不足一格的余量留在右下），
 * 贴上去是一比一，不会被浏览器再缩放。
 */
export function BlossomBranch({ width, height, mirror = false, dim = false, className }: BlossomBranchProps) {
  if (!(width > 0 && height > 0)) throw new Error(`花枝插画尺寸必须为正：${width}x${height}`);
  const dpr = useDevicePixelRatio();
  const cellDevice = branchCell(dpr);
  const { cols, rows, bitmap } = useMemo(() => paintedBranch(width, height, cellDevice, dpr), [width, height, cellDevice, dpr]);
  return (
    <div className={cx(styles.branch, className)} style={{ width, height }} aria-hidden="true" data-mirror={mirror || undefined} data-dim={dim || undefined}>
      <BitmapCanvas bitmap={bitmap} cssWidth={(cols * cellDevice) / dpr} cssHeight={(rows * cellDevice) / dpr} dpr={dpr} className={styles.art} />
    </div>
  );
}
