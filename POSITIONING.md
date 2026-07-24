# oh_myserial — 产品定位与完整架构

> 本文档冻结产品方向与架构边界，作为设计与实现的单一事实来源（SSOT）。  
> 实现细节可演进；未经讨论，不轻易改动本文中的原则与 ADR。

---

## 1. 一句话定位

> **跨平台、开源、小巧的串口共享中枢：独占真实串口，把数据安全分发给人类上位机与 AI Agent。**

英文：

> **A tiny, open-source serial hub for humans and agents — one real port, many safe clients, all platforms.**

---

## 2. 要填的空白（Why）

| 现状 | 缺口 |
|------|------|
| Windows 商业 Splitter | 闭源、收费、锁平台 |
| com0com + hub4com | 开源但老旧、Windows only、体验碎 |
| socat / ser2net | 零件强，不是「人 + Agent 共调试」产品 |
| macOS / 跨架构 | 几乎没有完整开源 Hub |

**我们做的不是「又一个虚拟串口驱动」，而是：**

```text
Serial Hub = 真串口独占 + 多客户端共享 + 写仲裁 + 双形态接入 + 可观测
```

时代红利：**嵌入式调试 × 自动化 × AI Agent 同时要碰串口**。

---

## 3. 产品原则（How we win）

1. **小巧优先** — 单二进制、低依赖、配置文件即可跑  
2. **工作流完整，而非按钮最多** — 人能用、Agent 能用、不互踩  
3. **默认安全可预期** — TX 策略清晰，冲突有规则  
4. **跨平台一等公民** — Windows / Linux / macOS，x64 / arm64  
5. **开源免费** — 建议 MIT 或 Apache-2.0  
6. **可扩展但不膨胀** — 核心固定，协议/接入可插拔  

反模式：一上来对标商业 VSPD 全功能、堆 GUI、做企业云。

---

## 4. 目标用户与场景

| 用户 | 场景 |
|------|------|
| 嵌入式工程师 | 上位机看日志的同时，脚本/Agent 自动测 |
| Agent / 自动化 | 读串口流、按策略发命令，不抢真口 |
| 产线 / CI | 烧录后冒烟，多消费者读同一日志 |
| 远程协作 | 本机真口，远端通过 TCP/WS 旁路观察 |

**非目标（至少 v1）：** 替代完整 IDE、做协议分析仪全家桶、做商业级内核虚拟 COM 驱动生态。

---

## 5. 核心价值主张

对外三句话：

1. **一个人打开串口，程序也能同时看。**  
2. **Agent 能读、能写，但不会和人手撕协议。**  
3. **Windows / Linux / macOS 同一套用法。**

对内能力公式：

```text
oh_myserial = RealPort Exclusive
            + RX Fan-out
            + TX Arbitration
            + Client Adapters (PTY/COM-like, TCP, WebSocket, HTTP)
            + Session Observability
```

---

## 6. 功能分层（做什么 / 不做什么）

### 6.1 MVP（必须有，第一完整版本）

| 能力 | 说明 |
|------|------|
| 打开/关闭真串口 | path、baud、data/parity/stop、flow |
| 独占占用 | 系统层面只由 hub 打开真口 |
| RX 广播 | 所有可读客户端收到同一份设备数据 |
| TX 仲裁 | 默认按行排队；支持写锁租约 |
| Unix 虚拟口 | macOS/Linux：PTY + 稳定 symlink 路径 |
| Windows 接入 | TCP raw + WebSocket（真虚拟 COM 见跨平台策略） |
| Agent API | WebSocket 字节流 + HTTP 写命令/查状态 |
| 会话日志 | 文件 raw/hex、控制台 mirror |
| 断线重连 | 真口掉线可配置自动重开 |
| CLI + TOML 配置 | `ohmyserial run -c config.toml` |
| 跨平台构建 | Windows / Linux / macOS，x64 / aarch64 |

### 6.2 v1.x（差异化增强）

| 能力 | 说明 |
|------|------|
| 多实例 / 多真口 | 一份进程管多个 real port profile |
| 客户端权限 | `read_only` / `can_write` / 命名角色 |
| 写策略插件化 | exclusive / queue_line / queue_frame / primary_wins |
| 历史环形缓冲 | Agent 晚连也能拿到最近 N KB/秒 |
| RFC2217 | 网络侧串口控制信号 |
| Web 监视页 | 可选轻量 UI（非必须） |
| 录制回放 | 当假设备喂给上位机 |
| 指标 | 字节数、客户端数、锁持有者、错误计数 |

### 6.3 明确后置 / 不做（防膨胀）

| 项 | 原因 |
|----|------|
| 自研 Windows 内核虚拟 COM 驱动 | 成本高、签名难；v1 用 TCP/WS 或对接 com0com |
| 深度协议语义仲裁（Modbus 状态机级） | 垂直场景，插件后做 |
| 云账号 / 远程 SaaS | 偏离本地调试工具定位 |
| 重 GUI 安装套件 | CLI + 配置优先 |

---

## 7. 跨平台策略

### 7.1 支持矩阵（产品承诺）

| 平台 | 架构 | 真串口 | 类串口客户端 | Agent 客户端 |
|------|------|--------|--------------|--------------|
| macOS | arm64 / x64 | ✅ | PTY ✅ | TCP/WS/HTTP ✅ |
| Linux | x64 / arm64 / 其它 unix | ✅ | PTY ✅ | TCP/WS/HTTP ✅ |
| Windows | x64 / arm64 | ✅ | **见下** | TCP/WS/HTTP ✅ |

### 7.2 Windows「虚拟串口」现实策略

用户空间 **无法** 像 Unix 一样轻松造出系统级 COM 而不装驱动。

**三层策略：**

| 层级 | 做法 | 定位 |
|------|------|------|
| **L0 默认（跨平台统一）** | TCP raw + WebSocket + HTTP | 所有平台行为一致；Agent 首选 |
| **L1 Unix 原生** | PTY + symlink | 上位机直接选 `/tmp/ohmyserial-ui` |
| **L2 Windows 兼容** | 文档化对接 com0com/hub4com，或后续可选驱动/桥 | 传统只认 COM 的上位机 |

架构保证：

> **业务核心不依赖「虚拟 COM 实现」**；虚拟口只是 Client Adapter 之一。

Rust 核心一套，平台差异关在 adapter 里。

### 7.3 技术选型（Rust 栈）

| 层级 | 建议 crate / 技术 |
|------|-------------------|
| 运行时 | `tokio` |
| 真串口 | `serialport`（或 async 包装） |
| CLI | `clap` |
| 配置 | `serde` + `toml` |
| HTTP/WS | `axum` + WebSocket |
| Unix PTY | `nix` / `portable-pty` |
| 日志 | `tracing` + `tracing-subscriber` |
| 错误 | lib: `thiserror`；bin: `anyhow` |
| 跨平台 CI | GitHub Actions：win / linux / mac × 主流 arch |

---

## 8. 系统总架构

```text
                        ┌──────────────────────────────────────┐
                        │              CLI / Config            │
                        │   run | status | list-ports | ...    │
                        └──────────────────┬───────────────────┘
                                           │
                        ┌──────────────────▼───────────────────┐
                        │           Hub Supervisor             │
                        │  lifecycle, reload, multi-profile    │
                        └──────────────────┬───────────────────┘
                                           │
          ┌────────────────────────────────┼────────────────────────────────┐
          │                                │                                │
          ▼                                ▼                                ▼
 ┌─────────────────┐            ┌─────────────────────┐          ┌──────────────────┐
 │  Serial Core    │            │   Router / Broker   │          │  Observability   │
 │  open/close     │◄──────────►│  RX fan-out         │─────────►│  binlog / metrics│
 │  baud/flow      │  bytes in  │  TX arbitration     │  events  │  tracing         │
 │  reconnect      │───────────►│  write lock lease   │          └──────────────────┘
 │  DTR/RTS policy │  bytes out │  client registry    │
 └─────────────────┘            └──────────┬──────────┘
                                           │
                    ┌──────────────────────┼──────────────────────┐
                    │                      │                      │
                    ▼                      ▼                      ▼
            ┌──────────────┐       ┌──────────────┐       ┌──────────────┐
            │ PtyAdapter   │       │ TcpAdapter   │       │ Ws/Http API  │
            │ (unix)       │       │ raw stream   │       │ agent-first  │
            └──────────────┘       └──────────────┘       └──────────────┘
                    │                      │                      │
                    ▼                      ▼                      ▼
              传统上位机              通用客户端/脚本              Agent / IDE 插件
```

### 8.1 数据面（Data plane）

```text
Device RX ──► Serial Core ──► Broker.broadcast(rx) ──► all readable clients

Client TX ──► Broker.admit(tx_policy) ──► queue/lock ──► Serial Core ──► Device
```

### 8.2 控制面（Control plane）

- 打开/关闭 profile  
- 查询客户端列表、写锁持有者  
- 申请/释放写锁  
- 动态调整部分策略（可选）  
- 健康状态：`connected` / `reconnecting` / `error`  

---

## 9. 核心模块划分

```text
ohmyserial/
├── cli/                 # 命令行入口、子命令
├── config/              # TOML schema、校验、默认值
├── serial/              # 真串口生命周期、参数、重连
├── broker/              # RX fan-out、TX 策略、客户端注册
├── policy/              # exclusive | queue_line | queue_frame | primary
├── client/
│   ├── pty.rs           # macOS/Linux
│   ├── tcp.rs           # 全平台
│   ├── ws.rs            # 全平台
│   └── http_api.rs      # 控制 + 写命令
├── observe/             # 日志、hex dump、metrics
└── util/                # 平台宏、路径、shutdown
```

原则：**broker 与 platform adapter 解耦**；任何新接入方式只加 client adapter。

---

## 10. 关键语义

### 10.1 RX

- 从真串口读到的每个字节/块，**复制**给所有 `can_read` 客户端  
- 不保证「消息边界」（串口本身无消息）；可选按行 mirror 到日志  
- 某客户端阻塞/慢：**不得拖死真口读循环**（有界队列 + 可配丢弃/断开）

慢客户端策略：

| 策略 | 含义 |
|------|------|
| `drop_oldest`（默认） | 保真口实时性 |
| `disconnect_slow` | 严格模式 |
| `block` | 不推荐，仅调试 |

### 10.2 TX

| 模式 | 行为 | 适用 |
|------|------|------|
| `queue_by_line`（**默认**） | 按 `\n` 完整行串行写出 | 文本日志 / AT / CLI 设备 |
| `queue_by_frame` | 按分隔符/长度帧串行 | 二进制协议 |
| `exclusive` | 同时仅一个 writer；需持锁 | 危险命令、烧录期 |
| `primary_wins` | 指定 primary 抢占 | 人在环优先 |

**写锁租约（lease）**

- `request_write(client, ttl_ms)`  
- 到期自动释放  
- 持锁者崩溃/断开 → 释放  
- HTTP/WS 控制面可查询 `lock_owner`

**冲突结果必须可预期**：拒绝并返回错误，或排队；**禁止静默字节交错**。

### 10.3 串口参数与控制线

| 项 | 默认策略 |
|----|----------|
| baud/parity 等 | 以 Hub 配置为准 |
| 客户端尝试改波特率 | Unix PTY：可忽略或仅记录；不随意改真口 |
| DTR/RTS | 配置项：`pass_from_primary` / `fixed` / `ignore` |
| 真口断开 | 客户端连接保持，状态 `reconnecting`，可自动重开 |

Arduino 等依赖 DTR 复位的场景，须在用户文档中单独说明。

---

## 11. 客户端模型

### 11.1 统一 Client 抽象

```text
Client {
  id, name, kind,          // pty | tcp | ws | http-session
  can_read, can_write,
  created_at,
  tx_queue_depth,
  stats
}
```

### 11.2 接入形态

| Kind | 平台 | 给谁用 | 能力 |
|------|------|--------|------|
| `pty` | macOS/Linux | 传统串口上位机 | 读写字节流 |
| `tcp` | 全平台 | 脚本、通用工具 | 原始双向字节流 |
| `websocket` | 全平台 | Agent、前端 | 二进制帧 + 可选 JSON 控制 |
| `http` | 全平台 | Agent 控制面 | status / write / lock |

### 11.3 Agent 推荐路径（产品默认故事）

```text
上位机  → PTY（Unix）或 TCP（Windows）
Agent   → WebSocket 订阅 RX + HTTP POST 写命令（带写锁）
```

不要强迫 Agent 假装自己是 COM 口。

---

## 12. 配置模型

```toml
[real]
path = "/dev/tty.usbmodem14101"   # Windows: "COM3"
baud = 115200
databits = 8
parity = "none"
stopbits = 1
flow = "none"
reconnect = true
reconnect_ms = 1000

[tx]
mode = "queue_by_line"            # exclusive | queue_by_line | queue_by_frame | primary_wins
primary = "ui"
write_lock_ms = 3000
slow_client = "drop_oldest"

[[clients]]
name = "ui"
type = "pty"
link = "/tmp/ohmyserial-ui"
can_write = true

[[clients]]
name = "agent"
type = "websocket"
bind = "127.0.0.1:8787"
can_write = true
history_bytes = 65536

[log]
file = "logs/session.blog"
mirror_console = true
format = "hex+text"               # text | hex | hex+text
```

配置即契约：**同一份配置在三平台语义尽量一致**；`type = "pty"` 在 Windows 上应明确报错或给出降级说明。

---

## 13. API 草图（控制面）

| 接口 | 作用 |
|------|------|
| `GET /v1/status` | 真口状态、波特率、客户端、锁 |
| `GET /v1/clients` | 列表 |
| `POST /v1/write` | body=bytes/text，可选 `wait_lock` |
| `POST /v1/lock` | 申请写锁 |
| `DELETE /v1/lock` | 释放 |
| `WS /v1/stream` | 二进制 RX；可选带侧信道事件 |
| `GET /v1/health` | liveness |

安全默认：**只绑 `127.0.0.1`**；若绑 `0.0.0.0` 必须显式，并警告无鉴权风险（鉴权可后置）。

---

## 14. 可观测性

| 信号 | 用途 |
|------|------|
| 结构化 tracing 日志 | 排障 |
| session blog（带时间戳与方向 tag） | 复盘、喂给 Agent |
| 计数器 | `rx_bytes`, `tx_bytes`, `drops`, lock 争用 |
| 事件 | client_join/leave, reconnected, lock_granted/expired |

方向标记：`RX`（设备→主机）、`TX:<client_name>`（某客户端→设备）。

---

## 15. 进程与部署形态

| 形态 | 说明 |
|------|------|
| 前台 CLI | 开发调试默认 |
| 单二进制 | `ohmyserial` |
| 可选系统服务 | 后置：systemd / launchd / Windows Service |
| 多 profile | `ohmyserial run -c a.toml` 可多开进程；或单进程多 real（v1.x） |

资源目标：

- 空闲内存尽量小（MB 级）  
- RX 路径低拷贝、有界缓冲  
- 不引入重运行时（无浏览器内核、无 Electron）  

---

## 16. 威胁模型与安全边界

| 风险 | 对策 |
|------|------|
| 局域网任意人写串口 | 默认 localhost；后续 token |
| 两客户端命令互毁 | TX 策略 + 权限 + 写锁 |
| 慢客户端内存爆 | 有界队列 + drop 策略 |
| 日志含密钥/token | 文档警示；可选 redact 插件后置 |
| 路径错误打开错误设备 | 启动前 list + 确认；配置显式 path |

---

## 17. 版本路线图

### Phase 0 — 定义冻结（当前）

- 定位、架构、TX 默认策略、平台矩阵  
- 配置 schema、模块边界  

### Phase 1 — MVP 可跑

- Serial Core + Broker + TCP + WS/HTTP  
- Unix PTY  
- TOML + CLI  
- 基础 blog 日志  
- CI：三平台编译  

### Phase 2 — 好用

- 写锁 API、history buffer、权限  
- 重连与状态完善  
- Windows 上位机对接指南（TCP↔COM 桥）  
- 完整文档与示例（Arduino / 日志设备 / Agent）  

### Phase 3 — 丰富

- RFC2217、frame policy、多真口  
- 可选 Web monitor  
- 录制回放  
- 评估 Windows 虚拟 COM 方案（驱动 or 官方桥）  

---

## 18. 成功标准

1. **同一块板子**：人用上位机、Agent 用 WS，同时在线稳定。  
2. **TX 不出现静默交错**（策略生效，错误可解释）。  
3. **macOS / Linux / Windows 官方支持**，arch 覆盖桌面主流。  
4. **5 分钟内**：装二进制 → 改配置 → 跑起来 → 两边都看到 RX。  
5. **许可证清晰、无付费墙**，文档足够让人不用读源码。  

---

## 19. 命名与品牌

| 项 | 建议 |
|----|------|
| 仓库/项目 | `oh_myserial` / `ohmyserial` |
| 二进制 | `ohmyserial` |
| 角色隐喻 | Hub / Broker / Switch（避免叫 Driver，以免被当成内核驱动） |
| Tagline | One port. Many clients. Zero fights. |

---

## 20. 架构决策记录（ADR 摘要）

| ID | 决策 | 理由 |
|----|------|------|
| ADR-1 | Rust 单二进制 | 跨平台、性能、分发简单 |
| ADR-2 | 核心与虚拟串口解耦 | Windows 无 PTY 也能完整 Agent 工作流 |
| ADR-3 | 默认 `queue_by_line` | 文本调试最常见，避免字节交错 |
| ADR-4 | Agent 走 WS/HTTP 一等公民 | 比强行虚拟 COM 更稳、更好自动化 |
| ADR-5 | 默认 localhost | 串口写入等价于碰硬件 |
| ADR-6 | Windows 真 COM 虚拟后置 | 先交付价值，驱动成本不挡 MVP |
| ADR-7 | 开源免费（MIT/Apache） | 对准商业闭源空白 |

---

## 21. 总结表

| 维度 | 定义 |
|------|------|
| **是什么** | 跨平台开源串口共享中枢 |
| **不是什么** | 商业 VSPD 克隆 / 协议分析 IDE |
| **核心矛盾解法** | 独占真口 + RX 广播 + TX 仲裁 |
| **人怎么接** | PTY（Unix）/ TCP（通用） |
| **Agent 怎么接** | WebSocket 流 + HTTP 控制 |
| **平台** | Windows · Linux · macOS（x64/arm64） |
| **默认 TX** | 按行排队 + 可选写锁 |
| **开源** | 免费，许可证待选定 MIT/Apache |
| **空白点** | 人+Agent 共调试的完整产品，而非零件 |

---

## 22. 待拍板事项

实现前建议明确：

1. **许可证**：MIT vs Apache-2.0  
2. **TX 默认**：是否锁定 `queue_by_line`  
3. **Windows 上位机**：v1 是否接受「TCP 为主、COM 靠桥」，还是必须原生 COM  
4. **二进制名 / 配置名**最终定名  
5. **MVP 范围**：第一版 `1 real + 1 pty + 1 ws`，或 `tcp + ws + pty` 全上  

---

*文档版本：v0.1（定位冻结稿）*
