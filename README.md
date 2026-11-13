# 文件因果溯源台 · FileLineage

> 给每个文件回答三个问题：**它从哪来、经过谁的手、现在还有没有用。**

一个常驻后台的 Windows 桌面应用 + 一个「血缘视图」窗口。核心对象是**单个文件的生命周期**，不是文件夹，也不是磁盘空间。

设计原文见 [IDEA.md](./IDEA.md)。

---

## V1 边界（本仓库当前状态）

按 IDEA.md 的 MVP 约定，V1 **只做一个平台、只监控三个目录、只回答三个问题**：

| 维度 | V1 |
| --- | --- |
| 平台 | Windows x64（ETW 归因风险最低，CI 可直接出安装包） |
| 监控目录 | `%USERPROFILE%\Downloads`、`Desktop`、`Documents`（含常见 OneDrive 重定向） |
| Q1 从哪来 | 浏览器下载记录 / 邮件附件 / IM 接收 / 本地生成 / 未知 |
| Q2 谁改过 | 进程名 + PID + 时间线 |
| Q3 删了会不会丢 | BLAKE3 内容指纹 → 相同内容的其他副本 + 可删除性判定 |

**明确不做**（留给 V2，见文末）：标签、全文搜索、云同步、团队协作、AI 摘要、开机自启 / 托盘常驻、ETW 内核事件流、macOS / Linux 适配。

数据全部留在本机：SQLite（`%LOCALAPPDATA%\FileLineage\lineage.db`），无任何网络上传，前端不引用任何外部 CDN。

---

## 架构

```
┌──────────────────────────────────────────────────────────────┐
│ React + TypeScript (Vite)                                    │
│   文件列表 │ 血缘图(Canvas) │ 三问详情 │ 时间线              │
└─────────────────────────── invoke ───────────────────────────┘
┌──────────────────────────────────────────────────────────────┐
│ Tauri 2 / Rust                                               │
│  ┌────────────┐  ┌──────────────┐  ┌───────────────────────┐ │
│  │ collector  │  │  attrib.rs   │  │     sources.rs        │ │
│  │ notify     │→ │ Restart Mgr  │  │ Chrome/Edge/Brave     │ │
│  │ 400ms 去抖 │  │ 句柄 → PID   │  │ Vivaldi/Chromium      │ │
│  │ 重命名配对 │  │ → exe 路径   │  │ Firefox + 路径特征    │ │
│  └─────┬──────┘  └──────────────┘  └───────────────────────┘ │
│        │            ┌──────────────────────────────────────┐ │
│        └───────────→│ db.rs  SQLite WAL                    │ │
│                     │ files / events + 索引                │ │
│                     └──────────────▲───────────────────────┘ │
│                     ┌──────────────┴───────────────────────┐ │
│                     │ fingerprint.rs  BLAKE3 (1 MiB 分块)  │ │
│                     │ 后台低优先级队列 + 512 MB 上限        │ │
│                     └──────────────────────────────────────┘ │
└──────────────────────────────────────────────────────────────┘
```

### 三个问题是怎么回答的

**Q1 来源** — 分两级证据：
1. **强证据**：只读快照打开浏览器 `History` / `places.sqlite`（先复制到临时文件，避免浏览器锁库），按下落路径精确匹配，再按「文件名 + 大小」保守兜底（覆盖下载后移动的情况）。
2. **路径特征**：`Content.Outlook` → 邮件附件；`WeChat Files` / `xwechat_files` / `FileRecv` / `WXWork` / `Feishu` / `DingTalk` / `Telegram Desktop` → IM；Downloads / Desktop / Documents → 本地或下载。
3. 若捕获到写句柄，再用进程名升级判定（浏览器 → 下载、Outlook → 邮件、IM 客户端 → IM、其它 → 本机生成）。

**Q2 进程** — V1 使用 **Windows Restart Manager**（`rstrtmgr.dll`）做非提权归因：给定路径，返回当前持有该文件句柄的进程。关键点是在 `notify` 的**原始事件**上立即探测（250 ms 限频），而不是等 400 ms 去抖之后再问——写入方通常在事件洪流期间仍持有句柄，写入结束后就问不到了。再用 `QueryFullProcessImageNameW` 把 PID 解析成完整 exe 路径。所有 FFI 都手工声明、失败即降级为「未知进程」，绝不阻塞采集。

> 诚实说明：重启管理器是「持有句柄」归因，不是「发起写入」归因。IDEA.md 里真正的进程级归因需要 ETW `Microsoft-Windows-Kernel-File` 内核会话（需管理员权限、需 `ferrisetw`/krabs 或等效实现）。V1 先用一个无需提权、可立即上线、覆盖率尚可的近似解把产品闭环跑通；`Attribution` 类型刻意与实现解耦，V2 可直接替换为 ETW 订阅者，存储与 UI 无需改动。

**Q3 副本与可删除性** — 后台队列按文件从小到大算 BLAKE3（1 MiB 分块读，>512 MB 标记 `too_large` 而非阻塞采集）。`size`/`mtime` 变化会让指纹失效并重新入队。相同指纹的路径即为内容副本；唯一副本且判定为下载产物时，会额外提示「原始来源可重新获取」。

其它工程细节：事件去重与 400 ms 合并、重命名 `[old, new]` 成对处理以维持短期身份、初始扫描按 400 条一批走事务、跳过 `node_modules`/`.git` 等噪声目录、跳过 `~$`/`.crdownload` 等临时文件、单次扫描 20 万文件硬上限、pending 队列 8000 条背压。

---

## 构建安装包（只走 GitHub Actions）

**本机不构建安装包。** 本机只负责写代码、提交、推送；编译与 NSIS 打包全部在 `windows-latest` runner 上完成。

1. 打开仓库 **Actions → Build Windows Installer → Run workflow**。
2. 等约 10–20 分钟（首次要编译 Rust 依赖）。
3. 在该次 run 的 **Artifacts** 里下载 `FileLineage-Windows-x64`，得到 `FileLineage_0.1.0_x64-setup.exe`。
4. 推送 `v*` tag，或在手动运行时勾选 `create_release`，会把安装包一并发布到 GitHub Release。

工作流见 [.github/workflows/build.yml](./.github/workflows/build.yml)。产物为 NSIS 单用户安装（`currentUser`），无需代码签名即可本机安装；Windows SmartScreen 对未签名新应用可能给出提示，选择「仍要运行」即可。

---

## 本地开发

需要 Node 20+、Rust stable (MSVC)、Visual Studio Build Tools + Windows 10/11 SDK、WebView2 Runtime。

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
src/                         React + TypeScript 前端
  api.ts                     Tauri invoke 封装 + 浏览器演示数据
  components/LineageGraph.tsx  Canvas 力导向血缘图（无外部图库依赖）
  App.tsx / styles.css       三栏主界面
src-tauri/                   Tauri 2 + Rust 采集 daemon
  src/collector.rs           监控、去抖、扫描、后台哈希
  src/attrib.rs              Restart Manager 进程归因
  src/sources.rs             浏览器下载记录 + 路径特征适配器
  src/fingerprint.rs         BLAKE3 分块指纹
  src/db.rs                  SQLite schema、查询、三问解释引擎、血缘图构造
  icons/                     Windows 安装包图标
scripts/make_icons.py        图标生成脚本
IDEA.md                      产品方向与 MVP 边界（原始需求）
```

---

## 已知限制（V1）

- 进程归因是句柄近似，短命写入（例如一次性复制小文件）可能抓不到，UI 会诚实显示「尚未捕获到写入进程」。
- 「唯一副本」只覆盖**已监控的三个目录**；其它磁盘位置或云端的副本不在此结论范围内。
- 来源解析读的是浏览器本地 DB 的只读快照；若浏览器使用非标准 Profile 路径或已清理历史记录，会退化为路径特征判定。
- 历史数据是「安装之后才开始积累」的，安装前的文件只有一次 `observed` 基线登记，不代表创建时间。

## V2 路线（待确认后再开工）

ETW 内核事件流（真·进程级归因，管理员权限可选降级）· inode/卷号短期身份 · 增量 BLAKE3 · 事件按月分区 + 90 天冷热归档 · 更多来源适配器（Outlook 本地库、IM 缓存内容解析）· 开机自启与托盘常驻 · 血缘图 WebGL 化（5000 节点）· macOS / Linux。
