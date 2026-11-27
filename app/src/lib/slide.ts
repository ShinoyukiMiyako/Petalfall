// 合成线程上的弹簧位移：整条轨迹事先按弹簧的解析解采样成关键帧，交给 Web Animations 去播。
//
// 为什么不用 framer-motion 的运动值：那是主线程上每帧一次的 JS。点侧栏换页、数据到达这些时刻主线程正忙着渲染，
// 由它驱动的位移会跟着卡住；事先算好的关键帧一旦提交就由合成线程自己播完。
// 途中改目标时按解析解算出此刻的位置与速度，带着速度接着走，不从零速重新起步。

import { springAt, type SpringName } from "./motion";

/** 关键帧的采样间隔：120 Hz，合成线程在两点之间线性插值。 */
const SAMPLE_MS = 1000 / 120;
/** 离目标的距离与速度都落到这以下就算停稳。 */
const REST_PX = 0.25;
const REST_PX_PER_S = 8;
/** 轨迹的最长时长：带着很大的初速度也到此为止。 */
const MAX_TRACK_MS = 1200;

const round = (value: number) => Math.round(value * 100) / 100;

/**
 * 从离目标 (dx, dy)、速度 (vx, vy) 回到目标 (restX, restY) 的整条轨迹。
 * 帧在时间轴上等距，末帧恰好落在目标上。
 */
export function springTrack(name: SpringName, dx: number, vx: number, dy: number, vy: number, restX = 0, restY = 0): { frames: Keyframe[]; duration: number } {
  const frames: Keyframe[] = [];
  for (let i = 0; ; i++) {
    const ms = i * SAMPLE_MS;
    const x = springAt(name, dx, vx, ms / 1000);
    const y = springAt(name, dy, vy, ms / 1000);
    const resting = Math.abs(x.x) < REST_PX && Math.abs(y.x) < REST_PX && Math.abs(x.v) < REST_PX_PER_S && Math.abs(y.v) < REST_PX_PER_S;
    if ((resting && i > 0) || ms >= MAX_TRACK_MS) break;
    frames.push({ translate: `${round(restX + x.x)}px ${round(restY + y.x)}px` });
  }
  frames.push({ translate: `${round(restX)}px ${round(restY)}px` });
  return { frames, duration: (frames.length - 1) * SAMPLE_MS };
}

/** 缩放离 1 的偏差与速度（每秒）都落到这以下就算停稳：20 px 的图标上不到 0.02 px。 */
const REST_SCALE = 0.001;
const REST_SCALE_PER_S = 0.05;

/**
 * 缩放从 from 静止出发、沿弹簧回到 1 的整条轨迹，写在独立的 scale 属性上（与 transform 叠加，互不覆盖）。
 * 取样间隔与时长上限同 springTrack。给不经 framer-motion 的单个元素弹出用（按钮图标位换上来的图标）。
 */
export function scaleTrack(name: SpringName, from: number): { frames: Keyframe[]; duration: number } {
  const frames: Keyframe[] = [];
  for (let i = 0; ; i++) {
    const ms = i * SAMPLE_MS;
    const at = springAt(name, from - 1, 0, ms / 1000);
    if ((i > 0 && Math.abs(at.x) < REST_SCALE && Math.abs(at.v) < REST_SCALE_PER_S) || ms >= MAX_TRACK_MS) break;
    frames.push({ scale: String(Math.round((1 + at.x) * 10000) / 10000) });
  }
  frames.push({ scale: "1" });
  return { frames, duration: (frames.length - 1) * SAMPLE_MS };
}

/** 把采样好的一条轨迹交给 Web Animations 播：整段在合成线程上走，帧之间线性相连。 */
export function playTrack(el: HTMLElement, track: { frames: Keyframe[]; duration: number }): Animation {
  return el.animate(track.frames, { duration: track.duration, easing: "linear" });
}

/**
 * 把 lib/motion 的补间（fade.in、fade.out 这类）交给 Web Animations 播：时长与三次贝塞尔照搬，
 * 与 framer-motion 播同一个补间时一样。用于透明度这类不需要弹簧的值。
 */
export function playTween(el: HTMLElement, frames: Keyframe[], tween: { duration: number; ease: readonly [number, number, number, number] }): Animation {
  return el.animate(frames, { duration: tween.duration * 1000, easing: `cubic-bezier(${tween.ease.join(", ")})` });
}

/**
 * 动画已经播了多少秒；还没开始计时的按 0。起步时刻钉在此刻之后、还没走到的（当前时间为负）也按 0：
 * 弹簧的解析解往负的时间外推会算出起点之外的位置与一个凭空的速度。
 */
export function elapsedSeconds(animation: Animation): number {
  const time = animation.currentTime;
  return typeof time === "number" && time > 0 ? time / 1000 : 0;
}

/**
 * 文档时间轴此刻的时间。途中接续时旧动画走了多久（currentTime）就是相对它算的，接着走的那一截起步时刻钉在这里才与读数同一刻。
 * 它在帧与帧之间停在上一帧的帧时间上，比 performance.now() 早：钉在后者上，旧轨迹要等到那时才接着走，在动的东西先顿一截。
 */
export function timelineNow(doc: Document): number {
  const time = doc.timeline.currentTime;
  if (typeof time !== "number") throw new Error(`文档时间轴没有可用的时间（${String(time)}），接续的起步时刻无从钉起`);
  return time;
}

interface Flight {
  animation: Animation;
  dx: number;
  vx: number;
  dy: number;
  vy: number;
}

/**
 * 把一个元素沿弹簧滑到指定位置。位置写在元素的 translate 上：静止时是内联样式，途中由动画顶替。
 * 元素不能另外再用 translate 定位。
 */
export class Slide {
  private x = 0;
  private y = 0;
  private flight: Flight | null = null;

  constructor(
    private readonly el: HTMLElement,
    private readonly spring: SpringName,
  ) {}

  /** 此刻的位置：途中是轨迹上的点，静止时就是目标。 */
  at(): { x: number; y: number } {
    const now = this.offset();
    return { x: this.x + now.dx, y: this.y + now.dy };
  }

  /** 直接到位，不走弹簧（首次摆放、跟手、减少动画）。 */
  jump(x: number, y: number): void {
    this.flight?.animation.cancel();
    this.flight = null;
    this.place(x, y);
  }

  /** 沿弹簧滑过去；途中再调用时带着当前速度转向。运行环境没有 Web Animations（组件测试的 jsdom）时直接到位。 */
  to(x: number, y: number): void {
    if (typeof this.el.animate !== "function") {
      this.jump(x, y);
      return;
    }
    const now = this.offset();
    const dx = this.x + now.dx - x;
    const dy = this.y + now.dy - y;
    // 旧的一段已在走（起步时刻已定）：新的一段起步时刻钉在文档时间轴此刻，与读旧轨迹的那一刻对齐。不钉的话新建的动画要到
    // 带着它的那一帧（忙时更晚）才定起步时刻，这期间停在首帧，在动的东西先顿一两帧再接着走。
    // 旧的一段自己还没定起步时刻（同一刻里连着转向）就不钉，与它一样交给引擎在本该起步的那一帧定。
    // 先取时刻再取消：取不到时报错，旧的一段与登记都原样留着
    const pin = this.flight !== null && typeof this.flight.animation.startTime === "number" ? timelineNow(this.el.ownerDocument) : null;
    this.flight?.animation.cancel();
    this.flight = null;
    this.place(x, y);
    if (Math.abs(dx) < REST_PX && Math.abs(dy) < REST_PX && Math.abs(now.vx) < REST_PX_PER_S && Math.abs(now.vy) < REST_PX_PER_S) return;
    const animation = playTrack(this.el, springTrack(this.spring, dx, now.vx, dy, now.vy, x, y));
    if (pin !== null) animation.startTime = pin;
    const flight: Flight = { animation, dx, vx: now.vx, dy, vy: now.vy };
    this.flight = flight;
    animation.onfinish = () => {
      if (this.flight === flight) this.flight = null;
    };
  }

  private place(x: number, y: number): void {
    this.x = x;
    this.y = y;
    this.el.style.translate = `${round(x)}px ${round(y)}px`;
  }

  private offset(): { dx: number; vx: number; dy: number; vy: number } {
    const flight = this.flight;
    if (flight === null) return { dx: 0, vx: 0, dy: 0, vy: 0 };
    const t = elapsedSeconds(flight.animation);
    const x = springAt(this.spring, flight.dx, flight.vx, t);
    const y = springAt(this.spring, flight.dy, flight.vy, t);
    return { dx: x.x, vx: x.v, dy: y.x, vy: y.v };
  }
}
