// 实例能跑哪些加载器的 Mod。与后端 crates/petalfall-core/src/compat.rs 的 runnable_loaders 是同一条规则：后端据它判落位、
// 下推版本过滤，前端据它下推搜索、判版本行能不能装进实例。改一边必须同步改另一边，否则搜得到的 Mod 判成装不上，
// 或判成装得上的 Mod 在搜索里被平台先筛掉。

import type { LoaderChoice } from "../ipc/instances";
import type { LoaderKind } from "./types";

/** NeoForge 还能加载 Forge Mod 的唯一一代 MC 版本：NeoForge 从 Forge 47.1 分出来，1.20.1 上与同版本的 Forge 二进制兼容，从 20.2 起两边分道。 */
const NEOFORGE_RUNS_FORGE_ON = "1.20.1";

/**
 * 装着这个加载器、基于这个 MC 版本的实例能跑哪些加载器的 Mod，自己装的在前、借来的在后。
 * 平台上大量 Mod 只标写它时针对的那一家，只认标签会把能跑的判成不能跑：
 * - Quilt 再借 Fabric：Quilt 能加载 Fabric Mod，大量 Fabric Mod 从不标 quilt。
 * - 1.20.1 的 NeoForge 再借 Forge：大量 1.20.1 的 Mod 只标 forge。MC 版本认不出（null）或不是 1.20.1 时不借，
 *   别的版本的 NeoForge 加载不了 Forge 的 jar。
 * 后端看实例装着的全部加载器，被借的那一家实例自己装着时不算借；实例摘要只带按 NeoForge > Forge > Fabric > Quilt 取的
 * 主加载器，主加载器是 Quilt 即没装 Fabric，所以只凭它就能判。
 */
export function runnableLoaders(loader: LoaderKind, mcVersion: string | null): LoaderChoice[] {
  switch (loader) {
    case "vanilla":
      return [];
    case "quilt":
      return ["quilt", "fabric"];
    case "neoforge":
      return mcVersion === NEOFORGE_RUNS_FORGE_ON ? ["neoforge", "forge"] : ["neoforge"];
    case "fabric":
    case "forge":
      return [loader];
  }
}
