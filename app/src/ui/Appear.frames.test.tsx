// @vitest-environment jsdom
// Appear 逐帧看入场从哪一帧开始走的用例。整个文件走假时钟、用 stepFrames 一帧一帧地推：
// 每个用例开始前先在真实计时器上把帧循环排空再换假时钟，收尾先卸载、再排空、再换回真实计时器（见 test/dom 的 drainFrames）。
import { MotionConfig, type Transition } from "framer-motion";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { fade } from "../lib/motion";
import { drainFrames, render, stepFrames, type Rendered } from "../test/dom";
import { Appear } from "./Appear";

let view: Rendered | undefined;

beforeEach(async () => {
  await drainFrames();
  vi.useFakeTimers();
});

afterEach(async () => {
  view?.unmount();
  view = undefined;
  await drainFrames();
  vi.useRealTimers();
});

const tree = (show: boolean, reducedMotion: "never" | "always" = "never", initial?: boolean) => (
  <MotionConfig reducedMotion={reducedMotion}>
    <Appear initial={initial}>{show && <p data-note="">提示</p>}</Appear>
  </MotionConfig>
);

const wrap = (): HTMLElement => {
  const el = view?.container.querySelector<HTMLElement>("[data-note]")?.parentElement;
  if (el === null || el === undefined) throw new Error("子项没有挂上");
  return el;
};

it.each([
  ["之后才出现的子项", "never", undefined],
  ["之后才出现的子项", "always", undefined],
  ["initial 为真时挂载时就在的子项", "never", true],
] as const)("%s（减少动画 %s）：第一帧画的仍是起点，画出来之后才开始入场，挂载那一帧再长也不吃掉入场的开头", async (who, reducedMotion, initial) => {
  if (who === "之后才出现的子项") {
    view = render(tree(false, reducedMotion));
    view.rerender(tree(true, reducedMotion));
  } else {
    view = render(tree(true, reducedMotion, initial));
  }
  const scaled = reducedMotion === "never";
  await stepFrames(1);
  expect(wrap().style.opacity).toBe("0");
  expect(wrap().style.transform).toBe(scaled ? "scale(0.98)" : "");
  // 放行排在绘制之后的任务里，假时钟要再推进一帧才到点
  await stepFrames(2);
  const opacity = Number(wrap().style.opacity);
  expect(opacity).toBeGreaterThan(0);
  expect(opacity).toBeLessThan(1);
  if (scaled) {
    const scale = /^scale\((.+)\)$/.exec(wrap().style.transform);
    if (scale === null) throw new Error(`子项没有在落到位的半路上：${wrap().style.transform}`);
    expect(Number(scale[1])).toBeGreaterThan(0.98);
    expect(Number(scale[1])).toBeLessThan(1);
  } else {
    // 减少动画时只有透明度在走
    expect(wrap().style.transform).toBe("");
  }
  await stepFrames(60);
  expect(wrap().style.opacity).toBe("1");
  expect(wrap().style.transform).toBe(scaled ? "none" : "");
});

it("画出第一帧之前就撤掉的子项：还停在看不见的起点，不播退场，随即移除；入场起步之后再撤掉的照常留在原处淡出", async () => {
  view = render(tree(false));
  view.rerender(tree(true));
  const early = wrap();
  view.rerender(tree(false));
  await stepFrames(1);
  expect(early.isConnected).toBe(false);

  view.rerender(tree(true));
  await stepFrames(3);
  const late = wrap();
  expect(Number(late.style.opacity)).toBeGreaterThan(0);
  view.rerender(tree(false));
  await stepFrames(1);
  expect(late.isConnected).toBe(true);
  expect(late.hasAttribute("inert")).toBe(true);
  await stepFrames(60);
  expect(late.isConnected).toBe(false);
});

const tuned = (show: boolean, motion: { exitTransition?: Transition; enterDelay?: number } = {}) => (
  <MotionConfig reducedMotion="never">
    <Appear exitTransition={motion.exitTransition} enterDelay={motion.enterDelay}>
      {show && <p data-note="">提示</p>}
    </Appear>
  </MotionConfig>
);

it("撤掉的那次渲染给了 exitTransition：按它退场，fade.none 时第一帧就不在了，不留在原处淡出", async () => {
  view = render(tuned(true));
  const el = wrap();
  view.rerender(tuned(false, { exitTransition: fade.none }));
  await stepFrames(1);
  expect(el.isConnected).toBe(false);
});

it("exitTransition 只认撤掉那一次渲染给的：之前给过、撤掉时不给的照常原地淡出", async () => {
  view = render(tuned(true, { exitTransition: fade.none }));
  const el = wrap();
  view.rerender(tuned(false));
  await stepFrames(1);
  expect(el.isConnected).toBe(true);
  expect(el.hasAttribute("inert")).toBe(true);
  await stepFrames(3);
  const opacity = Number(el.style.opacity);
  expect(opacity).toBeGreaterThan(0);
  expect(opacity).toBeLessThan(1);
  await stepFrames(60);
  expect(el.isConnected).toBe(false);
});

it("给了 enterDelay 的子项：画出第一帧之后再停这么久才开始入场，透明度与缩放一起晚起步", async () => {
  view = render(tuned(false));
  view.rerender(tuned(true, { enterDelay: 0.1 }));
  // 不带延迟时第 3 帧已在入场半路上（见第一条用例）；放行之后再停 100 ms，约 6 帧
  await stepFrames(3);
  expect(wrap().style.opacity).toBe("0");
  expect(wrap().style.transform).toBe("scale(0.98)");
  await stepFrames(4);
  expect(wrap().style.opacity).toBe("0");
  expect(wrap().style.transform).toBe("scale(0.98)");
  await stepFrames(4);
  const opacity = Number(wrap().style.opacity);
  expect(opacity).toBeGreaterThan(0);
  expect(opacity).toBeLessThan(1);
  expect(wrap().style.transform).not.toBe("scale(0.98)");
  await stepFrames(60);
  expect(wrap().style.opacity).toBe("1");
  expect(wrap().style.transform).toBe("none");
});

it("挂载时就在的子项：Appear 之后再渲染也不退回起点重新入场", async () => {
  view = render(tree(true));
  expect(wrap().style.opacity).toBe("1");
  // 挂载之后 Appear 每次渲染给子项的 own 都为真，挂载时就在的这一项不能因此当成新出现的
  view.rerender(tree(true));
  await stepFrames(3);
  expect(wrap().style.opacity).toBe("1");
  expect(wrap().style.transform).toBe("none");
});

interface Options {
  reducedMotion?: "never" | "always";
  enterDelay?: number;
  enterStart?: "painted" | "commit";
  exitInLayout?: boolean;
}

const configured = (show: boolean, options: Options = {}) => (
  <MotionConfig reducedMotion={options.reducedMotion ?? "never"}>
    <Appear enterDelay={options.enterDelay} enterStart={options.enterStart} exitInLayout={options.exitInLayout}>
      {show && <p data-note="">提示</p>}
    </Appear>
  </MotionConfig>
);

interface ExitFrame {
  /** popLayout 把它抽出布局时打的标记。 */
  popped: boolean;
  inert: boolean;
  opacity: number;
  transform: string;
}

/** 挂着一项，撤掉它，逐帧记下退场的样子，直到它被移除。 */
async function recordExit(options: Options): Promise<{ frames: ExitFrame[]; el: HTMLElement }> {
  view = render(configured(true, options));
  const el = wrap();
  // jsdom 不做布局：popLayout 按算好的样式量尺寸，量到 0 就不抽出布局；给一个尺寸，抽不抽出来才看得出
  el.style.width = "120px";
  el.style.height = "36px";
  view.rerender(configured(false, options));
  const frames: ExitFrame[] = [];
  while (el.isConnected) {
    if (frames.length > 60) throw new Error("退场一直没播完");
    frames.push({ popped: el.hasAttribute("data-motion-pop-id"), inert: el.hasAttribute("inert"), opacity: Number(el.style.opacity), transform: el.style.transform });
    await stepFrames(1);
  }
  return { frames, el };
}

it("对照：默认退场当场抽出布局，在原处淡出", async () => {
  const { frames } = await recordExit({});
  expect(frames.length).toBeGreaterThan(3);
  expect(frames.every((f) => f.popped)).toBe(true);
});

it("exitInLayout：撤掉的子项留在布局里淡完才移除，途中一直占着原位、带 inert，淡完那一刻移除", async () => {
  const { frames } = await recordExit({ exitInLayout: true });
  // fade.out 约 120 ms，七八帧
  expect(frames.length).toBeGreaterThan(3);
  expect(frames.length).toBeLessThanOrEqual(Math.ceil((fade.out.duration * 1000) / 16) + 2);
  expect(frames.filter((f) => f.popped)).toEqual([]);
  expect(frames.every((f) => f.inert)).toBe(true);
  // 一路淡下去：不透明度只降不升
  const opacities = frames.map((f) => f.opacity);
  expect(opacities).toEqual([...opacities].sort((a, b) => b - a));
  expect(opacities[0]).toBe(1);
  expect(view?.container.querySelector("[data-note]")).toBeNull();
});

it("exitInLayout 且减少动画：只淡出、不缩放，同样留在布局里，淡完即移除", async () => {
  const { frames } = await recordExit({ exitInLayout: true, reducedMotion: "always" });
  expect(frames.length).toBeGreaterThan(3);
  expect(frames.length).toBeLessThanOrEqual(Math.ceil((fade.out.duration * 1000) / 16) + 2);
  expect(frames.filter((f) => f.popped)).toEqual([]);
  expect(frames.map((f) => f.transform)).toEqual(frames.map(() => ""));
  const opacities = frames.map((f) => f.opacity);
  expect(opacities).toEqual([...opacities].sort((a, b) => b - a));
  expect(view?.container.querySelector("[data-note]")).toBeNull();
});

/** 最长连续几帧不透明度一点没动。 */
function longestStall(opacities: readonly number[]): number {
  let run = 0;
  let longest = 0;
  for (let i = 1; i < opacities.length; i++) {
    run = opacities[i] === opacities[i - 1] ? run + 1 : 0;
    longest = Math.max(longest, run);
  }
  return longest;
}

it("exitInLayout：退场途中被拉回，这一格一直没让出去，不再等 enterDelay，从半路的不透明度直接回到 1", async () => {
  const options: Options = { exitInLayout: true, enterDelay: 0.2, enterStart: "commit" };
  view = render(configured(false, options));
  view.rerender(configured(true, options));
  await stepFrames(60);
  const el = wrap();
  expect(el.style.opacity).toBe("1");

  view.rerender(configured(false, options));
  await stepFrames(3);
  const left = Number(el.style.opacity);
  expect(left).toBeGreaterThan(0);
  expect(left).toBeLessThan(1);

  view.rerender(configured(true, options));
  // 拉回的是退场中的那一格，不是重新插进来的一项
  expect(wrap()).toBe(el);
  expect(el.hasAttribute("inert")).toBe(false);
  const opacities = [left];
  while (opacities.at(-1) !== 1) {
    if (opacities.length > 60) throw new Error("拉回后一直没回到不透明");
    await stepFrames(1);
    opacities.push(Number(el.style.opacity));
  }
  expect(opacities).toEqual([...opacities].sort((a, b) => a - b));
  // 带着 200 ms 的延迟会停住十来帧
  expect(longestStall(opacities)).toBeLessThanOrEqual(1);
  expect(opacities.length - 1).toBeLessThanOrEqual(Math.ceil((fade.in.duration * 1000) / 16) + 1);
});

it("enterStart 为 commit：不等首帧画出，enterDelay 从插进来的那次提交起算，比默认的等首帧早起步", async () => {
  /** 插进来之后第几帧开始入场（不透明度离开 0）。 */
  const startFrame = async (enterStart: "painted" | "commit") => {
    const options: Options = { enterDelay: 0.1, enterStart };
    view = render(configured(false, options));
    view.rerender(configured(true, options));
    const el = wrap();
    for (let frame = 1; frame <= 30; frame++) {
      await stepFrames(1);
      if (Number(el.style.opacity) > 0) {
        view.unmount();
        view = undefined;
        await drainFrames();
        return frame;
      }
    }
    throw new Error(`enterStart ${enterStart} 的入场一直没起步`);
  };
  const commit = await startFrame("commit");
  const painted = await startFrame("painted");
  // 100 ms 约 6 帧：从提交起算，第 7 帧前后起步；等首帧的还要加上放行之前的那一两帧
  expect(commit).toBeGreaterThanOrEqual(Math.floor(100 / 16));
  expect(commit).toBeLessThanOrEqual(Math.ceil(100 / 16) + 1);
  expect(painted).toBeGreaterThan(commit);
});
