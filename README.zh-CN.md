# ohmyserial

<p align="center">
  <img alt="ohmyserial" src="https://img.shields.io/badge/ohmyserial-串口共享中枢-0ea5e9?style=for-the-badge&logo=rust&logoColor=white" />
</p>

<p align="center">
  <strong>面向人类与 AI Agent 的跨平台开源串口共享中枢</strong><br/>
  <em>一个真串口 · 多个安全客户端 · 禁止静默字节交错</em>
</p>

<p align="center">
  <a href="./README.md"><img alt="English" src="https://img.shields.io/badge/lang-English-blue?style=flat-square" /></a>
  <a href="./README.zh-CN.md"><img alt="简体中文" src="https://img.shields.io/badge/lang-简体中文-red?style=flat-square" /></a>
  <a href="./README.es.md"><img alt="Español" src="https://img.shields.io/badge/lang-Español-green?style=flat-square" /></a>
</p>

<p align="center">
  <b>语言 / Languages:</b>
  <a href="./README.md">English</a> ·
  <a href="./README.zh-CN.md">简体中文</a> ·
  <a href="./README.es.md">Español</a>
</p>

<p align="center">
  <a href="https://github.com/tianrking/oh_myserial/actions/workflows/ci.yml"><img alt="CI" src="https://img.shields.io/github/actions/workflow/status/tianrking/oh_myserial/ci.yml?branch=main&style=flat-square&label=CI" /></a>
  <a href="./LICENSE"><img alt="License" src="https://img.shields.io/badge/license-MIT-blue?style=flat-square" /></a>
  <a href="https://www.rust-lang.org/"><img alt="Rust" src="https://img.shields.io/badge/rust-edition%202021-orange?style=flat-square&logo=rust" /></a>
  <img alt="Platforms" src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux%20%7C%20Windows-lightgrey?style=flat-square" />
  <img alt="Status" src="https://img.shields.io/badge/status-MVP-22c55e?style=flat-square" />
  <a href="https://github.com/tianrking/oh_myserial"><img alt="GitHub" src="https://img.shields.io/badge/github-tianrking%2Foh__myserial-181717?style=flat-square&logo=github" /></a>
</p>

<p align="center">
  <img alt="serial" src="https://img.shields.io/badge/serial-UART%20%2F%20COM%20%2F%20tty-0ea5e9?style=flat-square" />
  <img alt="hub" src="https://img.shields.io/badge/hub-复用%20%2F%20共享-8b5cf6?style=flat-square" />
  <img alt="websocket" src="https://img.shields.io/badge/API-HTTP%20%2B%20WebSocket-06b6d4?style=flat-square" />
  <img alt="agent" src="https://img.shields.io/badge/AI-Agent%20友好-f59e0b?style=flat-square" />
  <img alt="embedded" src="https://img.shields.io/badge/领域-嵌入式调试-64748b?style=flat-square" />
  <img alt="tokio" src="https://img.shields.io/badge/async-tokio-c026d3?style=flat-square" />
  <img alt="axum" src="https://img.shields.io/badge/web-axum-7c3aed?style=flat-square" />
  <img alt="pty" src="https://img.shields.io/badge/Unix-PTY-14b8a6?style=flat-square" />
  <img alt="tcp" src="https://img.shields.io/badge/stream-TCP-3b82f6?style=flat-square" />
  <img alt="toml" src="https://img.shields.io/badge/config-TOML-e11d48?style=flat-square" />
</p>

---

## 目录

- [项目是什么？](#项目是什么)
- [要解决什么问题？](#要解决什么问题)
- [功能一览](#功能一览)
- [工作原理](#工作原理)
- [平台支持](#平台支持)
- [安装与编译](#安装与编译)
- [快速开始](#快速开始)
- [怎么用（场景）](#怎么用场景)
- [配置说明](#配置说明)
- [命令行](#命令行)
- [HTTP / WebSocket API](#http--websocket-api)
- [发送策略（TX）](#发送策略tx)
- [Unix 虚拟串口（PTY）](#unix-虚拟串口pty)
- [Windows 说明](#windows-说明)
- [安全](#安全)
- [目录结构](#目录结构)
- [开发](#开发)
- [路线图](#路线图)
- [常见问题](#常见问题)
- [贡献](#贡献)
- [许可证](#许可证)
- [技术标签](#技术标签)

---

## 项目是什么？

**ohmyserial** 是用 Rust 编写的轻量、开源 **串口共享中枢（Serial Hub）**。

它会：

1. **独占打开**真实串口  
2. 把设备 **RX 广播**给多个客户端  
3. 对 **TX 做仲裁**，避免两路写入静默交错、协议乱掉  
4. 提供 **TCP、HTTP/WebSocket（给 Agent）、PTY（给传统上位机，macOS/Linux）**

典型场景：嵌入式调试时，**人要看串口，AI Agent / 脚本也要同时读写**。

| 项 | 内容 |
|----|------|
| 二进制名 | `ohmyserial` |
| 仓库 | [github.com/tianrking/oh_myserial](https://github.com/tianrking/oh_myserial) |
| 语言 | Rust（edition 2021） |
| 许可证 | MIT |
| 文档 | [English](./README.md)（默认）· **简体中文** · [Español](./README.es.md) |
| 产品/架构详述 | [`POSITIONING.md`](./POSITIONING.md) |

---

## 要解决什么问题？

| 你想… | 现实 |
|------|------|
| 上位机一直开着 | 串口已被占用 |
| Agent/脚本同时读日志 | 第二个程序打不开口 |
| 两边都能发命令 | 字节交错 → 指令/协议损坏 |

### 解法

```text
设备 (UART/COM)
        │
        ▼
   ┌──────────┐
   │ ohmyserial│  ← 唯一打开真串口的进程
   └────┬─────┘
        │
   ┌────┴─────────────────────────────┐
   ▼                ▼                 ▼
  PTY            TCP 流           HTTP + WebSocket
 (上位机)         (脚本)              (Agent)
```

---

## 功能一览

### 功能特性

| 功能 | 说明 | 状态 |
|------|------|------|
| 独占真串口 | 系统层只由 hub 打开硬件 | ✅ |
| 串口参数 | 波特率、数据位、校验、停止位、流控 | ✅ |
| RX 广播 | 所有可读客户端收到设备数据 | ✅ |
| TX 仲裁 | 按行/帧排队、独占、主客户端优先 | ✅ |
| 写锁租约 | 限时独占发送权 | ✅ |
| 断线重连 | 可选自动重新打开 | ✅ |
| TCP 客户端 | 原始双向字节流 | ✅ |
| HTTP API | health / status / write / lock | ✅ |
| WebSocket | 实时 RX（可带历史） | ✅ |
| Unix PTY | 符号链接虚拟串口 | ✅（macOS/Linux） |
| 会话日志 | 控制台 + 文件；text/hex | ✅ |
| Mock 口 | `mock:demo` 无硬件回环 | ✅ |
| TOML + CLI | `run` / `init` / `list-ports` / `status` | ✅ |
| 单进程多真口 | 多 profile | 🔜 |
| RFC2217 | 网络串口控制 | 🔜 |
| Windows 原生虚拟 COM | 驱动级 | 🔜 / 外部桥接 |

### 技术特性

| 方面 | 技术 |
|------|------|
| 运行时 | Tokio |
| HTTP/WS | Axum |
| 串口 | `serialport` + 独立读线程 |
| 配置 | Serde + TOML |
| 日志 | `tracing` + 会话 blog |
| Unix PTY | `nix` openpty |
| 测试 | 单元 + 集成（mock） |
| CI | Ubuntu · macOS · Windows |

---

## 工作原理

### 数据面

```text
设备 ──RX──► Serial Core ──► Broker 广播 ──► 各客户端
客户端 ──TX──► Broker 准入(策略/锁) ──► Serial Core ──► 设备
```

### 控制面

- `GET /v1/status`：连接状态、波特率、客户端、锁、计数  
- `POST/DELETE /v1/lock`：写锁  
- `POST /v1/write`：以命名客户端注入发送  

### 模块结构

```text
CLI / 配置
    └── Hub
            ├── Serial 核心（打开、重连、mock）
            ├── Broker（注册、广播、TX 队列）
            ├── Policy（发送策略）
            ├── Clients：PTY · TCP · HTTP/WS
            └── Observe（日志）
```

---

## 平台支持

| 能力 | macOS | Linux / Ubuntu | Windows |
|------|:-----:|:--------------:|:-------:|
| 真串口 | ✅ | ✅ | ✅ |
| TCP | ✅ | ✅ | ✅ |
| HTTP + WebSocket | ✅ | ✅ | ✅ |
| PTY 虚拟串口 | ✅ | ✅ | — |
| Mock 回环 | ✅ | ✅ | ✅ |

**Ubuntu：** 编译前安装 `build-essential pkg-config libudev-dev`。  
**Windows：** 仅认 COM 的老上位机需 TCP/WS 或外部虚拟 COM 桥；不支持 `type = "pty"`。

---

## 安装与编译

### 依赖

- [Rust](https://rustup.rs/) stable  
- **Ubuntu/Debian：**

  ```bash
  sudo apt update
  sudo apt install -y build-essential pkg-config libudev-dev
  ```

### 源码编译

```bash
git clone https://github.com/tianrking/oh_myserial.git
cd oh_myserial
cargo build --release
```

| 系统 | 二进制 |
|------|--------|
| Unix | `./target/release/ohmyserial` |
| Windows | `.\target\release\ohmyserial.exe` |

```bash
cargo test
./target/release/ohmyserial --help
```

---

## 快速开始

### 1）生成配置

```bash
./target/release/ohmyserial init -o ohmyserial.toml
```

默认 **`path = "mock:demo"`**，无需硬件。

### 2）启动

```bash
./target/release/ohmyserial run -c ohmyserial.toml
```

| 服务 | 默认地址 |
|------|----------|
| HTTP / WebSocket | `http://127.0.0.1:8787` · `ws://127.0.0.1:8787/v1/stream` |
| TCP | `127.0.0.1:8788` |

### 3）试 API

```bash
curl -s http://127.0.0.1:8787/v1/health
curl -s -X POST http://127.0.0.1:8787/v1/write \
  -H 'content-type: application/json' \
  -d '{"text":"hello","newline":true}'
```

### 4）接真设备

```toml
[real]
path = "/dev/cu.usbmodem14101"   # macOS
# path = "/dev/ttyUSB0"          # Linux
# path = "COM3"                  # Windows
baud = 115200
```

```bash
./target/release/ohmyserial list-ports
```

---

## 怎么用（场景）

### 核心：一个真串口 → 并联多个监控/交互端

```text
                 ┌─ PTY /tmp/ohmyserial-v0  → 串口上位机 #1
                 ├─ PTY /tmp/ohmyserial-v1  → 串口上位机 #2 / Agent 读虚拟串口
 真串口 ──► hub ─┼─ TCP :8788            → 多个脚本同时连
                 ├─ TCP :8789            → 更多工具
                 └─ WS  /v1/stream        → 多个 Agent 同时连
```

所有端看到**同一份实时 RX**；TX 由策略/写锁仲裁，避免静默交错。

用 **`[fanout]`** 一键批量生成，或用 `[[clients]]` 逐个声明。

```bash
curl -s http://127.0.0.1:8787/v1/endpoints
```

### A. 多上位机 + Agent

```toml
[fanout]
pty_count = 2
pty_link_prefix = "/tmp/ohmyserial-v"
tcp_count = 1
tcp_base_port = 8788
```

- 两个串口软件分别打开 `v0` / `v1`  
- Agent：`ws://127.0.0.1:8787/v1/stream`（可多连）  
- 脚本：`nc 127.0.0.1 8788`（可多连）

### B. 仅脚本 / CI

`tcp_count = 2` + API，不配 PTY。

### C. 无硬件演示

`path = "mock:demo"` 回环。

### D. 写锁独占窗口

```bash
curl -s -X POST http://127.0.0.1:8787/v1/lock \
  -H 'content-type: application/json' \
  -d '{"as_client":"agent"}'
curl -s -X DELETE http://127.0.0.1:8787/v1/lock
```

---

## 配置说明

完整示例：[`ohmyserial.example.toml`](./ohmyserial.example.toml)

```toml
[real]
path = "mock:demo"
baud = 115200
reconnect = true

[tx]
mode = "queue_by_line"     # queue_by_line | queue_by_frame | exclusive | primary_wins
primary = "ui"
write_lock_ms = 3000
slow_client = "drop_oldest"

[api]
bind = "127.0.0.1:8787"
enabled = true

[[clients]]
type = "tcp"
name = "tcp"
bind = "127.0.0.1:8788"

[[clients]]
type = "websocket"
name = "agent"
history_bytes = 65536

# 仅 macOS / Linux
# [[clients]]
# type = "pty"
# name = "ui"
# link = "/tmp/ohmyserial-ui"

[log]
mirror_console = true
format = "hex+text"
```

| 字段 | 含义 |
|------|------|
| `real.path` | 设备路径或 `mock:名称` |
| `tx.mode` | 并发写策略 |
| `api.bind` | HTTP/WS 地址（建议本机） |
| `can_read` / `can_write` | 客户端权限 |

---

## 命令行

```bash
ohmyserial run -c ohmyserial.toml    # 启动 hub
ohmyserial init [-o file]           # 生成示例配置
ohmyserial list-ports               # 列出串口
ohmyserial status [--api URL]       # 查询运行状态
```

```bash
RUST_LOG=debug ohmyserial run -c ohmyserial.toml
```

---

## HTTP / WebSocket API

**默认根地址：** `http://127.0.0.1:8787`

| 方法 | 路径 | 说明 |
|------|------|------|
| `GET` | `/v1/health` | 存活检查 |
| `GET` | `/v1/status` | 串口/客户端/锁/统计 |
| `GET` | `/v1/clients` | 客户端列表 |
| `POST` | `/v1/write` | 向设备发送 text 或 hex |
| `POST` | `/v1/lock` | 申请写锁 |
| `DELETE` | `/v1/lock` | 释放写锁 |
| `WS` | `/v1/stream` | 实时 RX |

### 发送示例

```bash
curl -s -X POST http://127.0.0.1:8787/v1/write \
  -H 'content-type: application/json' \
  -d '{"text":"AT","newline":true,"as_client":"agent"}'

curl -s -X POST http://127.0.0.1:8787/v1/write \
  -H 'content-type: application/json' \
  -d '{"hex":"41 54 0d 0a","as_client":"agent"}'
```

### WebSocket

```text
ws://127.0.0.1:8787/v1/stream
```

### TCP

```bash
nc 127.0.0.1 8788
```

### 最小 Python 示例

```python
import json, urllib.request

req = urllib.request.Request(
    "http://127.0.0.1:8787/v1/write",
    data=json.dumps({"text": "status", "newline": True}).encode(),
    headers={"content-type": "application/json"},
    method="POST",
)
print(urllib.request.urlopen(req).read().decode())
```

---

## 发送策略（TX）

| 模式 | 行为 | 适用 |
|------|------|------|
| `queue_by_line` **（默认）** | 等到 `\n` 再整行发送 | 文本 / AT / CLI |
| `queue_by_frame` | 等到分隔字节 | 简单二进制帧 |
| `exclusive` | 必须持有写锁才能发 | 烧录 / 危险操作 |
| `primary_wins` | 优先 `tx.primary` | 人在环 |

**写锁** 生效期间仅所有者可 TX；超时、主动释放或断线会释放。

`slow_client = drop_oldest`：保证真口读取不被慢客户端拖死。

---

## Unix 虚拟串口（PTY）

```toml
[[clients]]
type = "pty"
name = "ui"
link = "/tmp/ohmyserial-ui"
can_write = true
can_read = true
```

在 minicom / screen / Serial Studio 等中打开 `/tmp/ohmyserial-ui`。

> 真实波特率由 hub 的 `[real]` 决定。部分软件对 PTY 的波特率 ioctl 可能失败，数据通道通常仍可用。

---

## Windows 说明

| 需求 | 做法 |
|------|------|
| Agent / 自动化 | HTTP + WebSocket ✅ |
| 简单字节流 | TCP `127.0.0.1:8788` ✅ |
| 硬件 | `path = "COM3"` ✅ |
| 只认 COM 的老上位机 | 外部桥（如 com0com），尚未内置 |
| `type = "pty"` | 不支持 |

---

## 安全

- 默认只绑 **`127.0.0.1`**  
- 串口写入等同于碰硬件（复位、危险指令）  
- 勿在不可信网络把服务绑到 `0.0.0.0`（MVP 无鉴权）  
- 日志可能含设备吐出的敏感信息  

---

## 目录结构

```text
oh_myserial/
├── README.md           # English（默认）
├── README.zh-CN.md     # 简体中文
├── README.es.md        # Español
├── POSITIONING.md
├── ohmyserial.example.toml
├── src/ ...
└── tests/
```

---

## 开发

```bash
cargo test
cargo run -- run -c ohmyserial.example.toml
cargo fmt
cargo clippy
```

CI：Ubuntu · macOS · Windows。

---

## 路线图

| 阶段 | 内容 |
|------|------|
| ✅ MVP | 核心 hub、策略、TCP、HTTP/WS、PTY、mock、日志、CLI |
| 🔜 下一步 | 多真口、更强历史缓冲、Windows COM 桥文档、加固 |
| 🧭 更后 | RFC2217、录制回放、轻量 Web 监视、指标导出 |

---

## 常见问题

**两个客户端能同时写吗？**  
不会静默交错字节。默认按完整行排队；写锁可给独占窗口。

**Agent 必须虚拟 COM 吗？**  
不必，推荐 WebSocket + HTTP。

**PTY 上改波特率为何不影响设备？**  
真口参数由 hub 持有。

**这是串口嗅探器吗？**  
不是。它是可交互的 **共享中枢**，带 TX 控制。

**mock 需要硬件吗？**  
不需要。

---

## 贡献

Issue / PR：https://github.com/tianrking/oh_myserial  

请与 [`POSITIONING.md`](./POSITIONING.md) 对齐。

---

## 许可证

[MIT](./LICENSE) © ohmyserial contributors

---

## 技术标签

`串口` · `UART` · `COM` · `tty` · `串口共享` · `串口复用` · `嵌入式` · `调试` · `AI Agent` · `WebSocket` · `HTTP API` · `TCP` · `PTY` · `Tokio` · `Axum` · `Rust` · `跨平台` · `macOS` · `Linux` · `Windows` · `TOML` · `MIT` · `ohmyserial`

---

<p align="center">
  <a href="./README.md">English</a> ·
  <a href="./README.zh-CN.md">简体中文</a> ·
  <a href="./README.es.md">Español</a>
  <br/>
  <sub>一个口。多个客户端。互不踩脚。</sub>
</p>
