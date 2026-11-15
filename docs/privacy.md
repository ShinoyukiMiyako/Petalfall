# 落樱隐私说明

版本：1

这份说明写的是落樱（Petalfall）官方发布的程序会联网访问哪些地址、在你的电脑上保存什么，以及你可以怎样删除这些数据。说明由落樱的维护者 Shinoyuki_Miyako 制定，联系方式见第 6 条。

## 1. 总体情况

- **落樱没有遥测、统计、崩溃报告上传或日志上传，不会把你的账户、日志、崩溃报告或使用情况发送给维护者。**
- 落樱联网时，你的电脑直接连接第 2 条列出的服务器。和访问任何网站一样，这些服务器会看到你的 IP 地址、请求的地址和 User-Agent；第三方的服务器按它们各自的隐私政策处理这些信息。落樱的请求里 User-Agent 写的是 `Petalfall/<版本号>`；检查和下载启动器更新的请求，大多用的是更新组件自己的 `tauri-plugin-updater/<版本号>`。
- 落樱不查询你的 IP 归属地或地理位置，也不按地区开关功能。
- 检查和下载启动器更新时访问的 `petalfall.org` 与几个下载用的存储空间由维护者管理，它们同样会看到上面这些网络信息，见第 2 条的「启动器更新」。除此之外，落樱不向维护者发送任何信息。

## 2. 联网访问的地址

### 账户

- 微软账户：`login.microsoftonline.com`（登录与续期）、`user.auth.xboxlive.com` 与 `xsts.auth.xboxlive.com`（Xbox Live 认证）、`api.minecraftservices.com`（换取 Minecraft 令牌、读取玩家档案）。登录时落樱只申请 XboxLive.signin 与 offline_access 两项权限。
- 当前账户是微软账户时，落樱每 15 分钟看一次它的 Minecraft 令牌，30 分钟内要过期就在后台提前续期，这时会访问上面这几个地址。
- 外置登录账户：你填写的验证服务器。用这类账户启动游戏前，落樱会从 `authlib-injector.yushi.moe` 或 BMCLAPI 镜像查询并下载最新的 authlib-injector。
- 账户皮肤：从账户档案里记着的皮肤地址下载。微软账户的皮肤在 Mojang 的服务器上，外置登录账户的皮肤在验证服务器给出的地址上。

### 游戏、Java 与加载器

- Mojang：`piston-meta.mojang.com`、`piston-data.mojang.com`、`launcher.mojang.com`、`launchermeta.mojang.com`、`libraries.minecraft.net`、`resources.download.minecraft.net`，用于版本清单、游戏文件、库、资源与 Java 运行时。
- 加载器：`meta.fabricmc.net`、`maven.fabricmc.net`、`meta.quiltmc.org`、`maven.minecraftforge.net`、`maven.neoforged.net`，以及加载器自己的配置里写着的库地址。
- BMCLAPI 社区镜像：`bmclapi2.bangbang93.com`，以及它转去的分发节点。什么时候用官方服务器、什么时候用镜像，见「使用须知」第 7 条。
- 下载源选「自动」时，下载之前落樱会向官方服务器与镜像各发一个只取 1 个字节的请求，比一比哪边快。有请求因为连不上而失败时，以及断网期间，落樱会向它们发 HEAD 请求，判断网络通了没有。
- 本机没有原版游戏时，你可以手动下载原版材质：落樱从 Mojang 或镜像下载一份游戏客户端和中文语言文件，只留下界面要用的图案与名称。

### Mod 与整合包

- `api.modrinth.com`、`api.curseforge.com`：搜索、浏览与安装。你输入的搜索词，以及游戏版本、加载器这类筛选条件，会作为查询条件发给它们。访问 CurseForge 时带着落樱的 API 密钥。
- 打开装了加载器的实例时，落樱会检查其中的 Mod 有没有更新；导出整合包时，会查哪些 Mod 能在 Modrinth 上找到。这时落樱把 Mod 文件的哈希值（SHA-1，或 CurseForge 用的文件指纹）连同实例的游戏版本与加载器发给 Modrinth 与 CurseForge，查出它们是哪个 Mod、有没有更新。发出去的是哈希值，不是文件本身。
- 文件与图片：`cdn.modrinth.com`、`edge.forgecdn.net`、`mediafilez.forgecdn.net`、`media.forgecdn.net`。导入本地的 Modrinth 整合包时，还可能访问包里写着的 `github.com`、`raw.githubusercontent.com`、`gitlab.com` 上的文件。

### 主页

- 资讯：打开主页时，从 `launchercontent.mojang.com` 读取 Mojang 的官方资讯、Java 版更新日志与资讯配图，6 小时内用上次取到的缓存；从 `api.github.com` 读取落樱在 GitHub 上的发布记录，1 小时内用缓存。这一项目前没有关闭开关。
- 服务器状态：只查你在主页固定的服务器。主页开着且联网时，大约每分钟查一次在线人数与延迟。

### 启动器更新

- 检查更新：「自动检查更新」开着时（默认开），落樱在打开约 8 秒后检查一次，之后每 12 小时检查一次；首次启动向导期间不检查。在「设置 · 关于」里手动检查、改选更新通道之后，也会检查。
- 检查时读取 `petalfall.org` 上所选更新通道和比它更稳的通道的清单，地址形如 `https://petalfall.org/updates/<通道>/latest.json`；预览版还会读取 GitHub 上滚动预发布 `dev-build` 里的同一份清单。请求地址里不带版本号、系统或设备信息，版本比较在你的电脑上完成。
- 下载更新：只从清单列出的这几个地方下载：`petalfall-dl-cn.oss-cn-hangzhou.aliyuncs.com` 与 `petalfall-dl.oss-cn-hongkong.aliyuncs.com`（阿里云对象存储）、`dl.petalfall.org`（Cloudflare R2），以及 GitHub 上本仓库的 Releases。
- `petalfall.org` 与这三个存储空间由维护者管理。

### 其他

- 游戏和 Mod 启动之后自己的联网，例如加入服务器、Mod 自己检查更新，由它们自己决定，不经过落樱。
- 「问题反馈」等外部链接用系统浏览器打开，落樱不附带任何数据。
- 落樱的界面由 Microsoft Edge WebView2 运行时显示，它是微软的组件。安装时这台电脑上还没有 WebView2 的话，安装程序会运行微软的 WebView2 安装程序，从微软下载。

## 3. 在你电脑上保存的数据

**这些数据都只保存在你的电脑上，落樱不会上传它们。**

数据目录是落樱程序所在的文件夹（能写入时），否则是 `%LOCALAPPDATA%\Petalfall`。按默认位置安装时，两者是同一个文件夹。数据目录里有：

- 账户：`credentials.bin`。微软账户的 refresh token 与 Minecraft 访问令牌、外置登录账户的访问令牌，连同玩家名、UUID、皮肤地址、Xbox 用户 ID、验证服务器地址、外置登录时填写的用户名或邮箱，以及添加与上次验证的时间，都在这里。整个文件用 Windows 的 DPAPI 按当前 Windows 用户加密，拷到别的电脑或换一个 Windows 用户都解不开。微软账户与外置登录的密码都不保存。
- 离线账户：`offline_accounts.json`，只有玩家名与添加时间，不加密。
- 设置：`config.json`，包括游戏目录的位置、下载与 Java 设置、固定的服务器、更新通道等。
- 游玩记录：`playtime.json`，按实例记游玩时长与上次游玩时间；每天的游玩时长只留最近 400 天。
- 任务记录：`tasks.json`，下载、安装等任务的列表。
- 启动器日志：`logs` 文件夹，每次打开落樱写一份，只留最近 10 份。日志里有本机路径、实例名与玩家名，不写访问令牌。
- 缓存：`cache` 文件夹，包括账户皮肤（不再属于任何账户的，在下次打开落樱时删掉）、Mod 与资讯的图片（30 天没用过的删掉，总量不超过 256 MiB）、资讯，以及从游戏文件里读出的原版图案。
- 托管的 Java 运行时（`runtime`）、authlib-injector（`authlib-injector`）、新实例的默认设置（`templates`），以及窗口位置、Java 识别结果这类缓存文件。

游戏目录里：

- 每个实例的 `.petalfall` 文件夹：会话日志只留最近 5 局，记着游戏的全部输出，其中的访问令牌已经遮挡；崩溃报告 `last-crash.json` 只留最近一次。
- 存档备份：游戏目录的 `.petalfall\backups`，按你在设置里定的份数与总量保留。

其他位置：

- 界面的 WebView2 数据：`%LOCALAPPDATA%\org.petalfall.launcher`。
- 安装程序的日志：`%LOCALAPPDATA%\Petalfall-Setup\setup.log`，每次安装往后追加，不会自动清理。

游戏崩溃时，Java 自己写在游戏目录里的 `hs_err_pid` 文件可能记着整条启动命令，其中有 Minecraft 访问令牌。这份文件不是落樱写的；落樱显示和分析它时会遮挡令牌。**把日志或崩溃文件发给别人之前，请先检查里面有没有访问令牌、玩家名和本机路径。**

## 4. 删除数据与收回授权

- 在落樱的「账户」页删除账户，会从这台电脑上删掉它的令牌。这一步不会通知微软或验证服务器作废令牌。
- 要收回落樱对你微软账户的访问权限，可以在微软账户的[应用和服务权限页](https://account.live.com/consent/Manage)里删除落樱。
- 卸载落樱时勾选「删除应用程序数据」（默认不勾），会删除数据目录里落樱写的文件，包括 `credentials.bin`、设置、日志与缓存，以及界面的 WebView2 数据。游戏目录（存档、Mod、各实例的会话日志与存档备份）和安装程序的日志不会删除，需要的话请自己删。
- 也可以在关掉落樱之后，直接删除数据目录。

## 5. 未成年人

除了第 1 条所说的网络信息，落樱不向维护者发送个人信息，也不专门收集未成年人的个人信息。以后如果加入需要发送个人信息的功能，会先更新这份说明；对未满 14 周岁的用户，会先取得监护人的同意。

## 6. 说明的更新与联系方式

处理的信息种类、目的或方式有变化时，这份说明会更新，文首的版本号加一；需要重新征得你同意的，会在落樱里先告诉你。

关于这份说明或你的个人信息的问题：一般问题在 [GitHub 仓库](https://github.com/ShinoyukiMiyako/Petalfall)的 Issues 页提交；涉及个人信息、不便公开的，用仓库的私密漏洞报告（仓库 Security 页的 Report a vulnerability），内容只有你和维护者看得到。
