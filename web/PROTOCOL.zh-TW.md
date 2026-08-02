# ohmyserial 本機協定說明（繁體中文）

本文件描述 **ohmyserial hub** 對外提供的 **HTTP REST** 與 **WebSocket** 協定。  
React 控制台（`web/`）與 AI Agent 皆應遵守此規格。

> 預設位址：`http://127.0.0.1:8787` · WebSocket：`ws://127.0.0.1:8787/v1/stream`  
> hub 的明文 API / WebSocket / 原始 TCP 僅允許 loopback。Bearer 可作額外防護，但不會讓網路上的明文安全；遠端存取請使用 SSH tunnel 或 TLS reverse proxy。

---

## 1. 整體架構

```text
┌──────────────┐   HTTP JSON    ┌─────────────────────┐
│  React 網頁  │ ─────────────► │  ohmyserial hub     │
│  / Agent     │ ◄──── WS 位元組 │  (獨佔真實串口)      │
└──────────────┘                └──────────┬──────────┘
                                           │
                                    真實 UART / mock
```

| 通道 | 用途 |
|------|------|
| HTTP | 健康檢查、狀態、端點清單、寫入、寫鎖 |
| WebSocket | 即時 RX 串流；亦可 TX 文字/二進位 |
| TCP / PTY | 給傳統工具（不在本網頁協定內，見 hub 設定） |

---

## 2. 連線與探索

### 2.1 預設埠

| 項目 | 預設 |
|------|------|
| HTTP | `http://127.0.0.1:8787` |
| WebSocket | `ws://127.0.0.1:8787/v1/stream` |

使用者可在網頁修改 host/port。瀏覽器**無法自動掃描**本機服務，只能嘗試約定埠或手動填寫。

### 2.2 建議連線流程

1. 本機執行：`ohmyserial share <裝置> --pty 2`（或 `mock:demo`）  
2. 開啟 React 頁（`npm run dev` 或部署後的靜態站）  
3. 點「連線」→ `GET /v1/health`  
4. 成功後訂閱 `WS /v1/stream`，並輪詢 `/v1/status`、`/v1/endpoints`  

### 2.3 CORS

未設定 `api.cors_origins` 時不加 CORS header，瀏覽器只能同源呼叫。設定時只接受完整、精確的 Origin（例如 `https://console.example.com`），`*`、含路徑、query 或 fragment 的值會在啟動時被拒絕。

WebSocket upgrade 另外檢查 `Origin`：瀏覽器來源必須與 hub 的 `Host` 相同，或列於 `cors_origins`。非瀏覽器 Agent 通常不帶 `Origin`，可通過這項檢查，但啟用 Bearer 後仍必須通過鑑權。

### 2.4 Bearer 鑑權

`api.token_env` 填的是**環境變數名稱**，密鑰只從該環境變數讀取，不得寫入 TOML、URL 或日誌。明文 API 與獨立 WebSocket bind 即使有 token 也只能使用 loopback；非回環設定會拒絕啟動。

- `/v1/health` 保持公開，方便 supervisor 探活。
- 其他 `/v1/*` HTTP：`Authorization: Bearer <token>`。
- 瀏覽器 WebSocket 無法設定 Authorization header，使用 `new WebSocket(url, ["bearer", token])`；伺服器只回選不含密鑰的 `bearer` subprotocol。
- 不支援 query-string token。

API Bearer 用於存取 hub；下文的 `lease_token` 用於獨占 TX，兩者是不同憑證。

### 2.5 HTTPS 頁面連本機

若網頁託管於 **HTTPS**（如 Vercel），連 `ws://127.0.0.1` 可能被瀏覽器混合內容策略阻擋。  
**建議**：開發用 `http://localhost:5173`；正式操控優先使用本機 HTTP 頁，或接受使用者手動授權。

---

## 3. HTTP API

所有 JSON 請求：`Content-Type: application/json`（有 body 時）。啟用 API Bearer 時，除 health 外另加 `Authorization` header。

### 3.1 `GET /v1/health`

**用途**：探活、確認 hub 是否在跑。

**回應示例：**

```json
{ "ok": true, "service": "ohmyserial" }
```

---

### 3.2 `GET /v1/status`

**用途**：真實埠狀態、TX 模式、寫鎖、已連線客戶端、統計、端點摘要。

**回應欄位（摘要）：**

| 欄位 | 說明 |
|------|------|
| `port.path` | 真實裝置路徑 |
| `port.baud` | 鮑率 |
| `port.connected` | 是否已開啟 |
| `port.epoch` | 連線世代；每次斷線後重新連線會遞增 |
| `port.detail` | 狀態說明 |
| `tx_mode` | 如 `queue_by_line` |
| `lock_owner` | 寫鎖持有者或 null |
| `lock_expires_ms` | 剩餘毫秒 |
| `endpoints[]` | 並聯端點（PTY/TCP/WS/HTTP） |
| `clients[]` | 目前已連上的 fan-out 客戶端 |
| `stats.rx_bytes` / `tx_bytes` / `rx_drops` / `tx_denies` | 計數 |

---

### 3.3 `GET /v1/endpoints`

**用途**：專門列出「一個真串口並聯出的所有端點」。

**回應示例：**

```json
{
  "real": { "path": "mock:demo", "baud": 115200, "connected": true, "detail": "mock loopback" },
  "endpoints": [
    {
      "kind": "http",
      "name": "api",
      "address": "http://127.0.0.1:8787",
      "can_read": true,
      "can_write": true,
      "note": "..."
    },
    {
      "kind": "pty",
      "name": "v0",
      "address": "/tmp/ohmyserial-v0",
      "can_read": true,
      "can_write": true,
      "note": "..."
    }
  ],
  "connected_clients": 2
}
```

`kind` 常見值：`http` · `websocket` · `tcp` · `pty`

---

### 3.4 `GET /v1/clients`

**用途**：目前已註冊的 fan-out 客戶端列表（含每個 WS 連線）。

```json
[
  { "id": "uuid", "name": "ws-...", "kind": "websocket", "can_read": true, "can_write": true }
]
```

---

### 3.5 `POST /v1/write`

**用途**：向真實串口發送資料（經 TX 策略 / 寫鎖）。

**請求 body：**

```json
{
  "text": "AT",
  "newline": true,
  "as_client": "web-ui",
  "lease_token": "opaque-random-token-if-a-lease-is-active"
}
```

或：

```json
{
  "hex": "41 54 0d 0a",
  "as_client": "web-ui",
  "lease_token": "opaque-random-token-if-a-lease-is-active"
}
```

| 欄位 | 必填 | 說明 |
|------|------|------|
| `text` | 與 hex 二選一 | UTF-8 文字 |
| `hex` | 與 text 二選一 | 十六進位（可含空白）；**優先於 text** |
| `newline` | 否，預設 true | 對 text：若無 `\n` 則自動補上 |
| `as_client` | 否 | 顯示/審計名稱；預設 `api`，**不是授權憑證** |
| `lease_token` | 有租約時必填 | `POST /v1/lock` 回傳的不透明 TX Bearer |

**成功：**

```json
{ "ok": true, "bytes": 3 }
```

**失敗：**

```json
{ "ok": false, "error": "write lock held by 'agent'", "bytes": 0 }
```

**注意（TX 策略）：**

- HTTP text/hex 是一次**原子寫入**，不經 delimiter assembler；大小受 `tx.max_write_bytes` 限制。
- `ok: true` 代表串口所有者已完成主機側 `write_all` + `flush`，不是設備協定 ACK。
- 入隊與主機寫入確認共用 `tx.write_timeout_ms` 截止時間；若錯誤表示結果可能 partial/unknown，可能已有部分或全部位元組送到驅動，**不可盲目重試**。
- `exclusive` 模式必須先持有寫鎖。  
- 多客戶端同時寫：由 hub 仲裁，**不會靜默交錯位元組**。

---

### 3.6 `POST /v1/workflows/run`

這是受限的線性 Agent 工作流，不是腳本引擎。步驟只有 `lease`、`send`、`expect`、`assert`、`wait` 與保留的 `control`；沒有迴圈、分支、重試、變數、網路或檔案操作。完整 DSL、上限與證據游標見 [`WORKFLOWS.md`](../WORKFLOWS.md)。

```json
{
  "request_id": "probe-001",
  "lease_token": "optional-opaque-token",
  "workflow": {
    "id": "identify",
    "steps": [
      { "op": "lease" },
      { "op": "send", "bytes": { "text": "ATI\r\n" } },
      { "op": "expect", "pattern": { "text": "OK" }, "timeout_ms": 2000 }
    ]
  }
}
```

`request_id` 完成後重送會得到相同結果；並行重送會被拒絕，不會執行第二次寫入。服務器生成 `workflow:<uuid>` actor，租約 token 不會出現在回應或事件賬本。`expect` 在 canonical RX 分片之間增量匹配；RX observation gap、游標遺失、斷線與 epoch 變更會 fail-closed，`client_delivery` gap 不會誤判為裝置 RX 遺失。`control` 目前只保留 schema，直到 serial-owner command channel 完成前會回傳 unavailable。

---

### 3.7 `POST /v1/control`

控制線操作需要 API 的 `can_control = true`，且必須攜帶有效的
`lease_token`。`dtr` / `rts` 使用布林 `level`；`break` 使用
`duration_ms`（1 到 1000）。命令由唯一的 serial owner 執行並等待 OS
驅動回覆；mock 模式會明確拒絕物理控制線，硬體 flow control 啟用時也拒絕 RTS。

```json
{ "op": "dtr", "level": true, "lease_token": "opaque-token" }
```

### 3.8 `POST /v1/lock`

**用途**：申請寫鎖租約。

```json
{ "as_client": "web-ui" }
```

**成功：**

```json
{
  "ok": true,
  "lock": {
    "owner": "web-ui",
    "expires_ms": 3000,
    "lease_token": "random-opaque-token"
  }
}
```

`owner` 僅供顯示/審計；真正授權的是隨機 `lease_token`，同名客戶端不能冒充。token 只在申請/續租回應中出現，不會出現在 `/v1/status`，也不應寫入磁碟或日誌。

在 TTL 到期前續租同一把鎖：

```json
{ "lease_token": "random-opaque-token" }
```

成功續租會回傳相同 token 與新的 `expires_ms`。

---

### 3.9 `DELETE /v1/lock`

**用途**：釋放寫鎖。存在有效租約時必須提交它的 token：

```json
{ "lease_token": "random-opaque-token" }
```

```json
{ "ok": true }
```

租約只會因 TTL 到期或持 token 主動釋放而結束。HTTP / WS / TCP / PTY 連線斷開、或同名客戶端註銷，均**不會**釋放租約。

---

## 4. WebSocket 協定 `WS /v1/stream`

### 4.1 連線

```text
ws://127.0.0.1:8787/v1/stream
```

- 每個連線 = 一個獨立 fan-out 客戶端；名稱由伺服器端 endpoint 配置決定，連線 UUID 僅用於內部 kind/診斷。
- 可同時開多條 WS，皆收到同一份裝置 RX。  
- 啟用 Bearer 時，瀏覽器以 `new WebSocket(url, ["bearer", token])` 連線；不要把 token 放在 URL。

### 4.2 伺服器 → 客戶端（RX）

| 訊框類型 | 內容 |
|----------|------|
| **Binary** | 裝置收到的原始位元組（主路徑） |
| Binary（連線後可能首包） | 歷史緩衝區（若 hub 設定 `history_bytes` > 0） |

網頁應：

1. 以 `ArrayBuffer` / `Blob` 接收  
2. 同時顯示 **文字（UTF-8 lossy）** 與 **hex**  
3. 時間戳記本地加上即可（協定本身不帶時間戳）

### 4.3 客戶端 → 伺服器（TX）

| 訊框類型 | hub 行為 |
|----------|----------|
| **Text** | 視為 UTF-8；若無結尾 `\n` 則補上，再走 delimiter 組幀；單幀受 `max_frame_bytes` 限制 |
| **Binary** | 整個訊框視為一次原子 TX，不經 delimiter 組幀；受 `max_write_bytes` 限制 |

寫入仍受 **can_write / 寫租約 / exclusive** 限制；拒絕時伺服器回傳 JSON Text 訊框：

```json
{ "type": "ohmyserial.error", "ok": false, "error": "..." }
```

WS TX 成功入隊沒有主機寫入 ACK。需要 `write_all` + `flush` 結果、或需攜帶 `lease_token` 時，請用 **`POST /v1/write`**。

### 4.4 心跳

瀏覽器 WebSocket 可能發送 Ping；hub 會忽略應用層控制，連線保活依 TCP。  
網頁可用定時 `GET /v1/health` 檢測 hub 是否仍在。

### 4.5 關閉

關閉 WS 會註銷 fan-out 客戶端並關閉它的有界 RX queue，但不會按名稱釋放租約。

---

## 5. 網頁功能對應表（React TODO / 已實作清單）

| 功能 | 協定 | 網頁行為 |
|------|------|----------|
| 設定 hub 位址 | — | 輸入 host/port，localStorage 記住 |
| 探活連線 | `GET /v1/health` | 顯示連線狀態燈 |
| 埠與統計 | `GET /v1/status` | 定時刷新 |
| 並聯端點 | `GET /v1/endpoints` | 列表 + 一鍵複製 |
| 連線客戶端 | `GET /v1/clients` 或 status | 表格 |
| 即時日誌 | `WS /v1/stream` | 滾動日誌、暫停、清空 |
| 送文字 | `POST /v1/write` text | 輸入框 + 可選補行 |
| 送 hex | `POST /v1/write` hex | hex 輸入 |
| 寫鎖 | `POST/DELETE /v1/lock` | 按鈕 |
| 協定說明 | 本文件 | 頁內「協定」分頁 |

---

## 6. 錯誤與限制（實作者必讀）

1. **無訊息邊界**：串口為位元組流；`\n` 只是預設 TX 組幀策略。  
2. **慢客戶端**：每個讀端都有 `client_queue` 有界 queue。`drop_oldest` 丟最舊待處理塊；`drop_newest` 丟新塊；`disconnect_slow` 立即斷線；`block` 最多等 `slow_block_ms`，逾時亦斷線。
3. **混合內容**：HTTPS 站連本機 `ws://` 可能失敗。  
4. **發現**：無 mDNS；請固定埠或手動設定。  
5. **安全**：能寫串口 ≈ 能碰硬體。API 使用 Bearer + 權限；原始 TCP 沒有此鑑權，請只綁回環並用 `ssh -L 8788:127.0.0.1:8788 user@host` 遠端轉發。
6. **重連隔離**：寫入帶連線 epoch，串口所有者在真正寫入前再驗證；斷線/重連後舊 queue 資料會被拒絕，不會回放至新連線。
7. **生命週期**：配置或 listener bind 失敗會讓啟動整體失敗並撤銷已啟動 task；正常 shutdown 會停止串口所有者、關閉 fan-out、拒絕/排空待寫資料。
8. **mock 邊界**：`mock:demo` 可驗證 hub、策略、租約與 API，但不能證明 OS 串口 driver、USB/UART 時序、控制線或真實設備 ACK 正確。

---

## 7. 快速 curl 對照

```bash
curl -s http://127.0.0.1:8787/v1/health
curl -s http://127.0.0.1:8787/v1/status
curl -s http://127.0.0.1:8787/v1/endpoints
curl -s -X POST http://127.0.0.1:8787/v1/write \
  -H 'content-type: application/json' \
  -d '{"text":"hello","newline":true,"as_client":"web-ui"}'
curl -s -X POST http://127.0.0.1:8787/v1/lock \
  -H 'content-type: application/json' \
  -d '{"as_client":"web-ui"}'
# 從上一步回應保存 .lock.lease_token，再帶 token 寫入、續租或釋放。
curl -s -X DELETE http://127.0.0.1:8787/v1/lock \
  -H 'content-type: application/json' \
  -d '{"lease_token":"<saved-token>"}'
```

---

*版本對應：ohmyserial hub MVP + fan-out · 與 `web/` React 控制台同步*
