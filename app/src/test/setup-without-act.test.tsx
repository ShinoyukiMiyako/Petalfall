// @vitest-environment jsdom
// 关掉 act 环境的整页测试经 test/dom 的 renderWithoutAct 挂载，同样由 test/setup.ts 的全局收尾卸载：用例故意不收尾就结束，
// 后一个用例检查它留下的组件与容器都收走了，挂载与收尾都没有调 act。act 环境是全局开关，与 setup.test.tsx 分开放，这里整份关着。
// 同一文件里的用例按书写顺序依次跑，后面的用例依赖前一个。
import { useEffect } from "react";
import { afterEach, beforeEach, describe, expect, it, vi, type MockInstance } from "vitest";
import { renderWithoutAct } from "./dom";

const unmounted: string[] = [];
/** 文件自己的 afterEach 跑的那一刻（全局收尾已跑完）：body 里还剩几个渲染容器，这个用例连同全局收尾里 console.error 收到了什么。 */
const seenByOwnTeardown: { containers: number; errors: unknown[][] }[] = [];
let consoleError: MockInstance<typeof console.error>;
let given: HTMLDivElement | undefined;

beforeEach(() => {
  // act 环境关着时调 act，React 经 console.error 报测试环境不支持 act
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
  consoleError = vi.spyOn(console, "error");
});

afterEach(() => {
  seenByOwnTeardown.push({ containers: document.body.childElementCount, errors: [...consoleError.mock.calls] });
});

function Probe({ id }: { id: string }) {
  useEffect(
    () => () => {
      unmounted.push(id);
    },
    [id],
  );
  return <span data-testid={id} />;
}

describe("不经 act 挂载的组件也由全局收尾卸载", () => {
  it("没给容器：新建一个挂进 body，渲染出来之后不卸载就结束", async () => {
    const view = renderWithoutAct(<Probe id="own" />);
    expect(view.container.parentElement).toBe(document.body);
    await vi.waitFor(() => expect(view.container.querySelector('[data-testid="own"]')).not.toBeNull());
  });

  it("上一个用例留下的组件在文件自己的 afterEach 之前已卸载、容器已移除，挂载与收尾都没有报 act 的错", () => {
    expect(unmounted).toEqual(["own"]);
    expect(seenByOwnTeardown).toEqual([{ containers: 0, errors: [] }]);
  });

  it("给了容器：挂在它上面，渲染出来之后不卸载就结束", async () => {
    const host = document.createElement("div");
    document.body.append(host);
    given = host;
    const view = renderWithoutAct(<Probe id="given" />, host);
    expect(view.container).toBe(host);
    await vi.waitFor(() => expect(host.querySelector('[data-testid="given"]')).not.toBeNull());
  });

  it("给的容器随组件一起卸载、从 body 里移除", () => {
    expect(unmounted).toEqual(["own", "given"]);
    expect(given?.isConnected).toBe(false);
    expect(seenByOwnTeardown.slice(2)).toEqual([{ containers: 0, errors: [] }]);
  });
});
