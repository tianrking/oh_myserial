# ohmyserial Web 控制台（繁體中文）

本目錄為 **React + Vite + TypeScript** 前端，連線本機 **ohmyserial hub** 的 HTTP / WebSocket。

## 功能清單（完整）

| 功能 | 狀態 |
|------|------|
| 設定 host/port，localStorage 記住 | ✅ |
| `GET /v1/health` 探活 | ✅ |
| `GET /v1/status` 狀態 / 寫鎖 / 統計 | ✅ |
| `GET /v1/endpoints` 並聯端點列表 + 複製 | ✅ |
| 客戶端列表 | ✅ |
| `WS /v1/stream` 即時日誌（文字 + hex） | ✅ |
| `POST /v1/write` 文字 / hex | ✅ |
| 寫鎖取得 / 釋放 | ✅ |
| 協定說明分頁 | ✅ |
| 完整協定文件 | ✅ `PROTOCOL.zh-TW.md` |

## 推薦用法：嵌在 hub 裡（同源，最穩）

先建置前端，再編譯 Rust（會把 `web/dist` 嵌進二進位）：

```bash
cd web && npm install && npm run build && cd ..
cargo build --release

# 一鍵共享並開啟瀏覽器控制台
./target/release/ohmyserial share mock:demo --ui
# 或真實裝置
./target/release/ohmyserial share /dev/cu.usbmodemXXXX --pty 2 --ui
```

瀏覽器開啟：**http://127.0.0.1:8787/**（與 API/WS 同源，無混合內容問題）。  
點「連線」即可（預設已填同一 host/port）。

## 本機熱重載開發（可選）

```bash
# 終端 1：hub
cargo run --release -- share mock:demo --pty 2

# 終端 2：Vite
cd web && npm run dev
```

通常 `http://localhost:5173`，網頁會連 `127.0.0.1:8787`。

## 部署到 Vercel（可選鏡像）

```bash
cd web
npm run build
# 使用 vercel.json；Root Directory 選 web
vercel --prod
```

> **注意**：HTTPS 的 Vercel 頁連 `ws://127.0.0.1` 可能被瀏覽器封鎖。  
> **實務請優先用本機** `http://127.0.0.1:8787/`。詳見 `PROTOCOL.zh-TW.md` §2.4。

## 目錄

```text
web/
├── PROTOCOL.zh-TW.md   # HTTP/WS 協定完整說明（繁中）
├── README.zh-TW.md     # 本說明
├── src/
│   ├── api/            # 型別、HTTP client、WS 封裝
│   ├── App.tsx         # 控制台 UI
│   └── ...
└── package.json
```

## 與 CLI 關係

| 方式 | 用途 |
|------|------|
| `ohmyserial share ...` | 本機高效能 hub（必須） |
| 本 React 頁 | 可選人機監控 / 除錯面板 |
| Agent | 同一套 WS/HTTP 協定 |
