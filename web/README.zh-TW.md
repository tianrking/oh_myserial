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

## 本機開發

```bash
# 終端 1：啟動 hub
cd ..
cargo run --release -- share mock:demo --pty 2

# 終端 2：啟動網頁
cd web
npm install
npm run dev
```

瀏覽器開啟 Vite 提示的位址（通常 `http://localhost:5173`），點「連線」。

## 建置靜態站（可部署 Vercel）

```bash
cd web
npm run build
# 產物在 dist/
```

> **注意**：若部署在 **HTTPS**（Vercel），瀏覽器可能封鎖連到 `ws://127.0.0.1`。  
> 開發與實務操控建議用 `http://localhost` 開啟本頁。詳見 `PROTOCOL.zh-TW.md` §2.4。

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
