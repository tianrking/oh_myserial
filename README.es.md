# ohmyserial

<p align="center">
  <img alt="ohmyserial" src="https://img.shields.io/badge/ohmyserial-hub%20serie-0ea5e9?style=for-the-badge&logo=rust&logoColor=white" />
</p>

<p align="center">
  <strong>Hub serie multiplataforma y open source para humanos y agentes</strong><br/>
  <em>Un UART real · Muchos clientes seguros · Sin intercalado silencioso de TX</em>
</p>

<p align="center">
  <a href="./README.md"><img alt="English" src="https://img.shields.io/badge/lang-English-blue?style=flat-square" /></a>
  <a href="./README.zh-CN.md"><img alt="简体中文" src="https://img.shields.io/badge/lang-简体中文-red?style=flat-square" /></a>
  <a href="./README.es.md"><img alt="Español" src="https://img.shields.io/badge/lang-Español-green?style=flat-square" /></a>
</p>

<p align="center">
  <b>Idiomas / Languages:</b>
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
  <img alt="hub" src="https://img.shields.io/badge/hub-mux%20%2F%20compartir-8b5cf6?style=flat-square" />
  <img alt="websocket" src="https://img.shields.io/badge/API-HTTP%20%2B%20WebSocket-06b6d4?style=flat-square" />
  <img alt="agent" src="https://img.shields.io/badge/AI-agent%20friendly-f59e0b?style=flat-square" />
  <img alt="embedded" src="https://img.shields.io/badge/dominio-embedded%20debug-64748b?style=flat-square" />
  <img alt="tokio" src="https://img.shields.io/badge/async-tokio-c026d3?style=flat-square" />
  <img alt="axum" src="https://img.shields.io/badge/web-axum-7c3aed?style=flat-square" />
  <img alt="pty" src="https://img.shields.io/badge/Unix-PTY-14b8a6?style=flat-square" />
  <img alt="tcp" src="https://img.shields.io/badge/stream-TCP-3b82f6?style=flat-square" />
  <img alt="toml" src="https://img.shields.io/badge/config-TOML-e11d48?style=flat-square" />
</p>

---

## Tabla de contenidos

- [¿Qué es ohmyserial?](#qué-es-ohmyserial)
- [Problema y solución](#problema-y-solución)
- [Funciones](#funciones)
- [Cómo funciona](#cómo-funciona)
- [Soporte de plataformas](#soporte-de-plataformas)
- [Instalación y compilación](#instalación-y-compilación)
- [Inicio rápido](#inicio-rápido)
- [Consola web serie](#consola-web-serie)
- [Cómo usarlo (escenarios)](#cómo-usarlo-escenarios)
- [Configuración](#configuración)
- [CLI](#cli)
- [API HTTP y WebSocket](#api-http-y-websocket)
- [Políticas de TX](#políticas-de-tx)
- [PTY en Unix](#pty-en-unix-macos--linux)
- [Notas para Windows](#notas-para-windows)
- [Seguridad](#seguridad)
- [Estructura del proyecto](#estructura-del-proyecto)
- [Desarrollo](#desarrollo)
- [Hoja de ruta](#hoja-de-ruta)
- [FAQ](#faq)
- [Contribuir](#contribuir)
- [Licencia](#licencia)
- [Etiquetas técnicas](#etiquetas-técnicas)

---

## ¿Qué es ohmyserial?

**ohmyserial** es un **hub de puerto serie** pequeño y open source escrito en Rust.

Hace lo siguiente:

1. Abre el **dispositivo serie real** una sola vez (propiedad exclusiva)
2. **Replica el RX** (dispositivo → host) a muchos clientes
3. **Arbitra el TX** (host → dispositivo) para que varios escritores no corrompan tramas en silencio
4. Expone clientes por **TCP**, **HTTP/WebSocket** (pensado para agentes) y **PTY** (herramientas clásicas en macOS/Linux)

Ideal para depuración embebida cuando **un terminal humano y un agente/script de IA deben compartir el mismo UART**.

| Concepto | Valor |
|----------|--------|
| Binario | `ohmyserial` |
| Repositorio | [github.com/tianrking/oh_myserial](https://github.com/tianrking/oh_myserial) |
| Lenguaje | Rust (edition 2021) |
| Licencia | MIT |
| Documentación | [English](./README.md) (por defecto) · [中文](./README.zh-CN.md) · **Español** |
| Arquitectura | [`POSITIONING.md`](./POSITIONING.md) |

---

## Problema y solución

### El problema

| Objetivo | Realidad |
|----------|----------|
| Dejar abierto el monitor serie | El puerto ya está ocupado |
| Que un agente lea el mismo log | El segundo proceso no puede abrir el puerto |
| Que ambos envíen comandos | Los bytes se intercalan y el protocolo se rompe |

### La solución

```text
Dispositivo (UART/COM)
        │
        ▼
   ┌──────────┐
   │ ohmyserial│  ← único proceso que abre el puerto real
   └────┬─────┘
        │
   ┌────┴─────────────────────────────┐
   ▼                ▼                 ▼
  PTY            flujo TCP       HTTP + WebSocket
 (UI host)       (scripts)          (agentes)
```

---

## Funciones

### Funciones de producto

| Función | Descripción | Estado |
|---------|-------------|--------|
| Puerto real exclusivo | Un solo dueño del UART hardware | ✅ |
| Parámetros del puerto | baud, bits, paridad, stop, flow | ✅ |
| Difusión RX | Todos los clientes de lectura reciben datos | ✅ |
| Arbitraje TX | Cola por línea/trama, exclusivo, primary | ✅ |
| Bloqueo de escritura | Propiedad temporal de TX | ✅ |
| Reconexión | Reapertura opcional al desconectar | ✅ |
| Cliente TCP | Flujo de bytes bidireccional | ✅ |
| API HTTP | health / status / endpoints / events / workflows / write / control / handoff / lock | ✅ |
| WebSocket | RX en vivo (+ historial opcional) | ✅ |
| PTY Unix | Serie virtual con symlink | ✅ (macOS/Linux) |
| Log de sesión | Consola + archivo; text/hex | ✅ |
| Consola web serie | Perfiles, finales de línea, comandos rápidos, temporizador, checksums, protocolos, onda y líneas de control | ✅ |
| Event ledger y replay seguro | Evidencia RX/TX/connection/control/gap con ring acotado y replay verificado | ✅ |
| Puerto mock | `mock:demo` sin hardware | ✅ |
| TOML + CLI | `share` / `run` / `init` / `list-ports` / `status` / `supervise` | ✅ |
| Multi-puerto en un proceso | Varios perfiles reales con detección de colisiones | ✅ |
| RFC2217 | Control serie por red | 🔜 |
| COM virtual nativo en Windows | Nivel driver | 🔜 / puente externo |

### Características técnicas

| Área | Stack |
|------|--------|
| Runtime | Tokio |
| HTTP/WS | Axum |
| Serie | `serialport` + hilo de lectura |
| Config | Serde + TOML |
| Logs | `tracing` + blog de sesión |
| PTY Unix | `nix` openpty |
| Tests | Unitarios + integración (mock) |
| CI | Ubuntu · macOS · Windows |

---

## Cómo funciona

### Plano de datos

```text
Dispositivo ──RX──► Serial Core ──► Broker.broadcast ──► clientes
Cliente ──TX──► Broker.admit(política/lock) ──► Serial Core ──► Dispositivo
```

### Plano de control

- `GET /v1/status` — estado, baud, clientes, dueño del lock, contadores  
- `POST /v1/lock` / `DELETE /v1/lock` — arriendo de escritura  
- `POST /v1/write` — inyectar TX como cliente con nombre  

### Módulos

```text
CLI / Config
    └── Supervisor Hub
            ├── Núcleo serie (open, reconnect, mock)
            ├── Broker (registro, fan-out, cola TX)
            ├── Policy (queue_by_line / exclusive / …)
            ├── Clientes: PTY · TCP · HTTP/WS
            └── Observe (log de sesión, tracing)
```

---

## Soporte de plataformas

| Capacidad | macOS | Linux / Ubuntu | Windows |
|-----------|:-----:|:--------------:|:-------:|
| Serie real | ✅ | ✅ | ✅ |
| TCP | ✅ | ✅ | ✅ |
| HTTP + WebSocket | ✅ | ✅ | ✅ |
| Serie virtual PTY | ✅ | ✅ | — |
| Mock | ✅ | ✅ | ✅ |

**Ubuntu:** instala `build-essential pkg-config libudev-dev` antes de compilar.  
**Windows:** apps que solo listan COM deben usar TCP/WS o un puente externo; PTY es solo Unix.

---

## Instalación y compilación

### Requisitos

- [Rust](https://rustup.rs/) stable  
- **Ubuntu/Debian:**

  ```bash
  sudo apt update
  sudo apt install -y build-essential pkg-config libudev-dev
  ```

### Compilar desde el código

```bash
git clone https://github.com/tianrking/oh_myserial.git
cd oh_myserial
cargo build --release
```

| SO | Binario |
|----|---------|
| Unix | `./target/release/ohmyserial` |
| Windows | `.\target\release\ohmyserial.exe` |

```bash
cargo test
./target/release/ohmyserial --help
```

---

## Inicio rápido (lo más fácil)

**Sin archivo de configuración:**

```bash
./target/release/ohmyserial list-ports
./target/release/ohmyserial share /dev/cu.usbmodem14101 --baud 115200
./target/release/ohmyserial share /dev/ttyUSB0 --pty 3
./target/release/ohmyserial share COM3 --tcp 2          # Windows
./target/release/ohmyserial share mock:demo
```

Por defecto en macOS/Linux: **2 series virtuales (PTY)** + TCP + WebSocket.  
Al arrancar imprime una **tarjeta de conexión** (SERIAL / TCP / WS).

También puedes fijar los parámetros completos al iniciar: `--baud`,
`--data-bits`, `--parity`, `--stop-bits` y `--flow-control` (`none`,
`software` o `hardware`). `run` acepta los mismos flags como overrides
temporales sin modificar el TOML.

---

## Consola web serie

`share --ui` o `run --ui` sirve la consola embebida en la misma dirección
que la API, normalmente **http://127.0.0.1:8787/**. Incluye perfiles de
sesión, finales de línea y `Ctrl/⌘+Enter`, comandos rápidos, envíos
temporizados (mínimo 50 ms), checksums `SUM8`/`XOR8`/`CRC16`, log de texto o
Hex con pausa/scroll/exportación, parseo local `RawData`/`FireWater`/
`JustFloat` con onda, y control DTR/RTS/BREAK.

Todos los envíos siguen pasando por `POST /v1/write`; los parámetros físicos
del puerto se fijan al iniciar por CLI/TOML. La guía de build y la tabla de
funcionalidades están en [`web/README.md`](./web/README.md), y el contrato
detallado en [`web/PROTOCOL.zh-TW.md`](./web/PROTOCOL.zh-TW.md).

---

## Cómo usarlo (escenarios)

### Idea central: un puerto real → muchos extremos en paralelo

```text
                 ┌─ PTY /tmp/ohmyserial-v0  → GUI serie #1
                 ├─ PTY /tmp/ohmyserial-v1  → GUI #2 / agente por serie virtual
 UART real ──► hub ┼─ TCP :8788            → muchos scripts a la vez
                 ├─ TCP :8789            → más herramientas
                 └─ WS  /v1/stream        → muchos agentes a la vez
```

Todos reciben el **mismo RX en vivo**. El TX se arbitra (política/lock).

Usa **`[fanout]`** para crear en bloque, o `[[clients]]` uno a uno.

```bash
curl -s http://127.0.0.1:8787/v1/endpoints
```

### A — Varias apps host + agente

```toml
[fanout]
pty_count = 2
pty_link_prefix = "/tmp/ohmyserial-v"
tcp_count = 1
tcp_base_port = 8788
```

### B — Solo scripts / CI

`tcp_count = 2` + API; sin PTY.

### C — Demo sin hardware

`path = "mock:demo"`.

### D — Lock exclusivo

```bash
curl -s -X POST http://127.0.0.1:8787/v1/lock \
  -H 'content-type: application/json' \
  -d '{"as_client":"agent"}'
curl -s -X DELETE http://127.0.0.1:8787/v1/lock
```

---

## Configuración

Ejemplo: [`ohmyserial.example.toml`](./ohmyserial.example.toml)

```toml
[real]
path = "mock:demo"
baud = 115200
databits = 8
parity = "none"
stopbits = 1
flow = "none"
reconnect = true

[tx]
mode = "queue_by_line"     # queue_by_line | queue_by_frame | exclusive | primary_wins
primary = "ui"
write_lock_ms = 3000
slow_client = "drop_oldest"

[api]
bind = "127.0.0.1:8787"
enabled = true
can_read = true
can_write = true
can_control = false # true habilita DTR/RTS/BREAK y handoff; cada petición necesita lease

[[clients]]
type = "tcp"
name = "tcp"
bind = "127.0.0.1:8788"

[[clients]]
type = "websocket"
name = "agent"
history_bytes = 65536

# solo macOS / Linux
# [[clients]]
# type = "pty"
# name = "ui"
# link = "/tmp/ohmyserial-ui"

[log]
mirror_console = true
format = "hex+text"
```

| Campo | Significado |
|-------|-------------|
| `real.path` | Ruta del dispositivo o `mock:nombre` |
| `tx.mode` | Política de escritura concurrente |
| `api.bind` | Dirección HTTP/WS (preferir localhost) |
| `api.token_env` | Nombre de la variable de entorno con el Bearer; no se guarda en TOML |
| `api.cors_origins` | Lista exacta de Origin; vacío = same-origin y `*` se rechaza |
| `api.can_control` | Habilita DTR/RTS/BREAK y handoff; cada petición necesita un lease |
| `can_read` / `can_write` | Permisos por cliente |

---

## CLI

```bash
ohmyserial run -c ohmyserial.toml    # iniciar hub
ohmyserial share <device> [opciones de serie/endpoint]
ohmyserial supervise -c board-a.toml -c board-b.toml
ohmyserial init [-o file]           # config de ejemplo
ohmyserial list-ports               # listar puertos
ohmyserial status [--api URL]       # consultar estado
```

`share` es el arranque directo de un dispositivo real o `mock:nombre`; `run`
usa un TOML y permite overrides temporales de los parámetros de línea y
endpoints. `supervise` levanta varios perfiles con detección de colisiones.

```bash
RUST_LOG=debug ohmyserial run -c ohmyserial.toml
```

---

## API HTTP y WebSocket

**Base:** `http://127.0.0.1:8787`

La API y el WebSocket dedicado solo escuchan en loopback. Si configuras
`api.token_env`, todas las rutas `/v1/*` salvo `/v1/health` requieren
`Authorization: Bearer <token>`; el token no debe aparecer en TOML, URL ni logs.
Para una página alojada en otro origen, usa `api.cors_origins` con orígenes
exactos y recuerda que WebSocket valida también `Origin`.

| Método | Ruta | Descripción |
|--------|------|-------------|
| `GET` | `/v1/health` | Liveness |
| `GET` | `/v1/status` | Puerto, clientes, lock, stats |
| `GET` | `/v1/clients` | Lista de clientes |
| `GET` | `/v1/endpoints` | PTY/TCP disponibles y estado |
| `GET` | `/v1/events` | Historial paginado de eventos |
| `GET` | `/v1/events/status` | Estado del ledger de eventos |
| `GET` | `/v1/events/export` | Exportar eventos como JSONL |
| `WS` | `/v1/events/stream` | Stream de eventos en vivo |
| `POST` | `/v1/workflows/run` | Ejecutar workflow acotado |
| `POST` | `/v1/control` | DTR/RTS/BREAK mediante el dueño serie |
| `POST` | `/v1/handoff` | Solicitar entrega controlada |
| `POST` | `/v1/handoff/resume` | Reanudar después de una entrega |
| `POST` | `/v1/write` | Enviar text o hex al dispositivo |
| `POST` | `/v1/lock` | Adquirir lock de escritura |
| `DELETE` | `/v1/lock` | Liberar lock |
| `WS` | `/v1/stream` | Stream RX en vivo |

### Write

```bash
curl -s -X POST http://127.0.0.1:8787/v1/write \
  -H 'content-type: application/json' \
  -d '{"text":"AT","newline":true,"as_client":"agent"}'

curl -s -X POST http://127.0.0.1:8787/v1/write \
  -H 'content-type: application/json' \
  -d '{"hex":"41 54 0d 0a","as_client":"agent"}'
```

Las escrituras HTTP son atómicas y `ok: true` solo confirma `write_all` +
`flush` del host, no el ACK del dispositivo. Con un lease activo hay que
enviar su `lease_token`; el nombre `as_client` solo sirve para auditoría.
Activa `api.can_control = true` para DTR/RTS/BREAK y usa primero un lease;
mock no tiene líneas físicas y RTS se rechaza con flow control hardware.
Para que una herramienta externa abra temporalmente el puerto real, sigue el
handoff acotado de [`HANDOFF.md`](./HANDOFF.md).

### WebSocket

```text
ws://127.0.0.1:8787/v1/stream
```

### TCP

```bash
nc 127.0.0.1 8788
```

### Agente mínimo en Python

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

## Políticas de TX

| Modo | Comportamiento | Uso típico |
|------|----------------|------------|
| `queue_by_line` **(defecto)** | Espera `\n` y envía la línea completa | Texto / AT / CLI |
| `queue_by_frame` | Espera un delimitador | Tramas binarias simples |
| `exclusive` | Solo con lock activo | Flash / operaciones críticas |
| `primary_wins` | Prefiere `tx.primary` | Humano en el bucle |

Con **write lock** activo, solo el dueño puede hacer TX. Expira por tiempo, release o desconexión.

`slow_client = drop_oldest` protege la lectura en tiempo real del puerto real.

---

## PTY en Unix (macOS / Linux)

```toml
[[clients]]
type = "pty"
name = "ui"
link = "/tmp/ohmyserial-ui"
can_write = true
can_read = true
```

Abre `/tmp/ohmyserial-ui` en minicom, screen, Serial Studio, etc.

> El baud real lo controla la sección `[real]` del hub. Algunas apps fallan en ioctls de baud sobre PTY; el flujo de datos suele seguir funcionando.

---

## Notas para Windows

| Necesidad | Usar |
|-----------|------|
| Agente / automatización | HTTP + WebSocket ✅ |
| Flujo simple | TCP `127.0.0.1:8788` ✅ |
| Hardware | `path = "COM3"` ✅ |
| UI antigua solo COM | Puente externo (p. ej. com0com) — aún no integrado |
| `type = "pty"` | No soportado |

---

## Seguridad

- Por defecto solo **localhost** (`127.0.0.1`)  
- Escribir al serie es tocar hardware (reset, comandos peligrosos)  
- Puedes configurar Bearer mediante `api.token_env`; aun así HTTP/WS/TCP en claro solo se pueden enlazar a loopback. Para acceso remoto usa un túnel SSH o un reverse proxy TLS
- Los logs pueden contener secretos del stream del dispositivo  

---

## Estructura del proyecto

```text
oh_myserial/
├── README.md           # English (por defecto)
├── README.zh-CN.md     # 简体中文
├── README.es.md        # Español
├── POSITIONING.md
├── ohmyserial.example.toml
├── web/README.md       # guía de la consola web embebida
├── src/ ...
└── tests/
```

---

## Desarrollo

```bash
cargo test
cargo run -- run -c ohmyserial.example.toml
cargo fmt
cargo clippy
```

CI: Ubuntu · macOS · Windows.

---

## Hoja de ruta

| Fase | Alcance |
|------|---------|
| ✅ Núcleo | Políticas, TCP, HTTP/WS, PTY, mock, logs, CLI y multi-puerto con colisiones |
| ✅ Control + web | Perfiles, eventos/replay, workflows, lease/handoff, DTR/RTS/BREAK, comandos rápidos, temporizador, checksums y onda |
| 🔜 Después | RFC2217, guía de puente COM virtual en Windows, más analizadores de protocolo y métricas |

---

## FAQ

**¿Pueden escribir dos clientes a la vez?**  
No como bytes intercalados. Por defecto se encolan líneas completas; el lock da ventanas exclusivas.

**¿El agente necesita un COM virtual?**  
No. Prefiere WebSocket + HTTP.

**¿Por qué el baud en el PTY no cambia el dispositivo?**  
El hub es dueño de la configuración real del puerto.

**¿Es solo un sniffer?**  
No. Es un **hub interactivo de compartición** con control de TX.

**¿Mock necesita hardware?**  
No.

---

## Contribuir

Issues y PRs: https://github.com/tianrking/oh_myserial  

Alinea los cambios con [`POSITIONING.md`](./POSITIONING.md).

---

## Licencia

[MIT](./LICENSE) © contribuidores de ohmyserial

---

## Etiquetas técnicas

`serial` · `uart` · `puerto-com` · `tty` · `serial-hub` · `multiplexor` · `compartir-puerto` · `embebido` · `depuración` · `agente-ia` · `websocket` · `http-api` · `tcp` · `pty` · `tokio` · `axum` · `rust` · `multiplataforma` · `macos` · `linux` · `windows` · `toml` · `mit` · `ohmyserial`

---

<p align="center">
  <a href="./README.md">English</a> ·
  <a href="./README.zh-CN.md">简体中文</a> ·
  <a href="./README.es.md">Español</a>
  <br/>
  <sub>Un puerto. Muchos clientes. Sin pelearse.</sub>
</p>
