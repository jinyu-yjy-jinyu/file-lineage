# 文件因果溯源台 · FileLineage

> 给每个文件回答三个问题：**它从哪来、经过谁的手、现在还有没有用。**

一个常驻后台的桌面应用 + 一个「血缘视图」窗口，支持 **Windows / macOS / Linux**。核心对象是**单个文件的生命周期**，不是文件夹，也不是磁盘空间。

设计原文见 [IDEA.md](./IDEA.md)。

**当前版本：0.3.0。** 三条主线：跨平台可编译可发行、Q1+Q2+Q3 综合结论与可信度、dark-saas 界面重构。

> **0.3.0**：三平台发行（NSIS / .app + .dmg / .deb + AppImage）；详情页在三张问答卡之后新增**最终结论卡**，可信度由证据强度推导（不是写死）；界面改为 dark-saas（Linear 式）视觉；发布流水线改为「三平台矩阵 + 单一 release job」，并新增版本一致性守卫。
>
> **0.2.2**：第三方下载器任务记录（Ghost Downloader）来源适配器，可追溯安装前已下载的文件。

---

## 版本边界

| | 0.2.2 | 0.3.0（本次） |
| --- | --- | --- |
| 平台 | Windows x64 | **Windows x64 / macOS universal / Linux x64** |
| 界面 | 三栏（冷色） | 三栏 + 综合结论卡，dark-saas token 体系 |
| Q1 可信度 | 按 `source_kind` 猜（下载目录=高） | **按证据通道强度分级**：`zone.ads` / `downloader.task` = 3，`rm.handle` / IM 布局 = 2，路径特征 = 1 |
| Q2 进程 | ETW + Restart Manager | 保留；**非 Windows 如实降级**并写进结论的保留意见 |
| Q3 删除后果 | 「唯一副本」只看是否命中哈希 | **必须满足覆盖率条件**（无未完成指纹 + 文件在监控目录内），否则降级 |
| 重命名 | 依赖 `RenameMode::Both`（Windows 后端从不发） | **按 FileID / inode 跨路径重连**，三平台一致 |
| 身份 | Windows NTFS FileID | Windows FileID + Unix `(st_dev, st_ino)` |
| 结论 | 三张卡各说各的 | **Q1+Q2+Q3 → headline / confidence / score / drivers / caveats / action** |
| 发布 | 单平台单 job 直接发 Release | 矩阵构建 → 单一 release job；版本守卫 + 产物存在性断言 |

数据全部留在本机：SQLite（Windows `%LOCALAPPDATA%\FileLineage\`，macOS `~/Library/Application Support/FileLineage/`，Linux `$XDG_DATA_HOME/FileLineage/`），无任何网络上传，前端不引用任何外部 CDN。

### 各平台的数据与系统集成

| 能力 | Windows | macOS | Linux |
| --- | --- | --- | --- |
| 数据目录 | `%LOCALAPPDATA%\FileLineage` | `~/Library/Application Support/FileLineage` | `$XDG_DATA_HOME/FileLineage`（默认 `~/.local/share`） |
| 开机自启 | `HKCU\...\Run` | `~/Library/LaunchAgents/io.github.filelineage.desktop.plist` | `~/.config/autostart/file-lineage.desktop` |
| 单实例 | 命名互斥体 | `instance.lock` + pid 存活检查 | `instance.lock` + `/proc` 存活检查 |
| 文件管理器定位 | Explorer `/select,"<path>"` | `open -R` | `xdg-open <目录>` |
| 进程归因 | ETW（需管理员）+ Restart Manager | 无等价通道 | 无等价通道 |
| 打开记录 | Shell「最近使用」`.lnk` | 不提供 | 不提供 |

界面上 `Status.etwState` 会明确写出 `unsupported: 本平台没有 ETW…`，综合结论也会把「Q2 证据上限为中」放进保留意见——不会假装有进程级归因。

### 配置

首次运行会在数据目录生成 `config.ini`：

```ini
autostart = true                # 开机自启（平台各自的登录项）
minimize_to_tray = true         # 关闭主窗口 = 最小化到托盘
enable_etw = true               # 允许 ETW 通道（未提权/非 Windows 自动降级）
incremental_hash = true         # 文件只增长时复用已有分块指纹
archive_after_days = 90         # 事件转只读归档的天数
settle_seconds = 20             # 最后一次写入后静默多久才计算指纹
hash_chunks_per_minute = 9000   # 每分钟最多计算多少个 1 MiB 分块（背压）
```

托盘右键菜单：显示主窗口 / 重新扫描 / 开机自启（勾选）/ 退出。勾选状态以**系统登录项真值**为准；切换失败或 `config.ini` 写不进去时会回滚勾选并把原因写进 `Status.lastError`。

---

## 架构

```
┌──────────────────────────────────────────────────────────────────────┐
│ React + TypeScript (Vite)                                             │
│   文件列表 │ 血缘图(Canvas) │ Q1/Q2/Q3 │ 最终结论卡 │ 时间线           │
└─────────────────────────────── invoke ───────────────────────────────┘
┌──────────────────────────────────────────────────────────────────────┐
│ Tauri 2 / Rust                                  托盘 + 单实例 + 自启 │
│  platform.rs  数据目录 / 监控目录 / 登录项 / 提权 / 单实例 / reveal     │
│  ┌──────────────┐   ┌─────────────────────┐   ┌───────────────────┐  │
│  │  etw.rs      │   │  attrib.rs          │   │  sources.rs       │  │
│  │ Kernel-File  │   │ Restart Manager     │   │ Zone.Identifier   │  │
│  │ (仅 Windows) │   │ (仅 Windows)        │   │ 浏览器下载库      │  │
│  └──────┬───────┘   └──────────┬──────────┘   └─────────┬─────────┘  │
│         └──────────┬───────────┘                        │            │
│              ┌─────▼──────────────────────────────────▼─────┐        │
│              │ collector.rs  notify → 400ms 去抖 → 归因     │        │
│              │ recent.rs / identity.rs / rename reconnect   │        │
│              └─────┬──────────────────────────┬─────────────┘        │
│        ┌───────────▼──────────┐   ┌───────────▼──────────────────┐   │
│        │ fingerprint.rs       │   │ db.rs  SQLite WAL            │   │
│        │ 1 MiB 分块 BLAKE3    │──▶│ files / events / chunks      │   │
│        │ 增量复用 + 预算限速  │   │ + archive-YYYYMM.db          │   │
│        └──────────────────────┘   └───────────┬──────────────────┘   │
│                                  ┌────────────▼──────────────┐       │
│                                  │ verdict.rs 纯函数打分      │       │
│                                  │ Q1+Q2+Q3 → 结论 + 可信度   │       │
│                                  └────────────────────────────┘       │
└──────────────────────────────────────────────────────────────────────┘
```

### Q1 来源：证据强度

每条来源结论都带 `source_evidence` 与 `source_strength`，它们决定了 Q1 与最终结论可信度的上限：

| 强度 | 证据 id | 说明 |
| --- | --- | --- |
| 3 | `zone.ads` | `Zone.Identifier` 备用数据流：真实来源主机，与浏览器无关 |
| 3 | `downloader.task` | Ghost Downloader 等任务日志：工具 + URL + 时间，可追溯安装前文件 |
| 3 | `browser.exact` | 浏览器下载记录**精确路径**命中 |
| 2 | `browser.name_size` | 浏览器下载记录按「文件名 + 大小」保守匹配 |
| 2 | `im.layout` | IM 接收目录布局（账号 + 接收月份） |
| 2 | `rm.handle` | Restart Manager 写句柄：**只证明谁写，不证明从哪来** |
| 1 | `path.shape` / `mail.ext` | 路径特征、扩展名：纯启发式 |
| 0 | `none` | 无证据 |

`\Downloads\` 路径本身最高只能给到 **low**；由写句柄升级的结论最高只能给到 **medium**，文案明确写「由 X 写入，来源未确认」。这样「可信度」才不是装饰。

### Q2 进程：两条通道与平台差异

- **ETW**（Windows，需管理员）：订阅 `Microsoft-Windows-Kernel-File`，从事件头拿真实写入 PID。`Write` 事件没有路径，靠 `FileObject`/`FileKey → (path, opener PID)` 关联表补齐；缓存写（PID 4）回落到打开者 PID。
- **Restart Manager**（Windows，无需提权）：在原始 watcher 事件上探测写句柄；`RmGetList` 因列表增长返回 `ERROR_MORE_DATA` 时会重试，不再偶发「无归因」。
- **macOS / Linux**：没有等价通道，`etw_state` 明确为 `unsupported`，Q2 上限为 medium。

「谁打开过它」来自 Shell 最近使用记录（仅 Windows），按文件名匹配，因此明确标注为**推断**，时间线上有 `推断` 徽章。

### Q3 删除后果：覆盖率是前提

「唯一副本」不再等于「没查到副本」：

1. 指纹未完成（`pending/settling/error/too_large`）→ 低置信 + `verify_first`；
2. 存在内容相同的独立副本 → 高置信；
3. 只有硬链接 → 删除不释放空间但也不丢内容；
4. 没有副本时，必须同时满足「库内无未完成指纹」且「文件在监控目录内」才给高置信，否则带保留意见降级；
5. 部分相同的关联文件**永不**算作副本。

### 最终结论与可信度

`verdict.rs` 是纯函数，输入是三条通道的证据摘要，输出：

```text
headline / detail / confidence(high|medium|low) / score(5..=97)
drivers[] / caveats[] / action(delete_safe|keep|verify_first)
```

打分（可解释、可单测）：

```text
base: high=90 medium=65 low=35 unknown=15
score = 0.35*base(Q1) + 0.25*base(Q2) + 0.40*base(Q3)
        - 8*未知通道数(≤3) - 10*冲突 - 5*ETW 不可用 - 8*Q3 覆盖率不足
clamp(5, 97)
confidence = score>=78 高；50..77 中；<50 低
语义封顶：Q3 未就绪 ⇒ ≤55；非 Windows ⇒ ≤77
```

冲突矩阵（摘要）：写入者已知但来源未知 → 「写入者已确定，来源仍未知」；下载目录 + 浏览器写句柄 → 「很可能是浏览器下载产物，但缺 URL 证据」；Q3 未就绪 → 「暂时无法判断删除后果」；三问全无证据 → 「证据不足，暂不给出结论」。

前端只负责展示标签与计量条，**不参与打分**，也不拼接三张卡的文字。

### 文件身份与重命名

`(卷号, FileID)`（Windows）或 `(st_dev, st_ino)`（Unix）是身份。重命名/移动的关联不再依赖 `RenameMode::Both`（Windows 后端只发 `From`/`To`，macOS 只发 `Name(Any)` 且只有新路径）：watcher 发现新路径时，用文件身份去匹配一条 `liveness='gone'` 的同身份记录，存在则**改路径而不是插新行**，事件、分块指纹和来源证据都跟着走。

### 指纹与冷热分离

每个 1 MiB 分块存一条 BLAKE3 摘要，内容根 = 对分块摘要再做 BLAKE3；文件只增长时先校验最后一块再复用前缀，校验失败退回全量。变化后先进入 `settling`，静默 `settle_seconds` 秒后才入队；哈希线程有每分钟分块预算。

超过 `archive_after_days` 的事件按月搬进只读 `archive-YYYYMM.db`，主库删除并 checkpoint。`DETACH` 失败会记录错误并重建连接——不再静默停摆。

---

## macOS / Linux 使用注意

- **macOS 首次运行需授权**：系统设置 → 隐私与安全性 → 文件与文件夹（或「完全磁盘访问权限」），允许访问「桌面 / 下载 / 文档」。未授权时 FSEvents 静默无事件，界面会显示「0 个目录可监控」。
- macOS 未签名发行版首次打开需在「隐私与安全性」里放行（或右键 → 打开）。发布说明会写明。
- Linux 的 AppImage 需要 `chmod +x`；部分发行版还需 `libfuse2`。托盘依赖 `libayatana-appindicator3`。
- **ETW 仅 Windows 且需要管理员权限**。默认常驻进程是普通权限，因此大多数时候实际生效的是 Restart Manager；macOS/Linux 完全没有进程级归因，这是平台事实，不是 bug。

---

## 构建与发布（只走 GitHub Actions）

本机只负责写代码、提交、推送；编译与打包全部在 runner 上完成。

工作流 `.github/workflows/build.yml`：

1. `version-guard`：`scripts/check-version.mjs` 断言 `package.json` / `Cargo.toml` / `tauri.conf.json` 版本一致；`v*` tag 必须等于 `v<version>`。
2. `build`（matrix，`fail-fast: false`）：Windows NSIS、macOS universal（`.app` + `.dmg`）、Linux（`.deb` + `.AppImage`）。每个平台先 `npm run build` + `cargo check`，再打包，最后断言产物真实存在。
3. `release`（单一 job）：`download-artifact merge-multiple` 后一次性发布，避免三个 job 并发写同一个 tag。

推荐流程：

```bash
# 1. 先验证三平台能编译（不打包、不发布）
gh workflow run build.yml -f check_only=true

# 2. 全绿后，推 tag 触发正式发布
git tag v0.3.0 && git push origin v0.3.0
```

发布产物文件名：

- `FileLineage_0.3.0_x64-setup.exe`（NSIS，单用户安装）
- `FileLineage_0.3.0_universal.dmg` / `FileLineage.app`
- `FileLineage_0.3.0_amd64.deb` / `FileLineage_0.3.0_amd64.AppImage`

Release 名称由版本守卫输出派生：`FileLineage v0.3.0`，release notes 自动生成。

---

## 本地开发

需要 Node 20+、Rust stable 1.90+、各平台的原生构建依赖：

- Windows：Visual Studio Build Tools + Windows SDK、WebView2 Runtime；
- macOS：Xcode Command Line Tools；
- Linux：`libwebkit2gtk-4.1-dev build-essential curl wget file libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev`。

```bash
npm ci
npm run tauri:dev      # 前端 Vite 1420 + Tauri 窗口
npm run build          # 仅前端类型检查 + 打包（本地唯一编译门禁）
npm run tauri:build    # 本机全量打包（正式交付不走这条路）
```

前端在普通浏览器里 `npm run dev` 也能打开：`api.ts` 的 `isTauri()` 返回 false 时使用 demo 数据渲染整页，可用于检查界面。

图标（需要 Pillow）：

```bash
python scripts/make_icons.py
```

该脚本生成 1024×1024 源图与 Windows/Linux 图标；macOS 的 `icon.icns` 由本机已安装的 Tauri CLI 生成一次并提交：

```bash
npx tauri icon src-tauri/icons/icon.png -o /tmp/fl-icons
cp /tmp/fl-icons/icon.icns src-tauri/icons/
```

---

## 目录

```
.github/workflows/build.yml   GitHub Actions：三平台矩阵 + 单一 release
scripts/check-version.mjs     版本一致性守卫（CI 使用）
scripts/make_icons.py         图标生成脚本
src/                          React + TypeScript 前端
  App.tsx                     三栏界面、筛选/分页、最终结论卡
  theme.ts                    画布与图例共用的配色 token（唯一真源）
  api.ts                      Tauri invoke + 浏览器 demo 数据
src-tauri/                    Tauri 2 + Rust 采集 daemon
  src/platform.rs             跨平台：数据目录/监控目录/登录项/提权/单实例/reveal
  src/collector.rs            监控、去抖、扫描、settle、维护循环
  src/etw.rs                  ETW 通道（仅 Windows）+ 非 Windows 空实现
  src/attrib.rs               进程归因（仅 Windows 有真实实现）
  src/identity.rs             FileID / inode 文件身份
  src/fingerprint.rs          1 MiB 分块 BLAKE3 + 增量复用
  src/sources.rs              来源解析（Zone/浏览器/下载器/IM/路径）+ 证据强度
  src/zoneid.rs               Zone.Identifier 备用数据流
  src/downloaders.rs          第三方下载器任务日志解析器
  src/im_layout.rs            IM 接收目录布局解析
  src/recent.rs               Shell「最近使用」打开记录（仅 Windows）
  src/db.rs                   schema/迁移、查询、Q1-Q3 解释、归档
  src/verdict.rs              Q1+Q2+Q3 综合结论打分（纯函数 + 单测）
  src/config.rs               config.ini
  src/runtime.rs              platform 的兼容门面（保持调用点不变）
  src/win32.rs                手写 Win32 FFI（仅 Windows 编译）
  src/lib.rs                  Tauri 壳：托盘、菜单、关闭到托盘、命令
```

---

## 已知限制（0.3.0）

- **ETW 需要管理员权限**，默认常驻进程拿不到，因此大多数时候实际生效的是 Restart Manager；macOS/Linux 没有等价通道，Q2 上限为 medium。
- 「唯一副本」只覆盖**已监控的三个目录**；其它磁盘位置、云端副本、未完成指纹的文件都可能改变结论（未完成时会降级并提示）。
- 部分相同判定基于固定 1 MiB 分块边界；中间插入内容会退化（内容定义分块留待后续）。
- `Recent` 的打开记录按文件名匹配，同名文件可能误判，因此只作为推断信号，界面有徽章。
- 下载器适配器目前只接了 Ghost Downloader；Motrix / IDM / FDM / aria2 留待按需添加。
- 归档库按 `file_id` 查询；如果将来加入文件记录清理，需要同时处理归档库的对应关系。
- macOS 未签名发行版需要手动放行；菜单栏托盘图标暂用彩色图标，深色菜单栏下的观感可能与系统模板图有差异。
- 本机（无 MSVC 链接器）无法验证 Rust 编译与三平台打包，这些必须由 CI 验证；唯一本地门禁是 `npm run build`。
