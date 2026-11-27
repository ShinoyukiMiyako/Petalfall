import { describe, expect, it } from "vitest";
import { runnableLoaders } from "./runnable-loaders";

describe("实例能跑哪些加载器的 Mod", () => {
  it("Quilt 再借 Fabric，自己的在前；换哪个 MC 版本都只借 Fabric", () => {
    expect(runnableLoaders("quilt", "1.21.1")).toEqual(["quilt", "fabric"]);
    expect(runnableLoaders("quilt", "1.20.1")).toEqual(["quilt", "fabric"]);
    expect(runnableLoaders("quilt", null)).toEqual(["quilt", "fabric"]);
  });

  it("NeoForge 只在 1.20.1 上再借 Forge；1.20.2 起、MC 版本认不出时不借", () => {
    expect(runnableLoaders("neoforge", "1.20.1")).toEqual(["neoforge", "forge"]);
    expect(runnableLoaders("neoforge", "1.20.2")).toEqual(["neoforge"]);
    expect(runnableLoaders("neoforge", "1.21.1")).toEqual(["neoforge"]);
    expect(runnableLoaders("neoforge", null)).toEqual(["neoforge"]);
  });

  it("Forge、Fabric 只跑自己的，1.20.1 的 Forge 也不反借 NeoForge；原版一家都跑不了", () => {
    expect(runnableLoaders("forge", "1.20.1")).toEqual(["forge"]);
    expect(runnableLoaders("fabric", "1.20.1")).toEqual(["fabric"]);
    expect(runnableLoaders("fabric", null)).toEqual(["fabric"]);
    expect(runnableLoaders("vanilla", "1.20.1")).toEqual([]);
    expect(runnableLoaders("vanilla", null)).toEqual([]);
  });
});
