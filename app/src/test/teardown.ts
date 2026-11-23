// 每个用例结束后要做的组件收尾：test/dom 在加载时登记进来，test/setup.ts 在用例结束时调用。
// setup 不直接引用 test/dom：dom 一加载就带上 framer-motion，而 framer-motion 在模块加载时就取走 requestAnimationFrame。
// setup 先于测试文件加载，引用了 dom 就会抢在测试文件前面：要先换掉 requestAnimationFrame 再加载动画库的、要 mock 掉
// framer-motion 的，都会拿到不对的那一份。没加载过 test/dom 的测试文件没有经它渲染的组件，也就没有要收的。

let registered: (() => Promise<void>) | undefined;

export function registerRenderTeardown(teardown: () => Promise<void>): void {
  registered = teardown;
}

export async function runRenderTeardown(): Promise<void> {
  if (registered !== undefined) await registered();
}
