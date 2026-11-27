// 人偶皮肤：本机拿不到可用的皮肤图时（还没有任何原版材质、皮肤信息还在读又没有上一张可留、图被外部删掉、
// 正版或外置账户的 UUID 不合规）头像与立绘画它。它是一张 64 × 64 的标准布局皮肤，只在内存里按账户色相生成，
// 不落盘、不进素材清单；经与真皮肤相同的头像与立绘管线画出来，所以能转身，与真皮肤交叉淡化时轮廓不跳。
// 形象是原创的人偶：光头、两点眼，本体压到很低的饱和度读作中性；帽子与衣裤按账户色相取色，
// 头像只取正脸时也能靠帽子认出是哪个账户。日夜两种模式同色，与真皮肤一样不压暗。

import { blankPixels, type Pixels } from "../illustrations/canvas";
import { hslToRgb, type Rgb } from "../illustrations/color";
import { LruCache } from "../iso/render";
import { figureParts, type BoxFace, type BoxLayer, type FigurePartName } from "./figure3d";
import type { SkinModel } from "./layout";
import { skinBitmapFromPixels, type LoadedSkin } from "./render";

/** M 人偶本体、J 关节缝、E 眼、K 帽身、R 帽檐、C 上衣、T 领口袖口与下摆、P 裤、S 鞋。 */
export type MannequinCell = "M" | "J" | "E" | "K" | "R" | "C" | "T" | "P" | "S";

/** 各部分的 HSL 饱和度与明度（百分数），色相是账户色相。 */
export const MANNEQUIN_SHADES: Readonly<Record<MannequinCell, readonly [s: number, l: number]>> = {
  M: [8, 80],
  J: [8, 64],
  E: [20, 24],
  K: [44, 56],
  R: [40, 72],
  C: [42, 66],
  T: [36, 82],
  P: [30, 40],
  S: [16, 26],
};

/** 盒子某一面上 (col, row) 处画什么，null 为透明。col、row 从这一面贴图的左上角数。 */
type CellRule = (layer: BoxLayer, face: BoxFace, col: number, row: number) => MannequinCell | null;

/** 帽子在外层：顶面整块，四个侧面上两行帽身、第三行帽檐，再往下透明，露出人偶的额头与眼睛。 */
const headCell: CellRule = (layer, face, col, row) => {
  if (layer === "outer") {
    if (face === "top") return "K";
    if (face === "bottom") return null;
    return row < 2 ? "K" : row === 2 ? "R" : null;
  }
  return face === "front" && row === 4 && (col === 2 || col === 5) ? "E" : "M";
};

/** 上衣占前 8 行，第 9 行是下摆，再往下是裤腰；正面第一行的领口露出脖子。 */
const bodyCell: CellRule = (layer, face, col, row) => {
  if (layer === "outer") return null;
  if (face === "top") return "C";
  if (face === "bottom") return "P";
  if (face === "front" && row === 0) return col === 3 || col === 4 ? "M" : col === 2 || col === 5 ? "T" : "C";
  return row < 8 ? "C" : row === 8 ? "T" : "P";
};

/** 短袖加袖口，下面是人偶本体，肘部一道关节缝。 */
const armCell: CellRule = (layer, face, _col, row) => {
  if (layer === "outer") return null;
  if (face === "top") return "C";
  if (face === "bottom") return "M";
  return row < 3 ? "C" : row === 3 ? "T" : row === 7 ? "J" : "M";
};

const legCell: CellRule = (layer, face, _col, row) => {
  if (layer === "outer") return null;
  if (face === "top") return "P";
  if (face === "bottom") return "S";
  return row < 10 ? "P" : "S";
};

const RULES: Readonly<Record<FigurePartName, CellRule>> = {
  head: headCell,
  body: bodyCell,
  rightArm: armCell,
  leftArm: armCell,
  rightLeg: legCell,
  leftLeg: legCell,
};

function palette(hue: number): Record<MannequinCell, Rgb> {
  const out = {} as Record<MannequinCell, Rgb>;
  for (const cell of Object.keys(MANNEQUIN_SHADES) as MannequinCell[]) out[cell] = hslToRgb(hue, MANNEQUIN_SHADES[cell][0], MANNEQUIN_SHADES[cell][1]);
  return out;
}

/**
 * 人偶皮肤的像素，新格式 64 × 64。贴图位置取立绘几何里各部位各面的 UV，与立绘取片的规则是同一份，
 * 细体型的手臂窄一列，贴图位置随之不同。身体与四肢的外层全透明。
 */
export function mannequinPixels(hue: number, model: SkinModel): Pixels {
  const colors = palette(hue);
  const pixels = blankPixels(64, 64);
  for (const part of figureParts("modern", model, true)) {
    const rule = RULES[part.name];
    for (const box of part.boxes) {
      for (const face of Object.keys(box.uv) as BoxFace[]) {
        const { x, y, w, h } = box.uv[face];
        for (let row = 0; row < h; row++) {
          for (let col = 0; col < w; col++) {
            const cell = rule(box.layer, face, col, row);
            if (cell === null) continue;
            const i = ((y + row) * pixels.width + x + col) * 4;
            pixels.data.set(colors[cell], i);
            pixels.data[i + 3] = 255;
          }
        }
      }
    }
  }
  return pixels;
}

/**
 * 账户数加上离线账户弹窗里边敲名字边换过的色相，这么多张足够；每张人偶还挂着各尺寸的成图，数量要有上限。
 * 被挤出去的再要时重新生成，内容一样，只是换了对象。
 */
const mannequins = new LruCache<string, LoadedSkin>(64);

/** 人偶皮肤：帽子在外层，叠帽子层。同一色相与体型给同一个对象，成图按它缓存。 */
export function mannequinSkin(hue: number, model: SkinModel): LoadedSkin {
  const key = `${hue}|${model}`;
  const hit = mannequins.get(key);
  if (hit) return hit;
  const skin: LoadedSkin = { bitmap: skinBitmapFromPixels(mannequinPixels(hue, model)), format: "modern", hat: true, model };
  mannequins.set(key, skin);
  return skin;
}
