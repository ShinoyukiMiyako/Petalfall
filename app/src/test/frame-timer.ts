// 用例结束、换回真实计时器之后检查 jsdom 的动画帧计时器还在不在，test/setup.ts 在每个用例结束时调用。
//
// jsdom 30 的 requestAnimationFrame（browser/Window.js）只在排着的回调从 0 个变成 1 个时现取全局的 setInterval 起计时器，
// 减回 0 个才停。卸下假时钟时还有回调排在它上面，计时器跟着假时钟没了，记着的回调个数却停在大于 0：之后再排的回调不再起计时器，
// 一个也不会跑。framer-motion 在模块加载时就取走了 jsdom 自己的 requestAnimationFrame，于是同文件后面的用例动画一帧都不走，
// 挂在离出事处很远的动画断言上。这里在丢了计时器的那个用例结束时就报出来。
// 不引用 test/dom：理由同 test/teardown，setup 加载时不能带上 framer-motion。

import { vi } from "vitest";

// setup 先于测试文件加载，这时窗口上还是 jsdom 自己的那一个：测试里替换的、假时钟装上的都不是它
const jsdomRequestFrame = typeof window === "undefined" ? undefined : window.requestAnimationFrame;
const jsdomCancelFrame = typeof window === "undefined" ? undefined : window.cancelAnimationFrame;

/** 已有回调排着时，等它在真实计时器上到点的上限：远长于一帧的 16 ms，机器满载时也等得到。 */
const PENDING_FRAME_TIMEOUT_MS = 1_000;
const POLL_MS = 16;

/**
 * 经 jsdom 自己的 requestAnimationFrame 排一个回调，看它起没起计时器。起了说明之前没有回调排着，随即撤掉；
 * 没起说明已有回调排着，等这一帧在真实计时器上到点，等不到就是计时器在假时钟切换时丢了，直接抛错。
 * 要在真实计时器下调用：假时钟上排着的帧不推进就不会到点，分不出是丢了还是没推。
 */
export async function assertAnimationFrameTimerAlive(): Promise<void> {
  if (jsdomRequestFrame === undefined || jsdomCancelFrame === undefined) return;
  if (vi.isFakeTimers()) throw new Error("assertAnimationFrameTimerAlive 要在真实计时器下用：假时钟上排着的帧不推进就不会到点");
  const setIntervalBefore = globalThis.setInterval;
  let started = false;
  globalThis.setInterval = new Proxy(setIntervalBefore, {
    apply(target, self, args) {
      started = true;
      return Reflect.apply(target, self, args);
    },
  });
  let ran = false;
  let handle: number;
  try {
    handle = jsdomRequestFrame.call(window, () => {
      ran = true;
    });
  } finally {
    globalThis.setInterval = setIntervalBefore;
  }
  if (started) {
    jsdomCancelFrame.call(window, handle);
    return;
  }
  const deadline = performance.now() + PENDING_FRAME_TIMEOUT_MS;
  while (!ran && performance.now() < deadline) await new Promise((resolve) => setTimeout(resolve, POLL_MS));
  if (ran) return;
  jsdomCancelFrame.call(window, handle);
  throw new Error(
    "jsdom 的动画帧计时器在假时钟切换时丢了：卸下假时钟时还有动画帧回调排在它上面，计时器跟着假时钟没了，jsdom 记着的回调个数却没归零，" +
      "之后经它排的帧（framer-motion 的帧循环）一帧都不会来。丢在同一文件里最先报这条错的用例里（之后的用例接着报是同一个卡死状态）：" +
      "先看那个用例结束时 test/setup 收尾里的 drainFrames（test/dom）为什么没排空；用例中途换过计时器的，看换之前自己调的那次 drainFrames。",
  );
}
