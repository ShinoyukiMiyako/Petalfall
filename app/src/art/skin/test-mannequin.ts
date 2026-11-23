// 组件与页面测试用（只被 *.test.tsx 引用）：头像或立绘的回落层上贴的是哪张图。
// 回落层是 ArtCrossfade 的第一层；成图按皮肤对象缓存，期望的那一张从同一份缓存取出，按对象比对。

import { accountHue } from "../illustrations/seed";
import type { CanvasStub } from "../illustrations/test-canvas";
import type { SkinModel } from "./layout";
import { mannequinSkin } from "./mannequin";
import { skinFaceCanvas, skinFigureCanvas } from "./render";

/** 回落层（root 本身或其中第一层）的显示画布上最后一次贴上去的图；画布不存在或什么都没画时抛错，不当成空图。 */
export function fallbackDrawn(stub: CanvasStub, root: Element): unknown {
  const layer = root.matches('[class*="layer"]') ? root : root.querySelector('[class*="layer"]');
  const canvas = layer?.querySelector("canvas");
  if (!canvas) throw new Error("回落层里没有画布");
  const draws = stub.draws.filter((d) => d.canvas === canvas);
  const last = draws.at(-1);
  if (last === undefined) throw new Error("回落层的画布上什么都没画");
  return last.source;
}

/** 这个账户的人偶头像成图，dpr 取 jsdom 的 1。 */
export function mannequinFace(uuid: string, face: number): OffscreenCanvas {
  return skinFaceCanvas(mannequinSkin(accountHue(uuid), "wide"), face, 1);
}

/** 这个账户的人偶立绘在默认姿态下的成图，dpr 取 jsdom 的 1。 */
export function mannequinFigure(uuid: string, model: SkinModel, scale: number): OffscreenCanvas {
  return skinFigureCanvas(mannequinSkin(accountHue(uuid), model), scale, 1);
}
