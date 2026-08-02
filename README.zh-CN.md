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
- [事件账本与安全回放](#事件账本与安全回放)
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
| 事件账本 | 版本化 RX/TX/连接/控制/gap 证据；有界内存 + 可选哈希 NDJSON | ✅ |
| 安全回放 | 校验后只读的 `immediate` / `original` / `manual` 回放 | ✅ |
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
| 证据 | 规范 v1 账本 + SHA-256 链式 NDJSON 分段 |
| 回放 | 只输出校验后的事件；与实时 Broker、串口写入隔离 |
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
            ├── Ledger（有序证据、有界 ring、可选持久化）
            ├── Replay（校验后只读、离线）
            └── Observe（人类可读日志）
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

## 快速开始（最省事）

**不用写配置文件**，一条命令：

```bash
# 1) 看本机有哪些串口
./target/release/ohmyserial list-ports

# 2) 共享（macOS/Linux 默认并联 2 个虚拟串口 + TCP + WebSocket）
./target/release/ohmyserial share /dev/cu.usbmodem14101 --baud 115200

# 要 3 个虚拟串口给 3 个上位机
./target/release/ohmyserial share /dev/ttyUSB0 --pty 3

# Windows：没有 PTY，用多 TCP + 多 WS
./target/release/ohmyserial share COM3 --tcp 2

# 无硬件演示
./target/release/ohmyserial share mock:demo
```

启动后终端会打印 **连接卡片**（SERIAL / TCP / WS 地址）。

| 参数 | 含义 | 默认 |
|------|------|------|
| `--pty N` | N 个虚拟串口（Unix） | macOS/Linux=`2`，Windows=`0` |
| `--tcp N` | N 个 TCP 端口 | `1` |
| `--api` | HTTP/WS 地址 | `127.0.0.1:8787` |

高级：`init` 生成 TOML，或 `run -c file.toml -d 设备 --pty 3`。

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
LEASE_TOKEN="$(curl -s -X POST http://127.0.0.1:8787/v1/lock \
  -H 'content-type: application/json' \
  -d '{"as_client":"agent"}' | jq -r '.lock.lease_token')"

curl -s -X POST http://127.0.0.1:8787/v1/write \
  -H 'content-type: application/json' \
  -d "{\"text\":\"AT\",\"newline\":true,\"as_client\":\"agent\",\"lease_token\":\"$LEASE_TOKEN\"}"

# 在租约到期前续租
curl -s -X POST http://127.0.0.1:8787/v1/lock \
  -H 'content-type: application/json' \
  -d "{\"lease_token\":\"$LEASE_TOKEN\"}"

curl -s -X DELETE http://127.0.0.1:8787/v1/lock \
  -H 'content-type: application/json' \
  -d "{\"lease_token\":\"$LEASE_TOKEN\"}"
```

`as_client` 只是显示和审计名称，不是凭证；同名客户端不能冒充持锁者。请把不透明的 `lease_token` 当作秘密，仅保存在内存中。

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
write_timeout_ms = 5000
max_frame_bytes = 65536
max_write_bytes = 65536
slow_client = "drop_oldest"
client_queue = 256
slow_block_ms = 1000

[api]
bind = "127.0.0.1:8787"
enabled = true
can_read = true
can_write = true
# token_env = "OHMYSERIAL_API_TOKEN"
# cors_origins = ["https://serial-console.example.com"]

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

[ledger]
memory_events = 16384
memory_bytes = 33554432
stream_capacity = 1024
# directory = "./ohmyserial-ledger"  # 开启哈希 NDJSON 持久化
rotate_bytes = 67108864
fsync_each_event = false

[log]
mirror_console = true
format = "hex+text"
```

| 字段 | 含义 |
|------|------|
| `real.path` | 设备路径或 `mock:名称` |
| `tx.mode` | 并发写策略 |
| `tx.write_timeout_ms` | 入队到主机写入确认的总超时 |
| `tx.max_frame_bytes` / `max_write_bytes` | 流式帧与原子写入的大小上限 |
| `tx.slow_client` / `client_queue` / `slow_block_ms` | 每个读客户端的有界背压策略 |
| `api.bind` | HTTP/WS 地址；明文监听始终限制为回环地址 |
| `api.token_env` | 保存 API Bearer 密钥的环境变量名，密钥不写入 TOML |
| `api.cors_origins` | 精确的浏览器 Origin 白名单；空值仅同源，拒绝 `*` |
| `ledger.memory_events` / `memory_bytes` | 始终启用的有界事件证据 ring |
| `ledger.directory` | 可选的追加式哈希 NDJSON 持久化根目录 |
| `ledger.stream_capacity` / `rotate_bytes` | 实时事件订阅上限 / 分段大小目标 |
| `ledger.fsync_each_event` | 每个事件都强制进入系统存储缓存；更稳但更慢 |
| `can_read` / `can_write` | 客户端权限 |

---

## 命令行

```bash
ohmyserial run -c ohmyserial.toml    # 启动 hub
ohmyserial init [-o file]           # 生成示例配置
ohmyserial list-ports               # 列出串口
ohmyserial status [--api URL]       # 查询运行状态
ohmyserial replay <source>          # 校验并输出封存的账本捕获
ohmyserial replay <source> --mode original --speed 2
ohmyserial replay <source> --mode manual --step 10
```

```bash
RUST_LOG=debug ohmyserial run -c ohmyserial.toml
```

---

## HTTP / WebSocket API

**默认根地址：** `http://127.0.0.1:8787`

明文 API 和独立 WebSocket 监听始终限制为回环地址。配置 `api.token_env` 后，除 `/v1/health` 外的所有 `/v1/*` 请求都必须带 `Authorization: Bearer <token>`。密钥只从该环境变量读取，不要放进 TOML、URL 或日志。Bearer 不能保护网络上的明文 `http://` / `ws://`，因此即使配置 token，非回环监听也会拒绝启动；远程访问请使用 SSH 隧道，或在回环监听前部署 TLS 反向代理。

浏览器默认只能同源调用；请求的 Host 还必须对应实际回环监听地址，以阻断 DNS rebinding 别名。`api.cors_origins` 只放行逐项精确匹配的 Origin，通配符会被拒绝。WebSocket 还会单独检查 `Origin` 是否同主机或在白名单中。浏览器用 `new WebSocket(url, ["bearer", token])` 传 Bearer，不支持查询参数 token；无 `Origin` 的非浏览器客户端在启用鉴权时仍须提供 Bearer。

| 方法 | 路径 | 说明 |
|------|------|------|
| `GET` | `/v1/health` | 存活检查 |
| `GET` | `/v1/status` | 串口/客户端/锁/统计 |
| `GET` | `/v1/endpoints` | 已配置的并联端点 |
| `GET` | `/v1/clients` | 客户端列表 |
| `GET` | `/v1/events/status` | 账本会话、ring、持久化和恢复状态 |
| `GET` | `/v1/events` | 按游标查询，可过滤类型/epoch/actor/字节 |
| `GET` | `/v1/events/export` | 规范事件 NDJSON 导出 |
| `POST` | `/v1/write` | 向设备发送 text 或 hex |
| `POST` | `/v1/lock` | 申请写锁 |
| `DELETE` | `/v1/lock` | 释放写锁 |
| `WS` | `/v1/stream` | 实时 RX |
| `WS` | `/v1/events/stream` | 只读 JSON 事件：先快照再实时 |

### 发送示例

```bash
curl -s -X POST http://127.0.0.1:8787/v1/write \
  -H 'content-type: application/json' \
  -d '{"text":"AT","newline":true,"as_client":"agent"}'

curl -s -X POST http://127.0.0.1:8787/v1/write \
  -H 'content-type: application/json' \
  -d '{"hex":"41 54 0d 0a","as_client":"agent"}'
```

HTTP 的 text/hex 都按一次原子命令处理，不受分隔符组帧影响。`ok: true` 表示串口所有者线程已完成主机侧 `write_all` 和 `flush`，不代表设备已解析或确认命令。入队和确认共用 `tx.write_timeout_ms` 截止时间；若错误提示结果可能为 partial/unknown，驱动可能已经写出部分或全部字节，不能盲目重试。

租约生效时，写请求必须携带 `lease_token`。`POST /v1/lock` 首次申请会返回随机 token；带 token 再次 POST 是续租；`DELETE /v1/lock` 带 token 才能释放。状态接口不会泄露 token。

### WebSocket

```text
ws://127.0.0.1:8787/v1/stream
```

- 服务端发二进制 RX；连接后可能先发送历史缓存。
- 客户端发文本时补换行并走流式组帧；发二进制时整帧作为一次原子写入，受 `max_write_bytes` 限制。
- 写入被拒绝时会收到 `type = "ohmyserial.error"` 的 JSON 文本帧。WebSocket 入队不等于设备写入确认；需要主机写入 + flush 结果时使用 HTTP。

### TCP

```bash
nc 127.0.0.1 8788
```

原始 TCP 没有 API Bearer 握手，必须保持回环监听。远程使用时通过 SSH 隧道转发：

```bash
ssh -L 8788:127.0.0.1:8788 user@device-host
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

## 事件账本与安全回放

每次运行都有一条有序的 v1 证据流，记录进程实际观察到的精确 RX 字节、主机侧确认完成的 TX、连接世代、控制动作和显式不确定性 `gap`。有界内存 ring 始终启用；设置 `ledger.directory` 后，还会写入轮转的 SHA-256 链式 NDJSON 分段，执行保守的崩溃恢复，并支持磁盘补全查询/导出。

```bash
# 从排他序号游标之后查询规范事件。
curl -s 'http://127.0.0.1:8787/v1/events?after_seq=0&limit=100&type=rx,tx'

# 回放一个封存分段，或只包含一个会话的目录。
ohmyserial replay ./one-session-directory --mode original --speed 2
```

原始 `/v1/stream` WebSocket 承载双向串口字节；`/v1/events/stream` 是另一条只读通道，发送 JSON 文本事件并支持游标/过滤器。回放只会校验并输出原始事件，不会打开设备，也不会把历史 TX 重新送进实时 Broker。

分段哈希能发现损坏和链断裂，但不是数字签名或来源证明；程序也不会自动删除旧分段。RX 证据从进程成功读到字节时才开始，硬件或驱动在此之前的丢失可能无法得知。主机侧 TX `write_all` + `flush` 不等于设备确认。Mock 测试也不等于硬件在环测试。

完整的 envelope、事件类型、gap 语义、存储/恢复模型、API 分页与 WebSocket 补流流程、回放安全边界见 [`EVENTS.md`](./EVENTS.md)。

---

## 发送策略（TX）

| 模式 | 行为 | 适用 |
|------|------|------|
| `queue_by_line` **（默认）** | 等到 `\n` 再整行发送 | 文本 / AT / CLI |
| `queue_by_frame` | 等到分隔字节 | 简单二进制帧 |
| `exclusive` | 必须持有写锁才能发 | 烧录 / 危险操作 |
| `primary_wins` | 优先 `tx.primary` | 人在环 |

**写租约** 生效期间，只有携带随机 `lease_token` 的请求可以 TX；owner 字符串只用于显示/审计。租约仅在 TTL 到期或持 token 主动释放时结束，同名 HTTP、WebSocket、TCP 或 PTY 客户端断线不会释放租约。

每个可读客户端都有以块计数的有界 RX 队列 `client_queue`：

| `slow_client` | 队列满时的行为 |
|---------------|----------------|
| `drop_oldest` **（默认）** | 丢弃该客户端最旧的待处理 RX 块，再放入新块 |
| `drop_newest` | 保留已有数据，丢弃发给该客户端的新块 |
| `disconnect_slow` | 立即断开慢客户端 |
| `block` | 最多等待 `slow_block_ms`，仍无空间则断开慢客户端 |

`queue_by_line` / `queue_by_frame` 的缓存受 `max_frame_bytes` 限制，HTTP 和 WS 二进制原子写受 `max_write_bytes` 限制。每次连接都有递增 epoch；写入在真正触发主机写操作前会再次核对 epoch、截止时间和租约，因此断线前排队的旧字节不会在重连后回放到新会话。

启动时会先完成配置校验和监听端口绑定；任一绑定失败都会让 hub 启动失败并撤销已经启动的任务。关闭时会停止串口所有者、关闭 fan-out，并拒绝/排空待写队列，不留下脱管后台任务。

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
- 明文 API、WebSocket 和原始 TCP 都只能监听回环地址；远程访问使用 SSH 或 TLS 反向代理
- 人类日志、事件分段和导出都可能含设备吐出的敏感信息
- 分段哈希用于发现损坏，不能认证捕获的产生者或修改者

---

## 目录结构

```text
oh_myserial/
├── README.md           # English（默认）
├── README.zh-CN.md     # 简体中文
├── README.es.md        # Español
├── POSITIONING.md
├── EVENTS.md           # 规范事件账本、持久化、API、回放
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
| ✅ 基础 | 核心 hub、可信 TX、租约、TCP、HTTP/WS、PTY、mock、日志、CLI |
| ✅ 证据 | 规范事件账本、可选哈希分段、查询/导出/事件 WS、安全回放 |
| 🔜 下一步 | 受控工作流、设备身份/控制线、独占交接、多端口监督 |
| 🧭 更后 | RFC2217、Windows COM 桥文档、更丰富的 Web 证据工具、指标导出 |

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
不需要。`mock:demo` 能验证 hub、策略、租约、API 和关闭流程，但不能证明操作系统串口驱动、USB/UART 时序、物理控制线或真实设备命令确认正确。

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
