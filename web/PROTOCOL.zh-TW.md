# ohmyserial 本機協定說明（繁體中文）

本文件描述 **ohmyserial hub** 對外提供的 **HTTP REST** 與 **WebSocket** 協定。  
React 控制台（`web/`）與 AI Agent 皆應遵守此規格。

> 預設位址：`http://127.0.0.1:8787` · WebSocket：`ws://127.0.0.1:8787/v1/stream`  
> hub 僅綁定本機時，資料不會出網；請勿在未授權網路暴露 `0.0.0.0`。

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

hub 目前允許任意 Origin（方便本機 Vite / 靜態站）。生產環境若暴露外網，應改為白名單。

### 2.4 HTTPS 頁面連本機

若網頁託管於 **HTTPS**（如 Vercel），連 `ws://127.0.0.1` 可能被瀏覽器混合內容策略阻擋。  
**建議**：開發用 `http://localhost:5173`；正式操控優先使用本機 HTTP 頁，或接受使用者手動授權。

---

## 3. HTTP API

所有 JSON 請求：`Content-Type: application/json`（有 body 時）。

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
  "as_client": "web-ui"
}
```

或：

```json
{
  "hex": "41 54 0d 0a",
  "as_client": "web-ui"
}
```

| 欄位 | 必填 | 說明 |
|------|------|------|
| `text` | 與 hex 二選一 | UTF-8 文字 |
| `hex` | 與 text 二選一 | 十六進位（可含空白）；**優先於 text** |
| `newline` | 否，預設 true | 對 text：若無 `\n` 則自動補上 |
| `as_client` | 否 | 身分名稱，用於鎖與審計；預設 `api` |

**成功：**

```json
{ "ok": true, "bytes": 3 }
```

**失敗：**

```json
{ "ok": false, "error": "write lock held by 'agent'", "bytes": 0 }
```

**注意（TX 策略）：**

- 預設 `queue_by_line`：未遇到 `\n` 前可能暫存不立刻下發（HTTP 若已補 newline 則整行送出）。  
- `exclusive` 模式必須先持有寫鎖。  
- 多客戶端同時寫：由 hub 仲裁，**不會靜默交錯位元組**。

---

### 3.6 `POST /v1/lock`

**用途**：申請寫鎖租約。

```json
{ "as_client": "web-ui" }
```

**成功：**

```json
{ "ok": true, "lock": { "owner": "web-ui", "expires_ms": 3000 } }
```

---

### 3.7 `DELETE /v1/lock`

**用途**：釋放寫鎖。Body 可選：

```json
{ "as_client": "web-ui" }
```

```json
{ "ok": true }
```

---

## 4. WebSocket 協定 `WS /v1/stream`

### 4.1 連線

```text
ws://127.0.0.1:8787/v1/stream
```

- 每個連線 = 一個獨立 fan-out 客戶端（名稱類似 `ws-<uuid>`）。  
- 可同時開多條 WS，皆收到同一份裝置 RX。  

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
| **Text** | 視為 UTF-8；若無結尾 `\n` 則自動補 `\n`，再走 TX 策略 |
| **Binary** | 原樣子節送入 TX 策略（`queue_by_line` 時仍可能按 `\n` 組幀） |

寫入仍受 **can_write / 寫鎖 / exclusive** 限制；被拒時目前 **不回 JSON 錯誤幀**（僅 hub 日誌）。  
可靠寫入請用 **`POST /v1/write`**（有明確 `ok/error`）。

### 4.4 心跳

瀏覽器 WebSocket 可能發送 Ping；hub 會忽略應用層控制，連線保活依 TCP。  
網頁可用定時 `GET /v1/health` 檢測 hub 是否仍在。

### 4.5 關閉

關閉 WS 即註銷客戶端；若持有寫鎖且 owner 為該客戶端名稱，鎖會釋放。  
（HTTP 申請的鎖 owner 為 `as_client` 字串，與 WS 隨機名不同。）

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
2. **慢客戶端**：預設 `drop_oldest`，網頁卡頓可能丟中間 RX。  
3. **混合內容**：HTTPS 站連本機 `ws://` 可能失敗。  
4. **發現**：無 mDNS；請固定埠或手動設定。  
5. **安全**：能寫串口 ≈ 能碰硬體；僅信任本機 UI。  

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
curl -s -X DELETE http://127.0.0.1:8787/v1/lock \
  -H 'content-type: application/json' \
  -d '{"as_client":"web-ui"}'
```

---

*版本對應：ohmyserial hub MVP + fan-out · 與 `web/` React 控制台同步*
