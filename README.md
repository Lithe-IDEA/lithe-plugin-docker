# Docker for Lithe

在 Lithe 的工具窗口中管理本机 Docker Engine，使用 Web UI 和 Docker 图标。
目前支持 macOS（Apple silicon 与 Intel）。Windows 的插件 Web 桥接尚不支持后端函数，
此插件也尚未实现 Windows 命名管道；不能安装为 Windows 插件使用。

## 本地安装

[下载最新版 macOS 安装包](https://github.com/Lithe-IDEA/lithe-plugin-docker/releases/latest)
（Apple silicon / Intel 通用；发布页附 SHA-256 校验文件）。

在 Lithe 的「设置 → 插件 → 从磁盘安装」中选择构建产物：

```text
dist/docker-0.1.0-darwin.zip
```

也可以选择解压后的 `dist/io.github.lithe-idea.docker/` 目录。
**不要选择源码仓库根目录**：源码中的 `node_modules/.bin` 包含符号链接，
Lithe 会以「插件包不能包含别名、符号链接或特殊文件」拒绝导入。
安装包只包含清单、已编译后端、页面、Docker 图标与许可证。

本插件是未附带 Lithe 发布者签名的社区插件；安装时由宿主显示权限与程序确认。
发布记录以 GitHub Releases 为准；本地构建或 GitHub 发布不代表已上架 Lithe Store。

## 能力

- 容器按 Compose 项目分组，支持筛选、启动、停止、重启、暂停、恢复、终止与删除。
- 查看有界日志、跟随输出、时间戳、原始检查结果与进程信息。
- 统计页用四张自适应实时图显示 CPU、内存、网络和磁盘速率；采样间隔可选 1/2/5/10 秒。
- 列出镜像、卷、网络及其使用者，拉取镜像、从镜像创建容器、删除单项资源。
- 查看磁盘用量，显式确认后清理未使用资源。
- 跟随 Docker 的真实连接配置，或明确保存插件专用的 context / Unix socket 选择。
- 随宿主明暗主题及语言显示，长列表与日志采用虚拟化；离开页面取消活动请求。
- 左侧导航、资源详情与名称/镜像列支持拖动或键盘调整，松手后保存布局。
- 优先获取真实 Docker Hub 仓库 Logo；对已登记的公开镜像补充随包品牌图标，未知镜像使用通用图标。
- 详情采用分组卡片与虚拟表格，显示实际 inspect 标签、端口、挂载和网络信息。
- 日志支持搜索、自动换行与横向滚动；HTTP(S) 链接标蓝并通过宿主打开系统浏览器。
- 容器文件页支持目录导航、拖入普通文件上传、原生选择文件上传，以及选定文件下载。
- 容器详情提供「打开终端」，经 API-24 打开 Lithe 原生终端，选择 `/bin/sh`、`/bin/bash` 或 `/bin/ash` 进入运行中的容器。

Compose 分组动作仅作用于已存在的容器，并不执行 `docker compose up/down`，
不解析 Compose 文件，也不编排服务依赖。尚不支持构建镜像、
私有仓库登录、SSH/TLS/TCP 远端或 Dev Containers。

## 连接与数据

先启动 Docker Desktop、OrbStack、Colima 或其他兼容 Docker API 的本机引擎。
无需在运行时下载 SDK 或语言运行时。

连接选择依次是：插件保存的选择、`DOCKER_CONTEXT`、`DOCKER_HOST`、
Docker 配置中的当前 context、默认 Unix socket。
只有默认连接未配置且不存在时，才尝试常见桌面引擎的本机 socket。
显式选择失败时不会自动切换到另一个引擎。插件不会写入 Docker CLI 的配置。

插件的 `${stateDir}/settings.json` 仅保存 context 名称或 socket 路径。
Docker Hub Logo 由后端通过 HTTPS 读取仓库的公开 PNG 资源，页面不直接联网，
不使用 Docker 凭据；不把 GHCR、私有 registry 的地址送往 Docker Hub。
图标缓存放在 `${stateDir}/hub-logos-v1/<namespace>/<repository>.json`，正结果保存 7 天，
404 保存 1 小时；过期后按需更新，卸载插件时由宿主一并清理，可删除该目录重新获取。
这份缓存属于各用户插件状态，不是构建资源，不跨工作树复用，也不写入 app bundle 或插件包。
Docker Hub 的 Logo 接口不是稳定的 Docker Engine API，暂时不可用或未提供 Logo 时使用下述补充图标；无匹配则使用通用图标。
Elastic 自有仓库、Open WebUI、MinIO 和 pgvector 使用确切公开仓库名匹配补充图标；
来源、许可证和内容哈希保存在 `icons/brands/`，不复制 OrbStack 的程序资源，
不根据容器名、私有镜像名或模糊子串猜品牌。优先使用 Hub 返回的图标。
页面状态经宿主保存当前标签、所选资源、折叠分组和筛选开关；
不保存创建表单、环境变量、检查结果或容器日志。
布局还保存导航/详情大小和名称/镜像列宽。Compose 分组名旁显示引擎记录的配置文件路径，缺少文件标签时显示实际工作目录；不猜测 YAML 文件名。拖动复用 Lithe 的 document-resize-session，
只逐帧写入局部 CSS，结束时通过同一个页面状态保存；Web 插件无法直接使用原生 Swift 分隔组件。
日志、检查结果和进程 JSON 使用只读 CodeMirror 虚拟文本视口，支持自动换行、
横向滚动和连续选择复制；保留原有日志保留上限与退出页面取消请求的生命周期。
日志正文使用主题默认文字色，独立时间戳使用次要文字灰色，链接保持蓝色；
检查与进程 JSON 使用 CodeMirror JSON 解析器高亮，日志中的完整 JSON 行也使用同一解析器。
详情表格随页面显示完整行数、由外层滚动决定虚拟行范围，不再嵌套固定 8 行视口。
统计只保留最多 60 个内存采样用于绘图，不保存或显示历史数据表；页面状态仅保存采样间隔，
切换间隔会取消旧请求和计时器，退出统计页即停止采样，停止的容器不继续轮询。
插件包与 app bundle 在运行时保持只读，不影响代码签名或 Sparkle delta 基线。
删除和清理会作用于当前引擎，包括 Lithe 之外创建的资源；界面会先确认。

文件浏览需要运行中的 Linux 容器提供 `/bin/sh` 与 GNU/BusyBox `stat -c`；
不提供这些工具的精简容器会明确报错。上传下载使用 Docker Engine archive API，
不执行拼接的文件命令，不安装额外的容器服务。文件表格最多 5,000 项，
单文件最多 256 MiB；暂不支持文件夹上传、文件夹下载或拖出至 Finder。
上传前显示同名替换确认；下载先由宿主弹出保存对话框。切换容器或离开文件页会取消活动传输。
拖入上传按 192 KiB 分块，临时数据放 `${stateDir}/file-transfers/` 的匿名临时文件，
正常完成、取消、关闭后端或进程退出时清理；在途上传只属于开始时的 Docker socket，
切换连接后拒绝提交。下载临时文件放在用户选择的目标目录，完整成功后才原子替换所选文件。
这些状态不跨工作树复用，也不写入安装包。浏览器打开复用 `browser.open` 宿主权限，
清单使用 `backend:2`；旧宿主会拒绝不支持的能力，不绕过 Web 页面导航限制。

交互终端需要支持 API-24 的本机 macOS Lithe 工作区，以及终端 PATH 中可用的 Docker CLI；
所选 Shell 必须已安装在容器内。插件声明 `terminal.create` 权限，通过后端能力发现调用
`lithe/host/terminal/create`，以独立 executable/argv 执行
`docker --host unix://<当前 socket> exec --interactive --tty <容器 ID> <Shell>`。
不拼接宿主 Shell 命令，也不向已有用户终端注入文字。容器暂停或停止时不能打开。
创建和重开由 Lithe 显示原生执行确认；取消后不自动重试。
相同工作区、引擎、容器与 Shell 再次点击会显示已有终端，关闭后再次点击通过 `reopen`
启动新进程并恢复最多 16 KiB 显示历史，不重放输入，也不恢复 Shell 变量或旧进程。
关闭按钮通过 `dispose` 释放该 Shell 的终端和历史；删除容器/Compose 分组时也先释放对应句柄。
终端与句柄只保存在宿主和后端内存中，后端结束、禁用插件或关闭工作区由宿主清理；
网页关闭只取消在途操作，已交付的终端继续由用户使用。未增加磁盘缓存或安装目录写入。
浏览器预览不具备 Lithe 原生终端服务，点击时明确提示不可用。

## 开发与验证

需要 Node.js 24+、Rust 1.87+ 和 macOS。官方 Rust SDK 锁定在
`sdk-rust-v0.1.0`，具体提交与依赖版本由 `backend/Cargo.lock` 固定。

```sh
npm ci --prefix ui
npm --prefix ui run check
npm --prefix ui test
cargo fmt --manifest-path backend/Cargo.toml --all -- --check
cargo clippy --locked --manifest-path backend/Cargo.toml --all-targets -- -D warnings
cargo test --locked --manifest-path backend/Cargo.toml
node scripts/package.mjs --host-only
```

构建可分发的 universal 包前安装两个 target，然后不带 `--host-only` 打包：

```sh
rustup target add aarch64-apple-darwin x86_64-apple-darwin
node scripts/package.mjs
```

重新构建前端（`npm --prefix ui run build`）只更新 `ui/dist/`，不会更新安装 ZIP。
修改后必须再次运行 `node scripts/package.mjs`，再导入上面的成品 ZIP。
`node scripts/preview.mjs` 直接加载成品目录中的页面和后端，使用真实 Docker 数据验收；
运行前先打包，避免预览的代码和导入的安装包不一致。
预览的上传按钮使用浏览器文件选择器，只把选中的文件暂存到预览私有临时目录，
然后调用同一个真实后端上传；下载完成后浏览器接收一次性下载链接。
预览关闭会清理暂存文件，下载成功后立即清理对应暂存。Lithe 内继续使用宿主原生文件对话框。
统计图随窗口调整但有高度上限，大窗口下方保留留白；采样间隔和布局仍由页面状态保存。

真实 Core + Docker 验证使用独立的稀疏检出，版本固定在 `e2e/lithe-core.json`，
不会写入开发者的 Lithe 工作区。先准备好测试镜像以免镜像下载耗尽操作截止时间：

```sh
docker pull alpine:3.21
node scripts/e2e.mjs
```

CI 与发布流程的打包和真实 Docker 集成车道使用 `macos-15-intel`，与 Colima 上游的 macOS
集成车道一致，避免托管 ARM Mac 无法启动嵌套 VZ 虚拟机的问题；仍构建和校验
arm64 / x86_64 两个 universal slice。引擎启动限时 10 分钟，失败时输出 Lima 启动日志。

该场景读取现有资源，创建专用临时容器，验证日志、统计、进程、暂停、恢复、
重启、停止、启动、删除、中文文件名分块上传/下载、原生选中文件上传对应的后端路径、
取消传输清理、浏览器与 API-24 终端请求的 Core 权限/上下文路由、停止容器拒绝终端、连接持久化及包文件不变性。
浏览器实际启动、原生选择/保存对话框和 Finder 拖入仍应在目标 Lithe 宿主验收，
Core 测试只验证请求交给宿主，不伪造或宣称浏览器或原生终端已启动；无原生窗口的测试宿主对终端请求明确返回不可用。
安装包内的终端仍需在最新 Lithe 验收：打开/重复显示、输入、Ctrl-C、缩放、关闭后重开历史、取消确认、释放和禁用后无残留进程。
测试完成后删除临时容器。
不要在包含真实数据的引擎上执行全局清理测试；本场景不调用全局 prune。

所有插件自有界面文案来自 `shared/i18n/`；Docker 返回的诊断与资源名保持原文。
Docker 名称和标识属于 Docker, Inc.，本项目不是 Docker 官方产品。

License: Apache-2.0.
