// vitest.config.ts 的 setupFiles：每个用例结束后的通用收尾，先卸载组件、排空帧循环，最后换回真实计时器；另给 jsdom 补上空闲回调。
// 帧排在假时钟上时直接换计时器，帧循环就卡在没人推的旧时钟上，同文件后面的用例一帧都走不动（见 test/dom 的 drainFrames）。
import { afterEach, beforeEach, vi } from "vitest";
import { runRenderTeardown } from "./teardown";

// jsdom 没有空闲回调；WebView2 有，产品代码按锁定的类型定义直接用。DOM 环境里补一个排在当前任务之后的版本，
// 现取全局的 setTimeout、clearTimeout，假定时器下跟着假时钟走。补之前先让 vitest 建好假时钟（它在第一次用到时
// 才记下全局上有哪些计时器可假）：vitest 4.1.12 内置的 @sinonjs/fake-timers 会假 requestIdleCallback，却因为探测 cancelIdleCallback
// 时写错了键名不假它，两个都被它记下的话，产品代码撤空闲回调时假时钟的 clearTimeout 认出类型不对直接抛错
vi.isFakeTimers();
if (typeof window !== "undefined" && typeof window.requestIdleCallback !== "function") {
  window.requestIdleCallback = (callback) => window.setTimeout(() => callback({ didTimeout: false, timeRemaining: () => 0 }), 0);
  window.cancelIdleCallback = (handle) => window.clearTimeout(handle);
}

// 要赶在测试文件自己换回真实计时器之前：vitest.config.ts 把 sequence.hooks 设成 list，这里登记的 afterEach
// 先于测试文件顶层的 afterEach 跑。describe 里的 afterEach 仍在它之前，在那里换计时器的要自己先卸载、再排空。
afterEach(async () => {
  await runRenderTeardown();
});

// 换回真实计时器放在最后，等文件自己的收尾都跑完：有的收尾还要在假时钟上推进，有的要先撤掉自己替换的
// requestAnimationFrame 再卸下假时钟。用例中途失败、没走到自己那句 useRealTimers 的，假时钟也不会带进下一个用例。
beforeEach(({ onTestFinished }) => {
  onTestFinished(() => {
    vi.useRealTimers();
  });
});
