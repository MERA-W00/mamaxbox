# **cuba libre**

# mamaxbox (isopod)

> **Experimental Xbox Series X|S (Scarlett) Compatibility & Recompilation Layer for Arch Linux**  
> *Capa experimental de compatibilidad y recompilación de Xbox Series X|S para sistemas basados en Arch Linux*

---

## ⚖️ Legal Disclaimer / Descargo de Responsabilidad Legal

### English
**FOR EDUCATIONAL, RESEARCH, AND INTEROPERABILITY PURPOSES ONLY.**  
This project is an open-source experimental research tool exploring PE mapping, High-Level Emulation (HLE), and Direct3D 12 translation for x86-64 console architecture on Linux.
- **No Proprietary Code or Game Assets:** This repository contains **NO** game binaries, copyrighted assets, ROMs, ISOs, BIOS dumps, or cryptographic keys.
- **User-Provided Software:** Users must provide and dump their own legitimately acquired title packages from legally owned retail media or console hardware.
- **Non-Affiliation:** This project is independent and is **not** endorsed, certified, sponsored, or affiliated in any way with Microsoft Corporation, Xbox, Take-Two Interactive, Rockstar Games, or any of their subsidiaries. All trademarks belong to their respective owners.

### Español
**ESTE PROYECTO TIENE FINES EXCLUSIVAMENTE EDUCATIVOS, DE INVESTIGACIÓN E INTEROPERABILIDAD.**  
Esta herramienta es un desarrollo experimental de código abierto destinado a la investigación técnica sobre el mapeo PE, la emulación de alto nivel (HLE) y la traducción de Direct3D 12 para arquitecturas de consola x86-64 en Linux.
- **Sin archivos ni código propietario:** Este repositorio **NO** incluye ejecutables de juegos, recursos protegidos por derechos de autor, ROMs, imágenes ISO, volcados de BIOS ni claves criptográficas.
- **Archivos suministrados por el usuario:** El usuario debe suministrar sus propios volcados de juegos legítimamente adquiridos desde sus consolas o medios físicos autorizados.
- **No afiliación:** Este proyecto no está patrocinado, avalado, respaldado ni afiliado de ninguna manera con Microsoft Corporation, Xbox, Take-Two Interactive, Rockstar Games ni sus filiales. Todas las marcas registradas pertenecen a sus respectivos dueños.

---

## 🚀 Key Features / Características Principales

### 🛡️ Kernel-Level Network Namespace Isolation (Aislamiento de Red a Nivel de Kernel)
- **100% Offline by Default:** Leverages Linux unprivileged network namespaces (`CLONE_NEWNET` / `unshare --net --map-root-user`) to guarantee that running titles, translation layers, and background libraries cannot transmit or receive external packets.
- **Dedicated Loopback (`lo`):** The loopback interface (`127.0.0.1`) is isolated from host physical/wireless adapters (`wlan*`, `eth*`) while remaining fully operational for local IPC (such as the DualSense controller bridge).
- **Hard Kernel Drop:** Any non-loopback outbound connection attempt is dropped immediately at the OS kernel boundary with `ENETUNREACH` / `WSAENETUNREACH`.

### 🎮 Native PS5 DualSense Integration (Soporte Nativo DualSense PS5)
- Native PlayStation 5 DualSense controller support via a local loopback UDP bridge (`127.0.0.1:28777`).
- Seamless gamepad polling mapped directly into Xbox Scarlett `GameInput.dll` HLE with real-time button and analog trigger dispatch.

### 📊 Interactive Performance HUD & Dropdown Menu (Menú Desplegable y Superposición HUD)
- **Toggled with `F2`** (or D-Pad): Full interactive in-game dropdown menu rendered directly onto the swapchain backbuffer.
- **Display Resolution Selector:** Defaults to **1920x1200** (16:10), with on-the-fly switching between `1200p`, `1080p`, `1440p`, `2560p`, and `4K`.
- **Performance & Pacing Ceilings:** Toggle FPS limits dynamically with **`F1`** (`30`, `60`, `120`, `Unlimited`).
- **Graphics & Ray-Tracing Controls:** Real-time toggles for Dynamic RT Shadows, RT Light Shadows, BVH Traversal Guard, AMD FidelityFX FSR 3.1 modes, FSR Sharpening, Water SSR, and Async Compute.
- **Settings Persistence:** Selected configuration choices are saved asynchronously to the user's persistent profile (`settings.xml`).

### 🌊 Memory & Texture Backreads (Lectura de Texturas y Optimizaciones de Memoria)
- Integrated texture backreads (`VICTORY_TEXTURE_BACK=1`) allowing title CPU code to read textures written by shaders (resolving water physics stalls in the prologue).
- Card memory allocation for small buffers (`VICTORY_CARD_BUFFERS_MIN_KB`) and scalar buffer loads reading four dwords per invocation (`VICTORY_SHADER_OPT=127`).

---

## 🛠️ Architecture on Arch Linux / Omarchy

```
[ Xbox Series X|S Title PE Binary (game_scarlett_master.exe) ]
                               │
               ┌───────────────┴───────────────┐
               ▼                               ▼
     [ Host Win32 / CRT ]            [ Scarlett HLE DLLs ]
    (Proton / Wine Runtime)         (d3d12_xs, xmem, GameInput)
               │                               │
               ▼                               ▼
     [ VKD3D-Proton / Vulkan ]       [ DualSense UDP Bridge ]
   (Host NVIDIA/Intel GPU Driver)       (127.0.0.1:28777)
               │                               │
               └───────────────┬───────────────┘
                               ▼
        [ Linux Kernel Network Namespace (lo only) ]
              (Drops all external WAN / LAN packets)
```

---

## 🎮 Controls & Hotkeys / Controles y Atajos

| Hotkey / Input | Action / Acción |
| :--- | :--- |
| **`F1`** | Cycle FPS limit: `[30]` ➔ `[60]` ➔ `[120]` ➔ `[UNLIMITED]` |
| **`F2`** | Toggle interactive Dropdown Menu / Mostrar u ocultar menú desplegable |
| **`Up` / `Down`** (or `W`/`S` / D-Pad) | Navigate menu rows / Navegar filas del menú |
| **`Left` / `Right`** (or `A`/`D` / D-Pad) | Change or cycle setting / Cambiar o ciclar opción |
| **`Enter` / `Space`** | Confirm or cycle option / Confirmar opción |
| **`Esc`** | Close dropdown menu / Cerrar menú desplegable |
| **PS5 `✕` / Xbox `A`** | Select / Jump / Continuar |
| **PS5 `○` / Xbox `B`** | Cancel / Back / Volver |

---

## 📦 Setup & Execution Guide / Guía de Instalación y Uso

### 1. Requirements (Requisitos)
- **OS:** Arch Linux / Omarchy (or compatible Linux distribution) with Wayland (Hyprland) or X11.
- **Dependencies:** `rust` (edition 2024), `clang`, `lld`, `unshare`, `python` (for DualSense bridge), and Steam Proton Experimental.
- **Hardware:** Vulkan 1.3 capable GPU (NVIDIA RTX / AMD RDNA with proprietary or Mesa drivers).

### 2. Configuration (`victory.toml`)
Create a `victory.toml` file at the root of the repository (or copy from `victory.example.toml`):
```toml
[paths]
game_dir  = "/path/to/extracted/game_directory"
cache_dir = "/path/to/cache"
data_dir  = "/path/to/data"

[network]
# Enforces Linux kernel network namespace isolation
isolation = "kernel"
```

### 3. Build (Compilación)
Compile the optimized Windows release binary:
```bash
TMPDIR=/tmp cargo xwin build --release --target x86_64-pc-windows-msvc
```

### 4. Run Under Kernel Isolation (Ejecución Aislada)
Use the included isolation wrapper to verify that host network interfaces are absent and launch the title:
```bash
# Verify isolation status:
./scripts/victory-isolate.sh --check

# Launch the game inside the isolated namespace:
./scripts/victory-isolate.sh ./scripts/run-victory.sh run "Z:\path\to\game_scarlett_master.exe" --execute
```

---

## 🤝 Credits & Acknowledgements / Créditos y Agradecimientos

- **[VIctory](https://github.com/Marvisak/VIctory)** by Marvisak and contributors — Original compatibility runtime, loader, and shader translation layer (GPL-3.0-or-later).
- **[fxceless/victory-fork](https://github.com/fxceless/victory-fork)** — Upstream rendering improvements, shader optimizations, and texture backreads.
- **[XCL](https://github.com/ivall/xcl)** by ivall and contributors — Reference Xbox Series behavior and architecture.
- **VKD3D-Proton & Wine / Valve Proton** — Direct3D 12 to Vulkan translation and Windows PE execution on Linux.
- **MERA-W00** — Arch Linux / Omarchy adaptation, kernel-level network isolation, interactive dropdown overlay, and DualSense integration.

---

## 📄 License / Licencia
This project is licensed under the **GNU General Public License v3.0 or later** ([GPL-3.0-or-later](LICENSE)).
