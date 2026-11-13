# 文件因果溯源台 · FileLineage

> 给每个文件回答三个问题：**它从哪来、经过谁的手、现在还有没有用。**

一个常驻后台的 Windows 桌面应用 + 一个「血缘视图」窗口。核心对象是**单个文件的生命周期**，不是文件夹，也不是磁盘空间。

设计原文见 [IDEA.md](./IDEA.md)。

**当前版本：V2.0.1（V2 采集层硬化 + 启动热修）。V2 只动后端，前端界面与 V1 完全一致；界面改版留给 V3。**

> **0.2.1 热修（重要）**：0.2.0 在**装过并运行过 V1** 的机器上会无法启动（双击无反应、无托盘、无提示）。原因见下方「升级与启动诊断」。如果你装过 0.2.0 且启动不了，直接覆盖安装 0.2.1 即可，原有数据会自动迁移，不会丢失。

---

## 版本边界

| | V1（已发布） | V2（本次） |
| --- | --- | --- |
| 平台 | Windows x64 | Windows x64 |
| 形态 | 手动启动的窗口应用 | **常驻后台**：托盘图标、关闭即最小化到托盘、开机自启、单实例 |
| Q1 来源 | 浏览器下载记录 + 路径特征 | + **`Zone.Identifier` 备用数据流**（与浏览器无关的真实来源 URL）+ IM 账号/月份目录解析 + 邮件文件类型 |
| Q2 进程 | Restart Manager 句柄归因 | + **ETW 内核事件流**（`Microsoft-Windows-Kernel-File`，真·写入 PID；需管理员）+ 增强版句柄探测 |
| Q2「经过谁的手」 | 只看写入 | + **Shell「最近使用」打开记录**（`opened` 事件） |
| 文件身份 | 路径 | + **卷号 + NTFS FileID**：重命名/移动关联、**硬链接**与副本的区分、路径被换文件检测 |
| 指纹 | 整文件 BLAKE3 | + **1 MiB 分块 BLAKE3**：部分相同/项目聚类，追加写入的增量复用 |
| 存储 | 单库 | + **90 天冷热分离**：按月归档到只读 `archive-YYYYMM.db`，详情查询自动合并 |
| 采集节流 | 去抖 + 背压 | + 写入静默期（settle）后才哈希、每分钟分块预算、ETW 仅内存索引 |
| 界面 | 三栏 | **不变**（新增数据已进入 API 与数据库，供 V3 呈现） |

V2 **明确不做**：标签、全文搜索、云同步、团队协作、AI 摘要、macOS / Linux 适配、血缘图 WebGL 化。这些与界面重构一起留给 V3。

数据全部留在本机：SQLite（`%LOCALAPPDATA%\FileLineage\lineage.db`），无任何网络上传，前端不引用任何外部 CDN。

### 配置

首次运行会生成 `%LOCALAPPDATA%\FileLineage\config.ini`，全部开关都在里面：

```ini
autostart = true                # 开机自启（写入 HKCU\...\Run）
minimize_to_tray = true         # 关闭主窗口 = 最小化到托盘
enable_etw = true               # 允许 ETW 通道（未提权时自动降级）
incremental_hash = true         # 文件只增长时复用已有分块指纹
archive_after_days = 90         # 事件转只读归档的天数
settle_seconds = 20             # 最后一次写入后静默多久才计算指纹
hash_chunks_per_minute = 9000   # 每分钟最多计算多少个 1 MiB 分块（背压）
```

托盘右键菜单：显示主窗口 / 重新扫描 / 开机自启（勾选）/ 退出。勾选状态会同步写回 `config.ini`。

---

## 架构

```
┌──────────────────────────────────────────────────────────────────────┐
│ React + TypeScript (Vite)  —— V2 未改动                               │
│   文件列表 │ 血缘图(Canvas) │ 三问详情 │ 时间线                        │
└─────────────────────────────── invoke ───────────────────────────────┘
┌──────────────────────────────────────────────────────────────────────┐
│ Tauri 2 / Rust                                               托盘 + 单实例 │
│  ┌──────────────┐   ┌─────────────────────┐   ┌───────────────────┐  │
│  │  etw.rs      │   │  attrib.rs          │   │  sources.rs       │  │
│  │ Kernel-File  │   │ Restart Manager     │   │ Zone.Identifier   │  │
│  │ 真写入 PID   │   │ 句柄→PID→exe        │   │ 浏览器下载库      │  │
│  │ (需管理员)   │   │ (无需提权)          │   │ im_layout.rs      │  │
│  └──────┬───────┘   └──────────┬──────────┘   └─────────┬─────────┘  │
│         │  path→PID 索引       │                        │            │
│         └──────────┬───────────┘                        │            │
│              ┌─────▼──────────────────────────────────▼─────┐        │
│              │ collector.rs  notify → 400ms 去抖 → 归因     │        │
│              │ recent.rs 打开记录 · identity.rs 文件身份     │        │
│              └─────┬──────────────────────────┬─────────────┘        │
│        ┌───────────▼──────────┐   ┌───────────▼──────────────────┐   │
│        │ fingerprint.rs       │   │ db.rs  SQLite WAL            │   │
│        │ 1 MiB 分块 BLAKE3    │──▶│ files / events / chunks      │   │
│        │ 增量复用 + 预算限速  │   │ + archive-YYYYMM.db 冷归档   │   │
│        └──────────────────────┘   └──────────────────────────────┘   │
└──────────────────────────────────────────────────────────────────────┘
```

### Q1 来源：证据强度从高到低

1. **`Zone.Identifier` 备用数据流** —— 浏览器或任何 shell 感知的下载器在文件上留下的隐藏流：

   ```text
   [ZoneTransfer]
   ZoneId=3
   ReferrerUrl=https://example.com/page
   HostUrl=https://cdn.example.com/file.zip
   ```

   它指向**真实的来源主机**，不依赖任何浏览器数据库，清理历史也仍在。这是 V2 里性价比最高的一条证据。
2. **浏览器下载记录** —— Chrome / Edge / Brave / Vivaldi / Chromium / Firefox。先把库复制到临时文件再只读打开（浏览器会锁库），按下落路径精确匹配，再按「文件名 + 大小」保守兜底。
3. **IM 接收目录布局** —— 目录结构本身就带信息：

   ```text
   Documents\xwechat_files\<账号>_<后缀>\msg\file\2026-03\report.pdf
   ```

   可以回答「哪个账号、哪个月份收到的」，不需要读客户端私有数据库。
4. **邮件文件类型**（`.msg` / `.eml` / `.pst` / `.ost`）。
5. **路径特征**（`Content.Outlook`、`FileRecv`、`Telegram Desktop`、Downloads/Desktop/Documents …）。

### Q2 进程：两条通道

**通道 A — ETW（真·写入 PID，需管理员）**

订阅清单提供程序 `Microsoft-Windows-Kernel-File`（GUID `EDD08927-…`），从事件头拿 `ProcessId`，这就是「哪个进程发起了这次写入」的权威答案。

有个不显然、但决定成败的细节：**`Write` 事件的载荷里根本没有文件名**，只有 `FileObject` / `FileKey`。路径来自 `NameCreate`（`FileKey`+`FileName`）和 `Create`（`FileObject`+`FileName`），所以 V2 在回调里维护一张有上限的 `FileObject/FileKey → (路径, 打开者 PID)` 关联表，在 `Cleanup`/`Close` 时清理；文件名是 `\Device\HarddiskVolumeN\…` 形式的 NT 路径，会用 `QueryDosDeviceW` 映射回 `C:\…`。缓存写由 System 进程（PID 4）刷盘时会回退到记录的打开者 PID。

关键词只开 `0x1EF0`（文件名 / create / write / delete / rename），刻意排除 READ —— 读事件占绝大多数流量且不是变更。

选择**清单提供程序**而不是经典的 NT Kernel Logger 会话，还有两个原因：NT Kernel Logger 是全局独占的（会和 Process Monitor、部分杀软、WPR 抢会话），而且它暴露的是 MOF 事件，在当前 Windows 构建上 TDH 无法稳定解码。清单提供程序用 `EnableTraceEx2` 打开，多个消费者可以共存。

回调只写内存索引（两个 map，各 16K 上限、30 秒过期、`try_parse` 全失败即静默丢弃），**不碰 SQLite、不落盘、不 unwrap** —— ferrisetw 的回调一旦 panic 会直接终止进程。只有文件系统监控真的报出某个路径发生变化时，才会去查这个索引，并且只认 15 秒内的新鲜记录。会话用固定名字启动，启动前/失败后会 `stop_trace_by_name` 清理，避免崩溃留下孤儿 ETW 会话。

**通道 B — Restart Manager（无需提权）**

`RmGetList` 返回当前持有该文件句柄的所有进程。关键点是**在 `notify` 的原始事件上探测**（250 ms 限频），而不是等 400 ms 去抖之后再问——写入方通常在事件洪流期间仍持有句柄。再用 `QueryFullProcessImageNameW` 把 PID 解析成完整 exe 路径。

两条通道产出同一个 `Attribution`，`attribution` 字段标注来源（`etw` / `restart-manager`），存储与界面不需要知道是谁答的。**ETW 是严格可选的**：未提权、被禁用、或启动失败，都会在 `Status.etw_state` 里写明原因，采集继续走通道 B。

> 诚实说明：ETW 需要提升权限才能启用，而本应用默认以普通用户身份常驻（开机自启时弹 UAC 是不可接受的）。因此**通道 A 只在「以管理员身份运行」时生效**。要做成开箱即用需要一个提权辅助进程 + 进程间通道，那会引入新的用户可见交互——按你的要求，界面与交互的改动留给 V3。

### Q2 的另一半：谁打开过它

「经过谁的手」不只是「谁改过」。Shell 每次打开文件都会在 `%APPDATA%\Microsoft\Windows\Recent` 放一个 `.lnk`。V2 读取该目录，按文件名匹配已跟踪的文件并写入 `opened` 事件（不增加 `change_count`，因为打开不是修改）。

快捷方式的名字是目标文件的显示名，同名文件可能来自别处，所以这条信号在文案里被明确标为**推断**，而不是硬证据。

### 文件身份：路径不是身份

`CreateFileW` + `GetFileInformationByHandle` 拿到 `(卷序列号, NTFS 文件索引)`，等价于 inode。三个直接收益：

* 重命名/移动不再被当成「新文件」；
* 相同 `FileID` 的两个路径是**硬链接**，不是副本 —— 删掉一个既不释放空间也不丢内容，这是与「有副本」完全不同的结论；
* 同一个路径上 `FileID` 变了，就是**文件被换掉了**，时间线会明确标注。

### 指纹：分块 BLAKE3

每个 1 MiB 分块存一条摘要，文件的内容根 = 对所有分块摘要再做一次 BLAKE3。相比整文件哈希，多出来的能力是**部分相同**：共享 ≥2 个分块的其它文件会被列为「关联文件（可能是同一项目）」，而不是副本。

**增量**这一块我把话说清楚：BLAKE3 的哈希器状态不可持久化，所以「增量」指的是一个更窄但诚实的东西——文件**只增长**时，先重读并重算**最后一个已存分块**做校验，通过后只哈希新增的尾部；校验失败就退回全量。下载完成、日志追加、文档多存几页这些常见情形因此变便宜，同时不会给出错误答案；原地改写的文件会校验失败并全量重算。

另外：文件变化后先进入 `settling`，要等 `settle_seconds` 秒没有新写入才进入哈希队列，避免给下载中的半成品反复算哈希；哈希线程还有每分钟分块预算，超了就退让。

### 冷热分离

超过 `archive_after_days` 的事件按月搬进 `%LOCALAPPDATA%\FileLineage\archive\archive-YYYYMM.db`（只读打开），主库删除这些行并做一次 WAL checkpoint。查阅某个文件的详情时，实时事件与所有归档库的事件会合并、排序、截断后返回，所以老文件的时间线不会因为归档而消失。

---

## 升级与启动诊断（0.2.1）

### 0.2.0 的启动故障

在**装过并运行过 V1** 的机器上，0.2.0 双击后完全没反应：没有窗口、没有托盘、没有错误提示。

原因链条：

1. V1 的 `files` 表只有 15 列，没有 `volume_serial` / `file_index` 等 V2 列；
2. 0.2.0 启动时把「建表 + 建索引」放在同一段 `execute_batch` 里，其中
   `CREATE INDEX idx_files_identity ON files(volume_serial, file_index)` 引用了还不存在的列；
3. 对 V1 数据库，`CREATE TABLE IF NOT EXISTS` 是空操作，列要等后面的 `migrate()` 才补 —— 顺序反了，SQLite 报 `no such column: volume_serial`；
4. 调用方是 `.expect("failed to open the lineage database")`，而发布版是
   `windows_subsystem = "windows"` 的 GUI 程序，没有控制台，panic 直接静默退出。

### 0.2.1 的修法

- schema 拆成两段：**先只建表 → `migrate()` 补列 → 再建索引**。第 1 阶段绝不引用任何可能被迁移补充的列，第 3 阶段执行时列一定存在。
- 启动不再用 `expect`：数据库打开失败、Tauri 应用/托盘构建失败，都会写
  `%LOCALAPPDATA%\FileLineage\startup.log` 并弹出**可见的错误框**（含错误详情和日志路径）。
- 安装 `panic hook`，任何启动期 panic 也会先落到 `startup.log`（超过 512 KiB 自动轮换）。
- 关键节点都有日志：`config loaded` / `database opened / migration ok` / `setup ok / tray built / collector started` / `tauri app built / entering event loop` / `second instance detected`。

所以再遇到启动问题，第一件事是看 `startup.log`——它会直接告诉你卡在哪一步、报什么错。

### 升级行为

覆盖安装不会删除数据。首次启动 0.2.1 会对 V1 库执行：

- 幂等补列（`hash_reason` / `volume_serial` / `file_index` / `chunk_count` / `opened_ts`），已存在的列会跳过；
- 把 V1 的整文件哈希全部置空并重新入队 —— V2 的内容根是「对分块摘要再哈希」，与 V1 的整文件 BLAKE3 数值不同，必须重算才能比较；
- 补建 `chunks` 表和全部索引。

对全新安装则是一次性建好全部表与索引。

---

## 构建安装包（只走 GitHub Actions）

**本机不构建安装包。** 本机只负责写代码、提交、推送；编译与 NSIS 打包全部在 `windows-latest` runner 上完成。

1. 打开仓库 **Actions → Build Windows Installer → Run workflow**。
   - 建议勾选 `create_release`：安装包会同时发布到 **Releases**，得到永久下载链接（Release 资源不占用 Actions Artifact 存储配额）。
2. 等约 10–25 分钟（首次要编译 Rust 依赖）。
3. 下载位置二选一：
   - **Releases** → `FileLineage_0.2.1_x64-setup.exe`（推荐，长期保留）；
   - 该次 run 的 **Artifacts** → `FileLineage-Windows-x64`（保留 30 天，若账户 Artifact 配额耗尽该步会被跳过，但不影响构建结果）。
4. 推送 `v*` tag 会自动触发构建并自动发布 Release。

工作流见 [.github/workflows/build.yml](./.github/workflows/build.yml)。产物为 NSIS 单用户安装（`currentUser`），无需代码签名即可本机安装；Windows SmartScreen 对未签名新应用可能给出提示，选择「仍要运行」即可。

安装后应用会常驻托盘并注册开机自启。想拿到完整进程归因，右键以管理员身份运行一次即可（`Status.etw_state` 在 V3 会显示在界面上；V2 可通过 `%LOCALAPPDATA%\FileLineage\lineage.db` 的 `events.attribution` 列确认）。

---

## 本地开发

需要 Node 20+、Rust stable (MSVC)、Visual Studio Build Tools + Windows SDK、WebView2 Runtime。

```bash
npm ci
npm run tauri:dev      # 启动 dev（前端 Vite 1420 + Tauri 窗口）
npm run build          # 仅前端类型检查 + 打包，不产出安装包
npm run tauri:build    # 本机全量打包（本项目的正式交付不走这条路）
```

前端在普通浏览器里 `npm run dev` 也能打开，此时 `api.ts` 会返回一组演示数据用于检查界面（不会连后端）。

图标由 [scripts/make_icons.py](./scripts/make_icons.py) 生成（需要 Pillow）：

```bash
python scripts/make_icons.py
```

---

## 目录

```
.github/workflows/build.yml   GitHub Actions：唯一的安装包构建入口
src/                         React + TypeScript 前端（V2 未改动）
src-tauri/                   Tauri 2 + Rust 采集 daemon
  src/collector.rs           监控、去抖、扫描、settle、维护循环
  src/etw.rs                 ETW 通道（Microsoft-Windows-Kernel-File）+ 路径索引
  src/attrib.rs              进程归因两条通道的公共类型 + Restart Manager
  src/identity.rs            卷号 + FileID 文件身份
  src/fingerprint.rs         1 MiB 分块 BLAKE3 + 增量复用
  src/sources.rs             来源解析（Zone/浏览器/IM/邮件/路径）
  src/zoneid.rs              Zone.Identifier 备用数据流
  src/im_layout.rs           IM 接收目录布局解析
  src/recent.rs              Shell「最近使用」打开记录
  src/db.rs                  SQLite schema/迁移、查询、三问解释引擎、血缘图、归档
  src/config.rs              config.ini
  src/runtime.rs             提权检测、单实例、开机自启、窗口唤起
  src/win32.rs               手写原生 FFI（kernel32 / user32 / advapi32 / rstrtmgr）
  src/lib.rs                 Tauri 壳：托盘、菜单、关闭到托盘、命令
scripts/make_icons.py        图标生成脚本
IDEA.md                      产品方向与 MVP 边界（原始需求）
```

---

## 已知限制（V2）

- **ETW 需要管理员权限**，默认常驻进程拿不到，因此大多数时候实际生效的是通道 B（句柄近似）。开箱即用的提权方案属于交互改动，留给 V3。
- ETW 通道在本机**无法实机验证**（构建机没有可用的 Windows Rust 工具链，也没有提权测试环境）；它通过了编译与逐行 API 核对，失败路径一律降级而不会中断采集，但真实覆盖率需要你在管理员身份下实测。
- 「唯一副本」只覆盖**已监控的三个目录**；其它磁盘位置或云端的副本不在此结论范围内。
- 部分相同判定基于分块边界。文件中间插入内容会移动后续所有分块边界，此时「部分相同」会退化（这是内容定义分块能解决的问题，属于后续版本）。
- `Recent` 的打开记录按文件名匹配，同名文件可能误判，因此只作为推断信号。
- 历史数据是「安装之后才开始积累」的；安装前的文件只有一次 `observed` 基线登记。
- 归档库按 `file_id` 查询，如果将来加入文件记录清理，需要同时处理归档库的对应关系。
- 前端顶部侧栏的说明文案仍是 V1 的口径（按你的要求 V2 不动界面），V3 一并更新。

## V3 计划（等你的界面需求）

界面重构，并把 V2 已经落库/入 API 的能力呈现出来：`etw_state` / `elevated` / `autostart` / `archived_events` / `related`（部分相同）/ `hardlink` / `opened_ts` / `chunk_count` / `incremental_hashes` / `config_path`。另外可以在 V3 一起做：一键提权辅助进程（让 ETW 开箱即用）、血缘图 WebGL 化（5000 节点）、内容定义分块（CDC）。
