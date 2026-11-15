// vitest.config.ts 的 setupFiles：每个用例结束后的通用收尾，先卸载组件、排空帧循环，最后换回真实计时器。
// 帧排在假时钟上时直接换计时器，帧循环就卡在没人推的旧时钟上，同文件后面的用例一帧都走不动（见 test/dom 的 drainFrames）。
import { afterEach, beforeEach, vi } from "vitest";
import { runRenderTeardown } from "./teardown";

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
