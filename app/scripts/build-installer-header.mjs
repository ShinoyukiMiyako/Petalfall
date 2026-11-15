// NSIS 向导页眉图：从 src/art/petal-mark.json 与 src/styles/tokens.css 生成 src-tauri/installer-header.bmp，产物提交进仓库，
// 改了母版或日樱底色就跑 pnpm build:icons。卸载向导与缺 WebView2 时回退到的安装向导都显示它。
//
// Tauri 的 NSIS 模板只开 MUI_HEADERIMAGE，没开 MUI_HEADERIMAGE_RIGHT，拉伸方式取 MUI2 默认的 FitControl：
// 图片控件在页眉最左侧，96 DPI、默认字体下是 150 x 57，标题从它右边约 8 像素处开始；图片总被拉伸到控件大小。
// 页眉其余部分是 MUI_BGCOLOR 的默认白，所以图的右段渐变到白，控件右缘看不出接缝。

import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { renderMark } from "./build-icons.mjs";

const SCRIPT_DIR = dirname(fileURLToPath(import.meta.url));
const MARK_PATH = join(SCRIPT_DIR, "..", "src", "art", "petal-mark.json");
const TOKENS_PATH = join(SCRIPT_DIR, "..", "src", "styles", "tokens.css");
const OUTPUT_PATH = join(SCRIPT_DIR, "..", "src-tauri", "installer-header.bmp");

const WIDTH = 150;
const HEIGHT = 57;

// 4 倍的 64 像素放不进 57 的高度，3 倍是放得下的最大整数倍。底色条带也按这个格宽切，整张图落在同一套像素格上。
const CELL = 3;
const MARK_SIDE = 16 * CELL;
const MARK_TOP = Math.floor((HEIGHT - MARK_SIDE) / 2);
const MARK_LEFT = 3 * CELL;

// 从左到右逐级变浅，宽度按格数计。第一条的宽度让花瓣实际占用的格子左右各留 15 像素；最后剩下的是页眉白。
const BANDS = [
  ["--sk-bg-3", 23],
  ["--sk-bg-2", 8],
  ["--sk-bg", 6],
];
// MUI2 的 MUI_BGCOLOR 默认值，Tauri 模板没有改它；它是 NSIS 的页眉底色，不是品牌令牌
const NSIS_HEADER_BG = [0xff, 0xff, 0xff];

const RGB_BYTES = 3;
const RGBA_BYTES = 4;
const HEX_COLOR = /^#[0-9A-Fa-f]{6}$/;

const BMP_FILE_HEADER_BYTES = 14;
const BMP_INFO_HEADER_BYTES = 40;
const BMP_BIT_COUNT = 24;
const BMP_BI_RGB = 0;
// 150 x 57 是按 96 DPI 的对话框单位算出来的建议尺寸
const BMP_PELS_PER_METER = 3780;

/** tokens.css 第一个 :root 块是日樱的基础令牌，只认写成 #RRGGBB 的值。 */
function dayTokenColor(tokensCss, name) {
  const block = tokensCss.replaceAll("\r\n", "\n").match(/^:root \{\n([\s\S]*?)^\}/m);
  if (block === null) throw new Error("tokens.css 里没有 :root 块");
  const declaration = block[1].match(new RegExp(`^\\s*${name}:\\s*([^;]+);`, "m"));
  if (declaration === null) throw new Error(`tokens.css 的 :root 块里没有 ${name}`);
  const value = declaration[1].trim();
  if (!HEX_COLOR.test(value)) throw new Error(`${name} 的值「${value}」不是 #RRGGBB，页眉图只能用写死的色值`);
  return [...Buffer.from(value.slice(1), "hex")];
}

/** 按行自上而下排的 RGB 像素。 */
function renderHeader(mark, tokensCss) {
  const rgb = Buffer.alloc(WIDTH * HEIGHT * RGB_BYTES);
  const columns = [];
  for (const [token, cells] of BANDS) {
    const color = dayTokenColor(tokensCss, token);
    for (let i = 0; i < cells * CELL; i++) columns.push(color);
  }
  if (columns.length > WIDTH) throw new Error(`底色条带共 ${columns.length} 像素，超出页眉图的 ${WIDTH} 像素宽`);
  while (columns.length < WIDTH) columns.push(NSIS_HEADER_BG);
  for (let y = 0; y < HEIGHT; y++) {
    for (let x = 0; x < WIDTH; x++) rgb.set(columns[x], (y * WIDTH + x) * RGB_BYTES);
  }

  const petal = renderMark(mark.grid16, mark.palette, MARK_SIDE);
  for (let y = 0; y < MARK_SIDE; y++) {
    for (let x = 0; x < MARK_SIDE; x++) {
      const at = (y * MARK_SIDE + x) * RGBA_BYTES;
      // renderMark 的空格是全透明，其余全不透明，没有半透明要混合
      if (petal[at + 3] === 0) continue;
      rgb.set(petal.subarray(at, at + RGB_BYTES), ((MARK_TOP + y) * WIDTH + MARK_LEFT + x) * RGB_BYTES);
    }
  }
  return rgb;
}

// 24 位不带 alpha：页眉图放在静态图片控件里，控件不做 alpha 混合。高度写正数、自下而上存，和 NSIS 自带的页眉图同一种排法。
function encodeBmp(rgb) {
  const stride = Math.ceil((WIDTH * RGB_BYTES) / 4) * 4;
  const pixelOffset = BMP_FILE_HEADER_BYTES + BMP_INFO_HEADER_BYTES;
  const bmp = Buffer.alloc(pixelOffset + stride * HEIGHT);
  bmp.write("BM", 0, "latin1");
  bmp.writeUInt32LE(bmp.length, 2);
  bmp.writeUInt32LE(pixelOffset, 10);
  bmp.writeUInt32LE(BMP_INFO_HEADER_BYTES, 14);
  bmp.writeInt32LE(WIDTH, 18);
  bmp.writeInt32LE(HEIGHT, 22);
  bmp.writeUInt16LE(1, 26);
  bmp.writeUInt16LE(BMP_BIT_COUNT, 28);
  bmp.writeUInt32LE(BMP_BI_RGB, 30);
  bmp.writeUInt32LE(stride * HEIGHT, 34);
  bmp.writeInt32LE(BMP_PELS_PER_METER, 38);
  bmp.writeInt32LE(BMP_PELS_PER_METER, 42);
  for (let y = 0; y < HEIGHT; y++) {
    const row = pixelOffset + (HEIGHT - 1 - y) * stride;
    for (let x = 0; x < WIDTH; x++) {
      const at = (y * WIDTH + x) * RGB_BYTES;
      bmp[row + x * RGB_BYTES] = rgb[at + 2];
      bmp[row + x * RGB_BYTES + 1] = rgb[at + 1];
      bmp[row + x * RGB_BYTES + 2] = rgb[at];
    }
  }
  return bmp;
}

/** 由母版与令牌文件的文本算出页眉图的 BMP 字节。 */
export function buildInstallerHeader(mark, tokensCss) {
  return encodeBmp(renderHeader(mark, tokensCss));
}

function main() {
  const mark = JSON.parse(readFileSync(MARK_PATH, "utf8"));
  writeFileSync(OUTPUT_PATH, buildInstallerHeader(mark, readFileSync(TOKENS_PATH, "utf8")));
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) main();
