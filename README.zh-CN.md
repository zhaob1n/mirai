<div align="center">

<img src="crates/mirai/resources/icons/hicolor/scalable/apps/io.github.zhaob1n.Mirai.svg" width="96" alt="">

# mirai

**为 GNOME 桌面打造的围棋分析、复盘与对弈工具，由 KataGo 驱动。**

[English](README.md) | 简体中文

</div>

![mirai 正在用 KataGo 实时分析一局棋](https://github.com/zhaob1n/mirai/releases/download/readme-assets/preview.png)

mirai 把 KataGo 的计算结果放到一块原生 Linux 棋盘上。每个候选手亏多少、这个判断有多可信，一眼就能看出；胜率图标出整局的每一步问题手；棋谱树保留你试过的每个变化。你可以和任意强度的引擎对弈，也可以从野狐拉一盘棋下来，让 KataGo 陪你复盘。

引擎不必和 mirai 在同一台电脑上。在有显卡的机器上运行 `mirai-server`，没有显卡的笔记本也能照样分析。

---

## 亮点

- **一目了然的分析。** 每个候选手都标出胜率、目差和计算量。颜色表示它比引擎首选亏了多少：从青色经过绿色、黄色一直到红色；色块越实，背后的计算越充分。鼠标悬停在候选手上，就能在棋盘上看到它的变化，而不会改动棋谱。
- **全盘复盘。** 一个按键就能分析整条主线。胜率和目差曲线随之补全，图下方的问题手条从落子一方的角度标出每一步失误，问题手列表可以直接跳到那一手。
- **地盘图和策略图** 显示 KataGo 认为每个点最终归谁，以及神经网络在搜索之前最想下在哪里。
- **真正的 SGF 编辑器。** 变化、摆子、标记、标签和注释，都能撤销和重做。支持多局合集；mirai 不显示的属性会原样保留，其他软件的棋谱存回去也不会丢东西。
- **和 KataGo 对弈**：按计算量、按每手用时，或者配合拟人网络按段位模仿人类棋风。棋盘从 2×2 到 19×19，支持让子、九种规则，以及包干、读秒和费舍尔加秒三种计时。双方停一手后由 KataGo 判断死子，判断错的棋块点一下就能改。也可以不用引擎，自己下双方。
- **野狐棋谱。** 按昵称或 UID 查找野狐围棋玩家，打开其最近的任意一盘公开对局。
- **安全的远程引擎。** `mirai-server` 通过 QUIC 把一个或多个 KataGo 共享给局域网里的所有客户端。客户端用令牌认证，并在第一次连接时固定服务器的证书指纹。
- **原生、流畅。** 基于 GTK 4 和 libadwaita，支持浅色和深色样式，在 Linux 上棋盘由 GTK 的 GPU 渲染器绘制。多个窗口共用一个 KataGo；程序意外退出后，自动保存会把你的棋谱找回来。
- **说你的语言。** 目前支持英文和简体中文，欢迎贡献更多翻译。

---

## 环境要求

- 当前稳定版 Rust（2024 edition），由 `rust-toolchain.toml` 选定，不支持更旧的编译器。
- GTK 4.22+、libadwaita 1.9+、libsoup 3 和 Blueprint Compiler 0.22+，以及它们的开发包。
- GNU gettext，用于编译翻译。
- [`just`](https://github.com/casey/just)，用于安装。
- KataGo 程序和神经网络模型（JSON 分析模式，不是 GTP）。mirai 不会替你下载 KataGo。
- 如需落子音效，还要安装 GStreamer 的 good 插件（Arch 上为 `gst-plugins-good`，Debian/Ubuntu 上为 `gstreamer1.0-plugins-good`），GTK 通过它播放声音。没有它 mirai 也能运行，只是没有声音。

Arch：`pacman -S gtk4 libadwaita libsoup3 blueprint-compiler gettext just`。Debian/Ubuntu：安装 `libgtk-4-dev`、`libadwaita-1-dev`、`libsoup-3.0-dev`、`blueprint-compiler`、`gettext` 和 `just`；Fedora：安装 `gtk4-devel`、`libadwaita-devel`、`libsoup3-devel`、`blueprint-compiler`、`gettext` 和 `just`。请注意版本：早于 GNOME 50 的发行版自带的 GTK 和 libadwaita 太旧，无法构建 mirai。

```
just build
sudo just install        # 两者都装；或 `just install mirai` / `just install mirai-server`
sudo just uninstall      # 同上
```

这会把 `mirai`、`mirai-server`、桌面文件、元信息、图标和翻译安装到 `/usr/local`；`just prefix=$HOME/.local install` 不需要 root 权限。在 Arch 上，也可以用 [`packaging/aur/`](packaging/aur/) 中的 PKGBUILD 构建 `mirai-git` 和 `mirai-server-git`（尚未发布到 AUR）。`tools/packaging/makepkg-local.sh` 会从当前检出的最新提交而不是 GitHub 构建；如果 cargo 来自 rustup，请加上 `-d`，然后执行 `sudo pacman -U target/archpkg/*.pkg.tar.zst`。

**Windows** 版在 Linux 上交叉编译，在 Fedora 容器里用 podman（或 docker）完成：

```
tools/packaging/windows-cross.sh
```

产物是 `target/windows/dist/` 下的便携压缩包和按用户安装的安装程序（尚未发布）；Windows 上如何使用 KataGo 见[指南](docs/user/GUIDE.zh-CN.md#在-windows-上)。第一次运行会构建镜像和所有 crate；之后只重新编译改动过的部分。

---

## 快速上手

```
cargo run -p mirai
cargo run -p mirai -- game.sgf
```

如果找到了 KataGo 和模型，mirai 会立即开始分析；否则请在首选项中添加。打开一份棋谱，按 <kbd>Space</kbd> 开始实时分析，按 <kbd>Ctrl</kbd>+<kbd>A</kbd> 进行全盘分析。设置方法和首次运行的行为见[用户指南](docs/user/GUIDE.zh-CN.md#2-首次运行)。

mirai 跟随桌面的语言设置。想换一种语言试试，可以用 `LANGUAGE` 启动，例如 `LANGUAGE=zh_CN mirai`。

---

## 通过网络使用引擎

在有显卡的机器上：

```
mirai-server --generate-token
mirai-server --config server.toml
```

令牌、UDP 端口、服务器配置和证书验证的详细说明见[远程引擎指南](docs/user/GUIDE.zh-CN.md#7-使用远程引擎)。

---

## 反馈

mirai 还很年轻，变化很快，目前还没有正式版本：请从本仓库构建。欢迎把 bug、不顺手的地方和各种想法提到 [issues](https://github.com/zhaob1n/mirai/issues)。请写明你在做什么、期望发生什么、实际发生了什么；如果是引擎的问题，`~/.local/share/mirai/katago-logs/` 里最新的日志通常能说明原因。

---

## 文档

- 用户：[docs/user/GUIDE.zh-CN.md](docs/user/GUIDE.zh-CN.md) —— 首次运行、界面、分析、野狐、对弈、远程引擎、设置和快捷键。
- 译者：[docs/dev/TRANSLATING.md](docs/dev/TRANSLATING.md)（英文）。
- 贡献者和智能体：[AGENTS.md](AGENTS.md)（英文）。

---

## 许可证

GNU 通用公共许可证第 3 版或更高版本（[`LICENSE`](LICENSE)）。

mirai 是自由软件：你可以依据自由软件基金会发布的 GNU 通用公共许可证第 3 版或（由你选择）任何更高版本的条款，重新分发和/或修改它。发布 mirai 是希望它有用，但不提供任何担保，甚至不包括适销性或特定用途适用性的默示担保。详情请参阅 GNU 通用公共许可证。
