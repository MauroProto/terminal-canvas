# Documentación Completa: Emulador de Terminal con Canvas Infinito

## Guía Exhaustiva de Construcción Paso a Paso

> **Versión del análisis:** 1.2.0  
> **Lenguaje:** 100% Rust (6,488 líneas en 36 archivos fuente)  
> **Licencia:** MIT  

---

## Tabla de Contenidos

1. [Descripción General del Proyecto](#1-descripción-general-del-proyecto)
2. [Requisitos Previos](#2-requisitos-previos)
3. [Estructura del Proyecto](#3-estructura-del-proyecto)
4. [Configuración Inicial](#4-configuración-inicial)
5. [Arquitectura de la Aplicación](#5-arquitectura-de-la-aplicación)
6. [Sistema de Canvas Infinito](#6-sistema-de-canvas-infinito)
7. [Emulación de Terminal](#7-emulación-de-terminal)
8. [Sistema de Input del Terminal](#8-sistema-de-input-del-terminal)
9. [Sistema de Paneles](#9-sistema-de-paneles)
10. [Workspaces](#10-workspaces)
11. [Sidebar](#11-sidebar)
12. [Command Palette](#12-command-palette)
13. [Minimap](#13-minimap)
14. [Sistema de Persistencia](#14-sistema-de-persistencia)
15. [Sistema de Auto-Update](#15-sistema-de-auto-update)
16. [Fuentes y Tipografía](#16-fuentes-y-tipografía)
17. [Sistema de Colores Completo](#17-sistema-de-colores-completo)
18. [Optimizaciones de Rendimiento](#18-optimizaciones-de-rendimiento)
19. [Manejo de Errores](#19-manejo-de-errores)
20. [Testing](#20-testing)
21. [CI/CD y Release](#21-cicd-y-release)
22. [Guía Paso a Paso para Construir la App](#22-guía-paso-a-paso-para-construir-la-app)
23. [Roadmap y Features Pendientes](#23-roadmap-y-features-pendientes)

---

## 1. Descripción General del Proyecto

### 1.1 ¿Qué es y qué hace?

La aplicación es un **emulador de terminal con canvas infinito**, acelerado por GPU, multiplataforma y escrito 100% en Rust. A diferencia de los emuladores de terminal tradicionales que usan pestañas o divisiones (splits), este proyecto presenta un paradigma completamente diferente: un **lienzo 2D infinito** donde los paneles de terminal pueden posicionarse libremente, redimensionarse y organizarse espacialmente.

Es una **aplicación de escritorio nativa**. No hay HTML, CSS, JavaScript, ni ninguna tecnología web involucrada. La interfaz gráfica completa se renderiza por GPU usando un framework de modo inmediato (immediate-mode GUI).

### 1.2 Propuesta de Valor

- **Sin pestañas, sin splits, sin tiling** — solo una superficie 2D infinita
- Paneles de terminal que se colocan en cualquier lugar con navegación por pan y zoom
- Renderizado a **60fps** acelerado por GPU
- **Multiplataforma**: Windows, macOS y Linux
- **Cero tecnologías web**: No Electron, no WebView, no runtime de JavaScript
- Emulación de terminal completa: ANSI/VT100, 256 colores, truecolor
- Sistema de workspaces independientes
- Snap guides para alineación visual
- Command palette estilo VS Code
- Sistema de auto-actualización integrado

### 1.3 Stack Tecnológico Completo

| Componente | Tecnología | Versión | Propósito |
|---|---|---|---|
| **Lenguaje** | Rust | Edition 2021 | Lenguaje principal (100% del código) |
| **Windowing** | eframe | 0.30 | Ventana nativa multiplataforma (Wayland, X11, Win32, Cocoa) |
| **UI Framework** | egui | 0.30 | GUI de modo inmediato — todos los widgets renderizados por GPU cada frame |
| **GPU Backend** | wgpu | (via eframe) | Vulkan (Linux/Windows), Metal (macOS), DX12 (Windows fallback) |
| **Terminal** | alacritty_terminal | 0.25.1 | Máquina de estados VT100/ANSI/256-color/truecolor completa |
| **PTY** | portable-pty | 0.9 | Pseudo-terminales multiplataforma |
| **Serialización** | serde + serde_json | 1.x | Serialización JSON para persistencia |
| **TOML** | toml | 0.8 | Parsing TOML (futuro: configuración) |
| **Imágenes** | image | 0.25 | Decodificación PNG para iconos |
| **UUID** | uuid | 1.x | Generación de UUIDs v4 para IDs de paneles y workspaces |
| **Tiempo** | chrono | 0.4 | Timestamps (con feature serde) |
| **Directorios** | directories | 5.x | Rutas de datos del sistema por plataforma |
| **Clipboard** | arboard | 3.x | Acceso al portapapeles del sistema |
| **File Dialogs** | rfd | 0.15 | Diálogos de archivo nativos |
| **HTTP** | minreq | 2.x | Cliente HTTP para auto-update (con HTTPS nativo) |
| **Hashing** | sha2 | 0.10 | Verificación SHA-256 de checksums |
| **Errores** | anyhow | 1.x | Manejo de errores ergonómico |
| **Logging** | log + env_logger | 0.4 / 0.11 | Sistema de logging |

### 1.4 Estadísticas del Proyecto

| Métrica | Valor |
|---|---|
| Total líneas de código Rust | 6,488 |
| Archivos fuente | 36 |
| Archivo más grande | `terminal/panel.rs` (1,423 líneas) |
| Segundo más grande | `app.rs` (829 líneas) |
| Tercero | `terminal/input.rs` (534 líneas) |
| Componentes UI (structs con show()) | 5 (App, TerminalPanel, Sidebar, CommandPalette, Minimap) |
| Threads background por terminal | 3 |
| Comandos registrados | 13 |
| Colores de acento de panel | 8 |
| Atajos de teclado | 15 |
| Módulos placeholder/TODO | 6 |
| Tests unitarios | 15 (en 5 módulos) |

---

## 2. Requisitos Previos

### 2.1 Herramientas Necesarias

| Requisito | Detalles |
|---|---|
| **Rust toolchain** | Canal estable vía [rustup.rs](https://rustup.rs) (edición 2021) |
| **Plataforma** | Windows, macOS o Linux |
| **GPU** | Compatible con Vulkan, Metal o DX12 |

#### Instalación de Rust

```bash
# Instalar Rust via rustup
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Verificar la instalación
rustc --version
cargo --version
```

### 2.2 Requisitos de Hardware

- **GPU**: Se requiere una GPU compatible con al menos uno de los siguientes APIs:
  - Vulkan (Linux/Windows)
  - Metal (macOS)
  - DX12 (Windows fallback)
- **RAM**: Mínimo 4GB recomendado para compilación
- **Disco**: ~2GB para el toolchain de Rust + dependencias compiladas

### 2.3 Dependencias por Sistema Operativo

#### Linux (Ubuntu/Debian)

```bash
sudo apt-get update && sudo apt-get install -y \
    libasound2-dev \
    libudev-dev \
    libwayland-dev \
    libx11-dev \
    libxcursor-dev \
    libxi-dev \
    libxinerama-dev \
    libxkbcommon-dev \
    libxrandr-dev \
    pkg-config
```

Estas dependencias son necesarias para:
- `libasound2-dev`: Audio (ALSA) — requerido por dependencias transitivas
- `libudev-dev`: Detección de dispositivos
- `libwayland-dev`: Soporte de Wayland
- `libx11-dev`, `libxcursor-dev`, `libxi-dev`, `libxinerama-dev`, `libxrandr-dev`: Soporte de X11
- `libxkbcommon-dev`: Manejo de teclado
- `pkg-config`: Detección de bibliotecas del sistema

#### macOS

No se requieren dependencias adicionales más allá de Xcode Command Line Tools:

```bash
xcode-select --install
```

#### Windows

- Visual Studio Build Tools con el workload de C++ (para MSVC linker)
- O MinGW-w64 para compilación con GNU toolchain

### 2.4 Targets de Compilación Soportados

| Target | Plataforma |
|---|---|
| `x86_64-pc-windows-msvc` | Windows x64 |
| `x86_64-unknown-linux-gnu` | Linux x64 |
| `aarch64-apple-darwin` | macOS Apple Silicon |
| `x86_64-apple-darwin` | macOS Intel |
| `aarch64-unknown-linux-gnu` | Linux ARM64 |

---

## 3. Estructura del Proyecto

### 3.1 Árbol Completo de Directorios y Archivos

```
proyecto/
├── Cargo.toml                      # Manifiesto principal con dependencias
├── Cargo.lock                      # Lock file de dependencias
├── dist.toml                       # Configuración de cargo-dist
├── RELEASING.md                    # Guía de proceso de release
├── LICENSE                         # Licencia MIT
├── README.md                       # Documentación principal
│
├── .cargo/
│   ├── config.toml                 # Configuración de cargo (cross-compilation)
│   ├── mingw-linker.cmd            # Helper MinGW para Windows
│   └── mingw-dlltool.cmd           # Helper MinGW para Windows
│
├── .github/
│   ├── release.yml                 # Configuración de release notes
│   └── workflows/
│       ├── ci.yml                  # CI: fmt, clippy, test, build
│       └── release.yml             # Release: build multiplataforma + publish
│
├── assets/
│   ├── icon.png                    # Icono de la aplicación (embebido en binario)
│   ├── brand.png                   # Logo de marca (embebido en binario)
│   ├── icon.ico                    # Icono Windows (para installer)
│   ├── banner.png                  # Banner para README
│   ├── demo.gif                    # Demo animado para README
│   └── demo.png                    # Screenshot para README
│
├── installer/
│   ├── install.sh                  # Script de instalación multiplataforma
│   └── installer.nsi                    # Script NSIS para instalador Windows
│
└── src/
    ├── main.rs                (53) # Entry point, bootstrap de eframe
    ├── app.rs                (829) # Struct principal VoidApp + loop de update
    ├── panel.rs              (168) # CanvasPanel enum — wrapper unificado de paneles
    │
    ├── canvas/                     # Sistema de canvas 2D infinito
    │   ├── mod.rs                  # Declaraciones de módulo
    │   ├── config.rs          (27) # Constantes del canvas (zoom, grid, snap, panel defaults)
    │   ├── viewport.rs       (107) # Cámara pan/zoom: transformaciones screen ↔ canvas
    │   ├── scene.rs           (61) # Input del canvas: pan (middle-click), zoom (Ctrl+scroll/pinch)
    │   ├── grid.rs            (34) # Renderizador de grid de puntos
    │   ├── minimap.rs        (181) # Overlay minimap con navegación por click
    │   ├── snap.rs           (254) # Motor de snap guides (alineación de bordes durante drag/resize)
    │   └── layout.rs           (1) # TODO: Algoritmos de auto-layout (grid, filas, cascada)
    │
    ├── terminal/                   # Emulación de terminal + renderizado
    │   ├── mod.rs                  # Declaraciones de módulo
    │   ├── panel.rs         (1423) # TerminalPanel: chrome, interacciones, selección, menú contextual
    │   ├── renderer.rs       (364) # Renderizado de grid de celdas: backgrounds (RLE), texto, cursor
    │   ├── input.rs          (534) # Eventos de tecla egui → secuencias de bytes terminal (CSI, SS3, etc.)
    │   ├── pty.rs            (285) # Spawn de PTY, 3 threads de I/O, resize, bell, OSC 52 clipboard
    │   └── colors.rs         (108) # Mapeo ANSI 16-color + 256-color + truecolor
    │
    ├── sidebar/                    # Sidebar izquierdo con pestañas
    │   ├── mod.rs            (315) # Contenedor sidebar: marca, pestañas, indicador update, atajos
    │   ├── workspace_list.rs (229) # Árbol de workspaces con terminales anidados
    │   └── terminal_list.rs  (149) # Lista plana de terminales del workspace activo
    │
    ├── command_palette/            # Paleta de comandos fuzzy (Ctrl+Shift+P)
    │   ├── mod.rs            (303) # UI overlay: input de búsqueda, lista filtrada, navegación por teclado
    │   ├── commands.rs        (95) # Enum Command + registro con labels y shortcuts
    │   └── fuzzy.rs           (68) # Fuzzy string matching con scoring heurístico
    │
    ├── state/                      # Estado de la aplicación + persistencia
    │   ├── mod.rs                  # Declaraciones de módulo
    │   ├── workspace.rs      (270) # Modelo Workspace: ciclo de vida de paneles, placement inteligente
    │   ├── persistence.rs     (74) # Save/load JSON a ~/.local/share/terminal-app/layout.json
    │   └── panel_state.rs      (1) # TODO: Gestión de estado de paneles
    │
    ├── theme/                      # Sistema de temas (placeholder)
    │   ├── mod.rs                  # Declaraciones de módulo
    │   ├── builtin.rs          (1) # TODO: Temas built-in (custom-dark, catppuccin)
    │   ├── colors.rs           (1) # TODO: Tipos de paleta de colores
    │   └── fonts.rs            (1) # TODO: Carga de fuentes, gestión de atlas
    │
    ├── shortcuts/                  # Sistema de atajos (parcialmente implementado)
    │   ├── mod.rs                  # Declaraciones de módulo
    │   └── default_bindings.rs (1) # TODO: Mapa de keybindings por defecto
    │
    ├── update.rs             (518) # Auto-update: GitHub Releases API, verificación SHA256
    │
    └── utils/                      # Utilidades compartidas
        ├── mod.rs                  # Declaraciones de módulo
        └── platform.rs          (1) # TODO: Detección de plataforma
```

### 3.2 Explicación de Cada Módulo

#### `src/main.rs` (53 líneas)
El punto de entrada de la aplicación. Inicializa el logger, carga el icono embebido, configura las opciones de la ventana nativa y lanza el loop de eframe.

#### `src/app.rs` (829 líneas)
El corazón de la aplicación. Contiene el struct `VoidApp` que implementa `eframe::App`. Su método `update()` es el loop principal de renderizado que se ejecuta cada frame (~60fps). Orquesta todo: shortcuts, sidebar, canvas, paneles, minimap, command palette.

#### `src/panel.rs` (168 líneas)
Enum `CanvasPanel` que envuelve todos los tipos de panel (actualmente solo `Terminal`). Provee una interfaz uniforme de 20+ métodos delegados para posición, tamaño, z-index, foco, drag, resize y renderizado.

#### `src/canvas/` — Sistema de Canvas
- **`config.rs`**: Todas las constantes centralizadas (zoom, grid, snap, tamaños por defecto)
- **`viewport.rs`**: Cámara con pan/zoom y matemática de transformación de coordenadas bidireccional
- **`scene.rs`**: Manejo de input del canvas (pan con middle-click, zoom con Ctrl+scroll/pinch)
- **`grid.rs`**: Renderizado del grid de puntos de referencia visual
- **`minimap.rs`**: Overlay bird's-eye con navegación por click
- **`snap.rs`**: Motor de snap guides para alineación de bordes entre paneles
- **`layout.rs`**: Placeholder para futuros algoritmos de auto-layout

#### `src/terminal/` — Emulación de Terminal
- **`panel.rs`**: El archivo más grande (1,423 líneas). Todo lo relacionado con un panel terminal individual: construcción, layout, ciclo de vida PTY, chrome (barra título, bordes, scrollbar, grips de resize), interacción con mouse, menú contextual
- **`renderer.rs`**: Renderizado two-pass del grid de celdas del terminal
- **`input.rs`**: Traducción completa de eventos de teclado egui a secuencias de bytes terminal
- **`pty.rs`**: Gestión de pseudo-terminales con 3 threads por terminal
- **`colors.rs`**: Mapeo completo de colores ANSI, 256-color y truecolor

#### `src/sidebar/` — Sidebar
- **`mod.rs`**: Contenedor principal con marca, barra de pestañas, indicador de update y hints
- **`workspace_list.rs`**: Vista de árbol de workspaces con terminales anidados
- **`terminal_list.rs`**: Lista plana de terminales con dots de color y estados

#### `src/command_palette/` — Command Palette
- **`mod.rs`**: UI overlay con búsqueda, lista filtrada y navegación por teclado
- **`commands.rs`**: Enum de 13 comandos con labels y shortcuts
- **`fuzzy.rs`**: Algoritmo de fuzzy matching con sistema de scoring

#### `src/state/` — Estado y Persistencia
- **`workspace.rs`**: Modelo `Workspace` con algoritmo inteligente de placement de paneles
- **`persistence.rs`**: Save/load JSON del estado de la app

#### `src/update.rs` (518 líneas)
Sistema completo de auto-actualización: verificación contra GitHub Releases API, descarga de assets por plataforma, verificación SHA-256, instalación por plataforma.

### 3.3 Diagrama de Arquitectura

```
┌──────────────────────────────────────────────────────────────┐
│                        VoidApp (app.rs)                       │
│            Struct principal + loop de renderizado              │
│                                                               │
│  ┌──────────┐ ┌──────────┐ ┌────────────┐ ┌──────────────┐  │
│  │ Viewport │ │ Sidebar  │ │  Command   │ │   Update     │  │
│  │ pan/zoom │ │ tabs     │ │  Palette   │ │   Checker    │  │
│  └────┬─────┘ └────┬─────┘ └─────┬──────┘ └──────┬───────┘  │
│       │            │             │                │           │
│  ┌────┴────────────┴─────────────┴────────────────┴───────┐  │
│  │                    Workspaces                           │  │
│  │  ┌─────────────────────────────────────────────────┐   │  │
│  │  │              Workspace[0]                        │   │  │
│  │  │  ┌──────────────┐ ┌──────────────┐              │   │  │
│  │  │  │ CanvasPanel  │ │ CanvasPanel  │ ...          │   │  │
│  │  │  │  Terminal    │ │  Terminal    │              │   │  │
│  │  │  │ ┌──────────┐│ │ ┌──────────┐│              │   │  │
│  │  │  │ │PtyHandle ││ │ │PtyHandle ││              │   │  │
│  │  │  │ │3 threads ││ │ │3 threads ││              │   │  │
│  │  │  │ └──────────┘│ │ └──────────┘│              │   │  │
│  │  │  └──────────────┘ └──────────────┘              │   │  │
│  │  └─────────────────────────────────────────────────┘   │  │
│  │  ┌─────────────────────────────────────────────────┐   │  │
│  │  │              Workspace[1] ...                     │   │  │
│  │  └─────────────────────────────────────────────────┘   │  │
│  └────────────────────────────────────────────────────────┘  │
│                                                               │
│  ┌────────────┐ ┌────────────┐ ┌──────────┐ ┌────────────┐  │
│  │   Grid     │ │   Snap     │ │ Minimap  │ │Persistence │  │
│  │   dots     │ │   guides   │ │ overlay  │ │   JSON     │  │
│  └────────────┘ └────────────┘ └──────────┘ └────────────┘  │
└──────────────────────────────────────────────────────────────┘
                              │
                              ▼
              ┌───────────────────────────┐
              │      eframe / egui        │
              │   Immediate-mode GUI      │
              │         + wgpu            │
              │  (Vulkan/Metal/DX12)      │
              └───────────────────────────┘
```

---

## 4. Configuración Inicial

### 4.1 Cargo.toml Completo

```toml
[package]
name = "mi-terminal"
version = "1.2.0"
edition = "2021"
description = "An infinite canvas terminal emulator, GPU-accelerated, cross-platform"
license = "MIT"

[[bin]]
name = "mi-terminal"
path = "src/main.rs"

[dependencies]
# GUI Framework
eframe = { version = "0.30", features = ["wgpu"] }
egui = "0.30"

# Terminal Emulation
alacritty_terminal = "0.25.1"
portable-pty = "0.9"

# Serialization
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"

# Image handling
image = { version = "0.25", default-features = false, features = ["png"] }

# Utilities
uuid = { version = "1", features = ["v4"] }
chrono = { version = "0.4", features = ["serde"] }
directories = "5"
arboard = "3"
rfd = "0.15"

# HTTP and crypto
minreq = { version = "2", features = ["https-native"] }
sha2 = "0.10"

# Error handling and logging
anyhow = "1"
log = "0.4"
env_logger = "0.11"

[profile.dev]
opt-level = 1       # Optimización ligera para rendimiento utilizable en desarrollo

[profile.release]
opt-level = 3       # Optimización máxima
lto = true          # Link-Time Optimization completo
codegen-units = 1   # Una sola unidad de codegen (compilación más lenta, binario más rápido)
strip = true        # Eliminar símbolos de debug

[profile.dist]
inherits = "release"
lto = "thin"        # Thin LTO (compilación más rápida, ligeramente menos óptimo)
```

### 4.2 Perfiles de Compilación

#### Perfil `dev` (desarrollo)
```toml
[profile.dev]
opt-level = 1
```
- Optimización nivel 1 incluso en builds de desarrollo
- Esto es crítico porque la aplicación renderiza a 60fps — sin optimización, el rendimiento interactivo es inaceptable
- Compilación rápida con rendimiento decente

#### Perfil `release` (producción)
```toml
[profile.release]
opt-level = 3      # Optimizaciones agresivas incluyendo vectorización
lto = true         # Optimización de todo el programa (Link-Time)
codegen-units = 1  # Máxima optimización inter-módulo
strip = true       # Reduce tamaño del binario eliminando símbolos
```

#### Perfil `dist` (distribución vía cargo-dist)
```toml
[profile.dist]
inherits = "release"
lto = "thin"       # LTO más rápido de compilar, ligeramente menos óptimo
```

### 4.3 Configuración de Cargo (`.cargo/config.toml`)

Para cross-compilation a Windows desde Linux usando MinGW:

```toml
[target.x86_64-pc-windows-gnu]
linker = "ruta/al/proyecto/.cargo/mingw-linker.cmd"
rustflags = ["-C", "dlltool=ruta/al/proyecto/.cargo/mingw-dlltool.cmd"]
```

Scripts helper (`mingw-linker.cmd`, `mingw-dlltool.cmd`) que rutean a través del toolchain MinGW de MSYS2.

### 4.4 Configuración de cargo-dist (`dist.toml`)

```toml
[dist]
cargo-dist-version = "0.30.3"
ci = ["github"]
installers = ["shell", "powershell", "msi"]
targets = [
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
]
install-path = ["$LOCALAPPDATA/Programs/MiTerminal", "~/.local/bin"]
```

---

## 5. Arquitectura de la Aplicación

### 5.1 Entry Point (`main.rs`) — Código Completo

```rust
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod canvas;
mod command_palette;
mod panel;
mod shortcuts;
mod sidebar;
mod state;
mod terminal;
mod theme;
mod update;
mod utils;

use anyhow::Result;
use std::sync::Arc;

fn main() -> Result<()> {
    env_logger::init();

    let icon = {
        let icon_data = include_bytes!("../assets/icon.png");
        let img = image::load_from_memory(icon_data)
            .expect("Failed to load icon")
            .to_rgba8();
        let (w, h) = img.dimensions();
        egui::IconData {
            rgba: img.into_raw(),
            width: w,
            height: h,
        }
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(format!("Mi Terminal | v{}", env!("CARGO_PKG_VERSION")))
            .with_inner_size([1024.0, 640.0])
            .with_min_inner_size([640.0, 400.0])
            .with_icon(Arc::new(icon)),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };

    eframe::run_native(
        "Mi Terminal",
        options,
        Box::new(|cc| Ok(Box::new(app::TerminalApp::new(cc)))),
    )
    .map_err(|e| anyhow::anyhow!("eframe error: {}", e))
}
```

**Puntos clave:**
- `#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]` — oculta la ventana de consola en builds de release en Windows
- `include_bytes!("../assets/icon.png")` — el icono se embebe en el binario en tiempo de compilación
- Tamaño de ventana por defecto: 1024×640, mínimo 640×400
- Se fuerza el renderer wgpu (sin fallback a glow/OpenGL)
- `env!("CARGO_PKG_VERSION")` — inyecta la versión del `Cargo.toml` en tiempo de compilación

### 5.2 Struct Principal VoidApp

```rust
pub struct VoidApp {
    workspaces: Vec<Workspace>,           // Vistas de canvas independientes
    active_ws: usize,                     // Índice del workspace activo
    viewport: Viewport,                   // Estado de la cámara pan/zoom
    sidebar_visible: bool,                // Toggle del sidebar
    show_grid: bool,                      // Toggle del grid overlay
    show_minimap: bool,                   // Toggle del minimap overlay
    ctx: Option<egui::Context>,           // Contexto egui cacheado
    command_palette: CommandPalette,      // Overlay de búsqueda fuzzy de comandos
    renaming_panel: Option<uuid::Uuid>,   // Panel siendo renombrado (modal)
    rename_buf: String,                   // Buffer de texto para diálogo de rename
    brand_texture: egui::TextureHandle,   // Textura del logo de marca
    sidebar: Sidebar,                     // Estado del sidebar (pestaña activa)
    update_checker: UpdateChecker,        // Auto-updater en background
}
```

### 5.3 Loop de Renderizado (`update()`) — Paso a Paso

El método `update()` se ejecuta cada frame (~60fps) y es el corazón de toda la aplicación:

```rust
impl eframe::App for VoidApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // PASO 1: Toggle command palette (Ctrl+Shift+P)
        if ctx.input(|i| i.modifiers.ctrl && i.modifiers.shift 
            && i.key_pressed(egui::Key::P)) {
            self.command_palette.toggle();
        }

        // PASO 2: Handle app-level shortcuts (si command palette NO está abierta)
        if let Some(cmd) = self.handle_shortcuts(ctx) {
            self.execute_command(cmd, ctx, canvas_rect_for_commands);
        }

        // PASO 3: Sync títulos de terminales (OSC title changes)
        for p in &mut self.ws_mut().panels {
            p.sync_title();
        }

        // PASO 4: Keyboard input al terminal enfocado 
        // (si NO hay command palette NI rename dialog)
        if !self.command_palette.open && self.renaming_panel.is_none() {
            for p in &mut self.ws_mut().panels {
                if p.focused() {
                    p.handle_input(ctx);
                    break;
                }
            }
        }

        // PASO 5: Render command palette overlay (si abierto)
        // PASO 6: Render rename dialog (si activo)
        // PASO 7: Render sidebar (egui::SidePanel::left)
        
        // PASO 8: Canvas background layer (egui::Area con Order::Background)
        //   - Grid dots
        //   - Pan/zoom input
        //   - Status bar
        //   - Unfocus-on-click del canvas vacío
        
        // PASO 9: Canvas content layer (egui::Area con Order::Middle)
        //   - Paneles ordenados por z_index
        //   - Renderizados con TSTransform para zoom
        //   - Snap guides
        //   - Procesamiento de interacciones
        //   - Manejo de close
        
        // PASO 10: Minimap overlay (egui::Area con Order::Debug)
    }
}
```

### 5.4 Sistema de Layers (Capas)

La aplicación usa el sistema de ordenamiento de capas de egui para z-stacking correcto:

```
┌──────────────────────────────────────────┐
│ [SidePanel::left (260px)]  [CentralPanel]│
│ ┌────────┐  ┌────────────────────────────┤
│ │ Brand  │  │ Canvas (Background Area)   │
│ │ Tabs   │  │ ┌─────────────────────────┐│
│ │ Content│  │ │ Canvas Content (Middle)  ││
│ │ Hints  │  │ │ [Panel A] [Panel B]     ││
│ └────────┘  │ │      [Panel C]          ││
│             │ └─────────────────────────┘│
│             │ ┌───────────┐              │
│             │ │ Minimap   │ (Debug)      │
│             │ └───────────┘              │
│             │ ┌───────────────────────┐  │
│             │ │ Command Palette       │  │
│             │ │ (Debug + Tooltip)     │  │
│             │ └───────────────────────┘  │
└──────────────────────────────────────────┘
```

**Orden de capas (egui Order):**

| Capa | Uso | Descripción |
|---|---|---|
| `Background` | Canvas grid, pan/zoom input, status bar | La capa más baja |
| `Middle` | Paneles de terminal (ordenados por z-index dentro de la capa) | Contenido principal |
| `Tooltip` | Capa compartida de texto para TODO el contenido terminal + chrome | Asegura z-overlap correcto |
| `Debug` | Command palette, rename dialog, minimap | Overlays que están encima de todo |

### 5.5 Pipeline de Renderizado Completo

```
VoidApp::update() [llamado cada frame]
│
├── 1. Handle shortcuts + command palette
├── 2. Sync títulos de terminal desde PTY
├── 3. Rutear teclado al terminal enfocado
│
├── 4. Sidebar (SidePanel::left) ─── si visible
│   ├── Logo de marca + indicador de update
│   ├── Barra de pestañas (Workspaces / Terminals)
│   └── Contenido scrollable + hints de atajos
│
├── 5. Canvas background (Area::Background)
│   ├── Manejo de input de pan/zoom
│   ├── Renderizado de grid de puntos
│   ├── Click en canvas vacío (desenfoca todo)
│   └── Status bar (zoom % + coordenadas)
│
├── 6. Canvas content (Area::Middle + TSTransform)
│   ├── Ordenar paneles por z_index
│   ├── Frustum cull de paneles fuera de pantalla
│   ├── Para cada panel visible:
│   │   ├── check_resize() → resize de PTY si necesario
│   │   ├── Render panel fill (capa Tooltip)
│   │   ├── Render contenido terminal (capa Tooltip):
│   │   │   ├── Pasada 1: Rectángulos de background RLE
│   │   │   └── Pasada 2: Caracteres de texto + atributos
│   │   ├── Render cursor (capa Tooltip)
│   │   ├── Render highlight de selección (capa Tooltip)
│   │   ├── Render chrome: borde, título, close, scrollbar, grip (capa Tooltip)
│   │   └── Procesar interacciones: drag, resize, click, select, mouse forward
│   └── Dibujar líneas de snap guide
│
└── 7. Minimap overlay (Area::Debug) ─── si visible
    ├── Thumbnails de paneles (rectángulos de color)
    ├── Rectángulo del viewport
    ├── Label de zoom
    └── Navegación por click/drag
```

### 5.6 Patrón de Despacho de Comandos

```rust
fn execute_command(&mut self, cmd: Command, ctx: &egui::Context, screen_rect: Rect) {
    match cmd {
        Command::NewTerminal => { /* spawn terminal en workspace activo */ },
        Command::CloseTerminal => { /* cerrar terminal enfocado */ },
        Command::RenameTerminal => { /* abrir diálogo de rename */ },
        Command::FocusNext => { /* ciclar foco al siguiente panel */ },
        Command::FocusPrev => { /* ciclar foco al panel anterior */ },
        Command::ZoomToFitAll => { /* auto-fit todos los paneles */ },
        Command::ToggleSidebar => { self.sidebar_visible = !self.sidebar_visible; },
        Command::ToggleMinimap => { self.show_minimap = !self.show_minimap; },
        Command::ToggleGrid => { self.show_grid = !self.show_grid; },
        Command::ZoomIn => { /* incrementar zoom */ },
        Command::ZoomOut => { /* decrementar zoom */ },
        Command::ResetZoom => { /* zoom a 1.0, pan a origen */ },
        Command::ToggleFullscreen => { /* fullscreen nativo */ },
    }
}
```

---

## 6. Sistema de Canvas Infinito

### 6.1 Viewport (Pan/Zoom)

El viewport gestiona la cámara del canvas infinito con transformaciones de coordenadas bidireccionales.

```rust
pub struct Viewport {
    pub pan: Vec2,    // Offset en espacio de pantalla
    pub zoom: f32,    // Factor de escala (0.125 a 4.0)
}
```

#### Métodos Clave del Viewport

```rust
impl Viewport {
    /// Construye la matriz de transformación para set_transform_layer
    pub fn transform(&self, canvas_rect: Rect) -> TSTransform {
        TSTransform::new(canvas_rect.min.to_vec2() + self.pan, self.zoom)
    }

    /// Transformación inversa: pantalla → canvas (para hit-testing)
    pub fn screen_to_canvas(&self, screen_pos: Pos2, screen_rect: Rect) -> Pos2 {
        let rel = screen_pos - screen_rect.min;
        Pos2::new(
            (rel.x - self.pan.x) / self.zoom,
            (rel.y - self.pan.y) / self.zoom,
        )
    }

    /// Transformación directa: canvas → pantalla (para renderizado)
    pub fn canvas_to_screen(&self, canvas_pos: Pos2, screen_rect: Rect) -> Pos2 {
        Pos2::new(
            canvas_pos.x * self.zoom + self.pan.x + screen_rect.min.x,
            canvas_pos.y * self.zoom + self.pan.y + screen_rect.min.y,
        )
    }

    /// Zoom anclado en la posición del puntero
    /// Mantiene el punto bajo el cursor estable durante el zoom
    pub fn zoom_around(&mut self, screen_pos: Pos2, screen_rect: Rect, factor: f32) {
        let canvas_pos = self.screen_to_canvas(screen_pos, screen_rect);
        self.zoom = (self.zoom * factor).clamp(ZOOM_MIN, ZOOM_MAX);
        // Recalcular pan para mantener canvas_pos en la misma posición de pantalla
        self.pan = Vec2::new(
            screen_pos.x - screen_rect.min.x - canvas_pos.x * self.zoom,
            screen_pos.y - screen_rect.min.y - canvas_pos.y * self.zoom,
        );
    }

    /// Centra el viewport en un punto del canvas
    pub fn pan_to_center(&mut self, canvas_pos: Pos2, screen_rect: Rect) {
        let center = screen_rect.center();
        self.pan = Vec2::new(
            center.x - screen_rect.min.x - canvas_pos.x * self.zoom,
            center.y - screen_rect.min.y - canvas_pos.y * self.zoom,
        );
    }

    /// Calcula qué parte del canvas es visible
    pub fn visible_canvas_rect(&self, screen_rect: Rect) -> Rect {
        let min = self.screen_to_canvas(screen_rect.min, screen_rect);
        let max = self.screen_to_canvas(screen_rect.max, screen_rect);
        Rect::from_min_max(min, max)
    }

    /// Frustum culling: determina si un panel es visible
    pub fn is_visible(&self, panel_rect: Rect, screen_rect: Rect) -> bool {
        let visible = self.visible_canvas_rect(screen_rect);
        panel_rect.intersects(visible)
    }
}
```

### 6.2 Constantes de Configuración del Canvas

```rust
// src/canvas/config.rs
pub const ZOOM_MIN: f32 = 0.125;              // Zoom mínimo (12.5%)
pub const ZOOM_MAX: f32 = 4.0;                // Zoom máximo (400%)
pub const ZOOM_KEYBOARD_FACTOR: f32 = 1.15;   // Factor de zoom por pulsación de tecla
pub const GRID_SPACING: f32 = 40.0;           // Espaciado del grid en píxeles canvas
pub const GRID_COLOR: Color32 = Color32::from_rgb(30, 30, 30);  // Color del grid
pub const SNAP_THRESHOLD: f32 = 8.0;          // Umbral de snap en píxeles
pub const MINIMAP_WIDTH: f32 = 200.0;         // Ancho del minimap
pub const MINIMAP_HEIGHT: f32 = 150.0;        // Alto del minimap
pub const MINIMAP_PADDING: f32 = 10.0;        // Padding del minimap
pub const MINIMAP_BG: Color32 = Color32::from_rgba_premultiplied(15, 15, 15, 200);
pub const MINIMAP_VIEWPORT_BORDER: Color32 = Color32::from_rgb(100, 100, 100);
pub const DEFAULT_PANEL_WIDTH: f32 = 1904.0;  // Ancho por defecto de un panel nuevo
pub const DEFAULT_PANEL_HEIGHT: f32 = 720.0;  // Alto por defecto de un panel nuevo
pub const PANEL_GAP: f32 = 30.0;              // Espacio entre paneles
```

### 6.3 Grid de Puntos

```rust
// src/canvas/grid.rs (34 líneas)

/// Renderiza un grid de puntos como referencia visual espacial
pub fn draw_grid(painter: &egui::Painter, viewport: &Viewport, screen_rect: Rect) {
    let visible = viewport.visible_canvas_rect(screen_rect);
    
    let start_x = (visible.min.x / GRID_SPACING).floor() as i32;
    let end_x = (visible.max.x / GRID_SPACING).ceil() as i32;
    let start_y = (visible.min.y / GRID_SPACING).floor() as i32;
    let end_y = (visible.max.y / GRID_SPACING).ceil() as i32;
    
    // Performance guard: salta si habría demasiados puntos
    let count = ((end_x - start_x) as i64) * ((end_y - start_y) as i64);
    if count > 15_000 {
        return;
    }
    
    // Radio del punto escalado al zoom (clamped entre 0.3 y 2.0)
    let dot_radius = (0.8 * viewport.zoom).clamp(0.3, 2.0);
    
    for gx in start_x..=end_x {
        for gy in start_y..=end_y {
            let canvas_pos = Pos2::new(gx as f32 * GRID_SPACING, gy as f32 * GRID_SPACING);
            let screen_pos = viewport.canvas_to_screen(canvas_pos, screen_rect);
            painter.circle_filled(screen_pos, dot_radius, GRID_COLOR);
        }
    }
}
```

**Especificaciones del grid:**
- Espaciado: 40px en espacio canvas
- Color de punto: `rgb(30, 30, 30)` — apenas visible contra el fondo `rgb(10, 10, 10)`
- Radio del punto: 0.8px (clamped 0.3–2.0 según zoom)
- Límite de rendimiento: máximo 15,000 puntos (evita stalling al hacer zoom out extremo)

### 6.4 Input del Canvas (Scene)

```rust
// src/canvas/scene.rs (61 líneas)

/// Maneja input de pan y zoom del canvas
pub fn handle_canvas_input(
    ctx: &egui::Context, 
    viewport: &mut Viewport, 
    canvas_rect: Rect,
    hovered_terminal: bool,  // Evita conflicto con scroll del terminal
) {
    // Pan: middle-click drag
    if response.dragged_by(egui::PointerButton::Middle) {
        viewport.pan += response.drag_delta();
    }
    
    // Zoom: Ctrl+scroll, trackpad pinch
    let scroll = ctx.input(|i| i.smooth_scroll_delta);
    if ctx.input(|i| i.modifiers.ctrl) {
        let factor = (scroll.y * 0.003).exp();
        if let Some(pos) = ctx.input(|i| i.pointer.hover_pos()) {
            viewport.zoom_around(pos, canvas_rect, factor);
        }
    } else if !hovered_terminal {
        // Scroll sin Ctrl = pan del canvas (solo si no estamos sobre un terminal)
        viewport.pan += scroll;
    }
}
```

### 6.5 Snap Guide Engine

```rust
// src/canvas/snap.rs (254 líneas)

/// Resultado de una operación de snap
pub struct SnapResult {
    pub delta: Vec2,              // Movimiento ajustado
    pub guides: Vec<SnapGuide>,   // Guías visuales a dibujar
}

pub struct SnapGuide {
    pub vertical: bool,    // true = línea vertical, false = horizontal
    pub position: f32,     // Posición X (vertical) o Y (horizontal)
    pub start: f32,        // Inicio de la línea
    pub end: f32,          // Fin de la línea
}

/// Calcula snap durante drag de paneles
pub fn snap_drag(
    moving_rect: Rect,           // Rect del panel siendo movido
    other_panels: &[Rect],       // Rects de los demás paneles
    threshold: f32,              // SNAP_THRESHOLD = 8.0
) -> SnapResult {
    let mut best_dx: Option<(f32, SnapGuide)> = None;
    let mut best_dy: Option<(f32, SnapGuide)> = None;
    
    for other in other_panels {
        // Testea 3 bordes X (left/center/right) × 3 bordes Y (top/center/bottom)
        // del panel en movimiento contra los mismos bordes de cada otro panel
        
        // X-axis snaps
        for &(my_x, other_x) in &[
            (moving_rect.left(), other.left()),
            (moving_rect.left(), other.center().x),
            (moving_rect.left(), other.right()),
            (moving_rect.center().x, other.left()),
            (moving_rect.center().x, other.center().x),
            (moving_rect.center().x, other.right()),
            (moving_rect.right(), other.left()),
            (moving_rect.right(), other.center().x),
            (moving_rect.right(), other.right()),
        ] {
            let dx = other_x - my_x;
            if dx.abs() < threshold {
                if best_dx.is_none() || dx.abs() < best_dx.as_ref().unwrap().0.abs() {
                    best_dx = Some((dx, SnapGuide {
                        vertical: true,
                        position: other_x,
                        start: moving_rect.top().min(other.top()),
                        end: moving_rect.bottom().max(other.bottom()),
                    }));
                }
            }
        }
        
        // Y-axis snaps (análogo)
        // ...
    }
    
    let mut delta = Vec2::ZERO;
    let mut guides = Vec::new();
    
    if let Some((dx, guide)) = best_dx {
        delta.x = dx;
        guides.push(guide);
    }
    if let Some((dy, guide)) = best_dy {
        delta.y = dy;
        guides.push(guide);
    }
    
    SnapResult { delta, guides }
}
```

**Tipos de snap comprobados:**
- Bordes Left/Center/Right (eje X)
- Bordes Top/Center/Bottom (eje Y)
- Umbral: 8px (`SNAP_THRESHOLD`)
- Elige el candidato de snap más cercano cuando hay múltiples dentro del umbral
- Guías visuales renderizadas como líneas de 1px en `rgba(100, 160, 255, 150)`

**Snap de drag:** Testea los 3 bordes X × 3 bordes Y del panel en movimiento contra todos los bordes de los demás paneles.

**Snap de resize:** Testea solo el borde que se está redimensionando contra todos los bordes de los demás paneles.

---

## 7. Emulación de Terminal

### 7.1 Arquitectura PTY (3 Threads por Terminal)

Cada panel terminal spawn un proceso de pseudo-terminal independiente gestionado por 3 threads de background:

```rust
pub struct PtyHandle {
    pub term: Arc<Mutex<Term<EventProxy>>>,         // Estado completo del terminal VTE
    pub title: Arc<Mutex<String>>,                   // Título actual (vía OSC)
    pub alive: Arc<AtomicBool>,                      // Si el proceso sigue vivo
    pub bell_fired: Arc<AtomicBool>,                 // Si sonó el bell
    writer: Arc<Mutex<Box<dyn Write + Send>>>,       // Writer al PTY
    last_input_at: Arc<Mutex<Instant>>,              // Último input del usuario
    last_output_at: Arc<Mutex<Instant>>,             // Último output del terminal
    master: Box<dyn portable_pty::MasterPty + Send>, // Handle maestro PTY
    killer: Box<dyn ChildKiller + Send + Sync>,      // Kill handle del proceso hijo
    _event_thread: thread::JoinHandle<()>,           // Thread de eventos
    _reader_thread: thread::JoinHandle<()>,          // Thread de lectura
    _waiter_thread: thread::JoinHandle<()>,          // Thread de espera de exit
}
```

#### Thread 1: Event Thread
Procesa eventos de `alacritty_terminal::Event`:
- `Event::PtyWrite(text)` → reenvía al writer del PTY
- `Event::Title(t)` → actualiza `Arc<Mutex<String>>` title
- `Event::ResetTitle` → restaura título a "Terminal"
- `Event::ChildExit` | `Event::Exit` → `alive = false`
- `Event::Bell` → `bell_fired = true`
- `Event::ClipboardStore` → `arboard::Clipboard.set_text()` (OSC 52)

#### Thread 2: Reader Thread
Lee bytes de salida del PTY y los alimenta al parser VTE:

```rust
// Pseudocódigo del reader thread
loop {
    let mut buf = [0u8; 4096];
    match reader.read(&mut buf) {
        Ok(n) if n > 0 => {
            let mut term = term.lock().unwrap();
            let mut processor = Processor::new();
            for byte in &buf[..n] {
                processor.advance(&mut *term, *byte);
            }
            *last_output_at.lock().unwrap() = Instant::now();
            ctx.request_repaint();
        }
        Ok(_) | Err(_) => {
            alive.store(false, Ordering::Relaxed);
            break;
        }
    }
}
```

#### Thread 3: Waiter Thread
Monitorea la terminación del proceso hijo:

```rust
// Pseudocódigo del waiter thread
let _ = child.wait();
alive.store(false, Ordering::Relaxed);
ctx.request_repaint();
```

#### Primitivas de Concurrencia

| Tipo | Uso |
|---|---|
| `Arc<Mutex<Term<EventProxy>>>` | Estado del terminal compartido entre UI y reader thread |
| `Arc<Mutex<String>>` | Título del terminal actualizado por event thread |
| `Arc<AtomicBool>` | Flag `alive` y `bell_fired` para verificaciones lock-free |
| `Arc<Mutex<Box<dyn Write + Send>>>` | Writer al PTY compartido |
| `std::sync::mpsc::channel` | Canal de eventos terminal (PTY → event thread) |

#### Ciclo de Vida
En el `Drop` del `PtyHandle`, se establece `alive=false` y se llama `killer.kill()` para matar el proceso hijo.

### 7.2 Integración con alacritty_terminal

El terminal utiliza `alacritty_terminal::Term<EventProxy>` como máquina de estados. El `EventProxy` implementa el trait `EventListener` de alacritty:

```rust
pub struct EventProxy {
    event_tx: mpsc::Sender<Event>,
    ctx: egui::Context,
}

impl alacritty_terminal::event::EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        let _ = self.event_tx.send(event);
        self.ctx.request_repaint();
    }
}
```

### 7.3 Variables de Entorno del Terminal

Cuando se spawn un proceso PTY, se configuran estas variables de entorno:

```rust
cmd.env("TERM", "xterm-256color");
cmd.env("COLORTERM", "truecolor");
cmd.env("MI_TERMINAL", "1");
```

- `TERM=xterm-256color`: Identifica el terminal como compatible con 256 colores
- `COLORTERM=truecolor`: Indica soporte de color de 24 bits
- `MI_TERMINAL=1`: Permite a los scripts detectar que están corriendo en esta aplicación

### 7.4 Sistema de Colores ANSI Completo

#### Paleta de 16 Colores ANSI

```rust
const ANSI_COLORS: [Color32; 16] = [
    Color32::from_rgb(0, 0, 0),         // 0  Black
    Color32::from_rgb(204, 0, 0),       // 1  Red
    Color32::from_rgb(78, 154, 6),      // 2  Green
    Color32::from_rgb(196, 160, 0),     // 3  Yellow
    Color32::from_rgb(52, 101, 164),    // 4  Blue
    Color32::from_rgb(117, 80, 123),    // 5  Magenta
    Color32::from_rgb(6, 152, 154),     // 6  Cyan
    Color32::from_rgb(211, 215, 207),   // 7  White
    Color32::from_rgb(85, 87, 83),      // 8  Bright Black
    Color32::from_rgb(239, 41, 41),     // 9  Bright Red
    Color32::from_rgb(138, 226, 52),    // 10 Bright Green
    Color32::from_rgb(252, 233, 79),    // 11 Bright Yellow
    Color32::from_rgb(114, 159, 207),   // 12 Bright Blue
    Color32::from_rgb(173, 127, 168),   // 13 Bright Magenta
    Color32::from_rgb(52, 226, 226),    // 14 Bright Cyan
    Color32::from_rgb(238, 238, 236),   // 15 Bright White
];
```

Esta paleta está inspirada en la paleta Tango/GNOME Terminal.

#### 256 Colores (Cubo 6×6×6 + Rampa de Grises)

```rust
fn indexed_to_egui(idx: u8) -> Color32 {
    match idx {
        0..=15 => ANSI_COLORS[idx as usize],
        // Cubo de color 6×6×6 (índices 16-231)
        16..=231 => {
            let idx = idx - 16;
            let r_idx = idx / 36;
            let g_idx = (idx % 36) / 6;
            let b_idx = idx % 6;
            let r = if r_idx > 0 { r_idx * 40 + 55 } else { 0 };
            let g = if g_idx > 0 { g_idx * 40 + 55 } else { 0 };
            let b = if b_idx > 0 { b_idx * 40 + 55 } else { 0 };
            Color32::from_rgb(r, g, b)
        }
        // Rampa de grises (índices 232-255)
        232..=255 => {
            let gray = (idx - 232) * 10 + 8;
            Color32::from_rgb(gray, gray, gray)
        }
    }
}
```

#### Truecolor (24-bit)
Mapeo directo de `rgb(r, g, b)` a `Color32::from_rgb(r, g, b)`.

#### Colores Dim
Multiplicar cada canal por 2/3 para el atributo DIM:

```rust
fn dim_color(color: Color32) -> Color32 {
    Color32::from_rgb(
        (color.r() as u16 * 2 / 3) as u8,
        (color.g() as u16 * 2 / 3) as u8,
        (color.b() as u16 * 2 / 3) as u8,
    )
}
```

### 7.5 Renderizado de Celdas (Two-Pass)

```rust
// src/terminal/renderer.rs

pub const FONT_SIZE: f32 = 18.0;
pub const PAD_X: f32 = 10.0;
pub const PAD_Y: f32 = 6.0;
```

#### Pasada 1 — Backgrounds de Celdas (Run-Length Encoded)

```rust
// Agrupa celdas consecutivas con el mismo color de fondo
// en una sola operación de dibujo de rectángulo
let mut current_run: Option<(Color32, f32, f32, f32)> = None;  // (color, x, y, width)

for row in 0..rows {
    for col in 0..cols {
        let cell = content.get_cell(col, row);
        let bg = get_background_color(cell);
        let w = cell_width * (if cell.flags.contains(Flags::WIDE_CHAR) { 2.0 } else { 1.0 });
        let sx = content_rect.left() + col as f32 * cell_width;
        let sy = content_rect.top() + row as f32 * cell_height;
        
        // ¿Es continuación de la run actual?
        if let Some(ref mut r) = current_run {
            if r.0 == bg && (r.2 - sy).abs() < 0.1 && (r.1 + r.3 - sx).abs() < 0.5 {
                r.3 += w;  // Extiende la run en vez de dibujar rect individual
                continue;
            }
            // Flush run anterior
            painter.rect_filled(
                Rect::from_min_size(pos2(r.1, r.2), vec2(r.3, cell_height)),
                0.0, r.0,
            );
        }
        current_run = Some((bg, sx, sy, w));
    }
    // Flush al final de cada fila
}
```

Esta optimización masiva reduce dramáticamente el número de llamadas de dibujo para la salida típica de terminal donde muchas celdas consecutivas comparten el mismo color de fondo.

#### Pasada 2 — Texto (Screen-Space para Renderizado Nítido)

```rust
// Cada carácter de celda renderizado individualmente
for row in 0..rows {
    for col in 0..cols {
        let cell = content.get_cell(col, row);
        let c = cell.c;
        if c == ' ' || c == '\0' { continue; }
        
        let mut fg = get_foreground_color(cell);
        
        // Atributos de fuente
        if cell.flags.contains(Flags::BOLD) {
            fg = brighten(fg);  // Multiplicar cada canal por 4/3
        }
        if cell.flags.contains(Flags::DIM) {
            fg = dim_color(fg);  // Multiplicar cada canal por 2/3
        }
        if cell.flags.contains(Flags::HIDDEN) {
            continue;  // No dibujar
        }
        
        // Posición en screen-space (no canvas-space) para texto nítido a cualquier zoom
        let text_pos = Pos2::new(
            screen_x + col as f32 * cell_width * zoom,
            screen_y + row as f32 * cell_height * zoom,
        );
        
        // Offset itálico
        let italic_offset = if cell.flags.contains(Flags::ITALIC) { 1.5 } else { 0.0 };
        
        painter.text(
            text_pos + Vec2::new(italic_offset, 0.0),
            Align2::LEFT_TOP,
            c.to_string(),
            FontId::monospace(FONT_SIZE * zoom),
            fg,
        );
        
        // Decoraciones
        if cell.flags.contains(Flags::UNDERLINE) {
            let y = text_pos.y + cell_height * zoom - 1.0;
            painter.line_segment(
                [Pos2::new(text_pos.x, y), Pos2::new(text_pos.x + cell_width * zoom, y)],
                Stroke::new(1.0, fg),
            );
        }
        if cell.flags.contains(Flags::STRIKEOUT) {
            let y = text_pos.y + cell_height * zoom * 0.5;
            painter.line_segment(
                [Pos2::new(text_pos.x, y), Pos2::new(text_pos.x + cell_width * zoom, y)],
                Stroke::new(1.0, fg),
            );
        }
    }
}
```

### 7.6 Cursor

El cursor soporta múltiples formas y parpadeo:

```rust
// Formas de cursor soportadas
enum CursorShape {
    Block,       // Rectángulo relleno semi-transparente
    Beam,        // Línea vertical de 2px
    Underline,   // Línea horizontal en la base
    HollowBlock, // Rectángulo contorno (outline)
    Hidden,      // No visible
}

// Color del cursor
const CURSOR_COLOR: Color32 = Color32::from_rgb(196, 223, 255);  // Azul claro

// Ciclo de parpadeo
const BLINK_ON_MS: f64 = 600.0;   // 600ms visible
const BLINK_OFF_MS: f64 = 400.0;  // 400ms invisible
const BLINK_CYCLE: f64 = BLINK_ON_MS + BLINK_OFF_MS;  // 1000ms total

fn blink_phase_visible(time: f64) -> bool {
    let phase = time % (BLINK_CYCLE / 1000.0);
    phase < (BLINK_ON_MS / 1000.0)
}
```

**Lógica de visibilidad del cursor:**
- Oculto cuando el panel no tiene foco
- Oculto durante la fase "off" del parpadeo (si blinking está habilitado)
- `request_repaint_after(200ms)` para despertar el loop de eventos durante el parpadeo (eficiencia de batería)

### 7.7 Atributos de Texto

| Flag | Efecto Visual |
|---|---|
| `BOLD` | Color de primer plano más brillante (×4/3) |
| `DIM` | Color más oscuro (×2/3) |
| `ITALIC` | Offset de 1.5px a la derecha |
| `UNDERLINE` | Línea de 1px en la base de la celda |
| `STRIKEOUT` | Línea de 1px en el centro vertical de la celda |
| `HIDDEN` | Carácter no se dibuja |
| `INVERSE` | Intercambio de fg/bg |
| `WIDE_CHAR` | Celda ocupa 2 columnas de ancho |

---

## 8. Sistema de Input del Terminal

### 8.1 Mapeo Completo de Teclas a Secuencias de Bytes

```rust
// src/terminal/input.rs (534 líneas)

/// Convierte un evento de teclado egui a bytes de terminal
fn key_to_bytes(key: &Key, modifiers: &Modifiers, mode: &InputMode) -> Option<Vec<u8>> {
    match key {
        // Texto plano → bytes UTF-8 directos
        // Enter
        Key::Enter => {
            if modifiers.shift { Some(b"\n".to_vec()) }
            else if modifiers.alt { Some(b"\x1b\r".to_vec()) }
            else { Some(b"\r".to_vec()) }
        },
        
        // Backspace
        Key::Backspace => {
            if modifiers.ctrl { Some(b"\x17".to_vec()) }      // Ctrl+Backspace = delete word
            else if modifiers.alt { Some(b"\x1b\x7f".to_vec()) } // Alt+Backspace
            else { Some(b"\x7f".to_vec()) }                    // DEL
        },
        
        // Tab
        Key::Tab => {
            if modifiers.shift { Some(b"\x1b[Z".to_vec()) }   // Shift+Tab = backtab
            else { Some(b"\t".to_vec()) }
        },
        
        // Escape
        Key::Escape => Some(b"\x1b".to_vec()),
        
        // Flechas (respeta application cursor mode)
        Key::ArrowUp => cursor_key_sequence(b'A', modifiers, mode),
        Key::ArrowDown => cursor_key_sequence(b'B', modifiers, mode),
        Key::ArrowRight => cursor_key_sequence(b'C', modifiers, mode),
        Key::ArrowLeft => cursor_key_sequence(b'D', modifiers, mode),
        
        // Home/End
        Key::Home => {
            if modifiers.any() { csi_modifier(b'H', modifiers) }
            else if mode.app_cursor { Some(b"\x1bOH".to_vec()) }
            else { Some(b"\x1b[H".to_vec()) }
        },
        Key::End => {
            if modifiers.any() { csi_modifier(b'F', modifiers) }
            else if mode.app_cursor { Some(b"\x1bOF".to_vec()) }
            else { Some(b"\x1b[F".to_vec()) }
        },
        
        // PageUp/PageDown, Insert, Delete
        Key::PageUp => tilde_key_with_mods(5, modifiers),
        Key::PageDown => tilde_key_with_mods(6, modifiers),
        Key::Insert => tilde_key_with_mods(2, modifiers),
        Key::Delete => {
            if modifiers.ctrl { Some(b"\x1bd".to_vec()) }  // readline delete-word-forward
            else { tilde_key_with_mods(3, modifiers) }
        },
        
        // F1-F20
        Key::F1 => fkey_sequence(1, modifiers),
        Key::F2 => fkey_sequence(2, modifiers),
        // ... hasta F20
        
        // Ctrl+A-Z → caracteres de control (0x01-0x1A)
        // Ctrl+Space → NUL (0x00)
        // Alt+key → prefijo ESC + carácter
        
        _ => None,
    }
}
```

### 8.2 Secuencias de Teclas de Cursor

```rust
/// Genera la secuencia para teclas de cursor respetando el modo
fn cursor_key_sequence(letter: u8, modifiers: &Modifiers, mode: &InputMode) -> Option<Vec<u8>> {
    if modifiers.shift || modifiers.alt || modifiers.ctrl {
        // Con modificadores: siempre formato CSI con parámetro de modificador
        let param = modifier_param(modifiers);
        Some(format!("\x1b[1;{}{}", param, letter as char).into_bytes())
    } else if mode.app_cursor {
        // Application cursor mode (para vim, etc.): formato SS3
        Some(vec![0x1b, b'O', letter])
    } else {
        // Normal mode: formato CSI
        Some(vec![0x1b, b'[', letter])
    }
}

/// Calcula el parámetro de modificador xterm
fn modifier_param(modifiers: &Modifiers) -> u8 {
    1 + (if modifiers.shift { 1 } else { 0 })
      + (if modifiers.alt { 2 } else { 0 })
      + (if modifiers.ctrl { 4 } else { 0 })
}
```

### 8.3 Teclas de Función (F1-F20)

```rust
fn fkey_sequence(fnum: u8, modifiers: &Modifiers) -> Option<Vec<u8>> {
    let has_mods = modifiers.shift || modifiers.alt || modifiers.ctrl;
    
    match fnum {
        // F1-F4: formato SS3 sin modificadores, CSI con modificadores
        1..=4 => {
            let letter = match fnum {
                1 => b'P', 2 => b'Q', 3 => b'R', 4 => b'S',
                _ => unreachable!(),
            };
            if has_mods {
                let param = modifier_param(modifiers);
                Some(format!("\x1b[1;{}{}", param, letter as char).into_bytes())
            } else {
                Some(vec![0x1b, b'O', letter])
            }
        }
        // F5-F20: formato CSI tilde con código numérico
        5..=20 => {
            let code = match fnum {
                5 => 15, 6 => 17, 7 => 18, 8 => 19, 9 => 20,
                10 => 21, 11 => 23, 12 => 24,
                13 => 25, 14 => 26, 15 => 28, 16 => 29,
                17 => 31, 18 => 32, 19 => 33, 20 => 34,
                _ => unreachable!(),
            };
            if has_mods {
                let param = modifier_param(modifiers);
                Some(format!("\x1b[{};{}~", code, param).into_bytes())
            } else {
                Some(format!("\x1b[{}~", code).into_bytes())
            }
        }
        _ => None,
    }
}
```

### 8.4 Bracketed Paste

```rust
// Cuando el terminal solicita bracketed paste mode:
if mode.bracketed_paste {
    // Envuelve el texto pegado con secuencias de marcado
    let wrapped = format!("\x1b[200~{}\x1b[201~", paste_text);
    writer.write_all(wrapped.as_bytes());
} else {
    writer.write_all(paste_text.as_bytes());
}
```

### 8.5 Copy/Paste Multiplataforma

```rust
fn should_copy_selection(modifiers: &Modifiers, key: &Key, has_selection: bool) -> bool {
    #[cfg(target_os = "macos")]
    {
        // macOS: Cmd+C siempre copia (si hay selección)
        modifiers.command && *key == Key::C && has_selection
    }
    #[cfg(not(target_os = "macos"))]
    {
        // Linux/Windows: 
        // Ctrl+C con selección → copia
        // Ctrl+C sin selección → envía SIGINT (0x03) al terminal
        // Ctrl+Shift+C siempre copia
        (modifiers.ctrl && *key == Key::C && has_selection) ||
        (modifiers.ctrl && modifiers.shift && *key == Key::C)
    }
}
```

### 8.6 Mouse Forwarding (SGR Mouse Events)

```rust
// Cuando el terminal está en mouse mode (htop, lazygit, vim):
// Se envían eventos SGR mouse al PTY

// Click
let seq = format!("\x1b[<{};{};{}M", button, col + 1, row + 1);

// Release
let seq = format!("\x1b[<{};{};{}m", button, col + 1, row + 1);

// Scroll up (button 64)
let seq = format!("\x1b[<64;{};{}M", col + 1, row + 1);

// Scroll down (button 65)
let seq = format!("\x1b[<65;{};{}M", col + 1, row + 1);
```

### 8.7 Filtrado de Atajos de la App

```rust
pub const VOID_SHORTCUTS: &[(Modifiers, Key)] = &[
    (Modifiers { ctrl: true, shift: false, .. }, Key::B),    // Toggle sidebar
    (Modifiers { ctrl: true, shift: false, .. }, Key::M),    // Toggle minimap
    (Modifiers { ctrl: true, shift: false, .. }, Key::G),    // Toggle grid
    (Modifiers { ctrl: true, shift: true,  .. }, Key::T),    // New terminal
    (Modifiers { ctrl: true, shift: true,  .. }, Key::W),    // Close terminal
    (Modifiers { ctrl: true, shift: true,  .. }, Key::P),    // Command palette
    // ... etc.
];

fn is_void_shortcut(modifiers: &Modifiers, key: &Key, shortcuts: &[(Modifiers, Key)]) -> bool {
    shortcuts.iter().any(|(m, k)| 
        m.ctrl == modifiers.ctrl && m.shift == modifiers.shift && k == key
    )
}
```

Estos atajos son interceptados por la aplicación y **NO** se envían al terminal.

### 8.8 Scroll Inteligente por Modo

```rust
fn handle_scroll(&mut self, delta: f32, ctx: &egui::Context) {
    let lines = /* convertir píxeles a líneas */;
    
    if self.is_alt_screen() {
        // Alt screen (vim, less): envía teclas de flecha arriba/abajo
        for _ in 0..lines.abs() {
            let seq = if lines > 0 { b"\x1b[A" } else { b"\x1b[B" };
            self.pty_write(seq);
        }
    } else if self.is_mouse_mode() {
        // Mouse mode (htop, lazygit): envía SGR mouse scroll events
        let button = if lines > 0 { 64 } else { 65 };
        for _ in 0..lines.abs() {
            let seq = format!("\x1b[<{};1;1M", button);
            self.pty_write(seq.as_bytes());
        }
    } else {
        // Normal mode: scroll display buffer (historial de scrollback)
        term.scroll_display(Scroll::Delta(lines));
    }
}
```

---

## 9. Sistema de Paneles

### 9.1 CanvasPanel Enum (Extensible)

```rust
// src/panel.rs (168 líneas)

pub enum CanvasPanel {
    Terminal(TerminalPanel),
    // Futuro: Webview(WebviewPanel), Notes(NotesPanel), etc.
}
```

Este enum provee una interfaz uniforme con más de 20 métodos delegados:

```rust
impl CanvasPanel {
    // Identidad
    pub fn id(&self) -> Uuid { match self { Self::Terminal(t) => t.id } }
    pub fn title(&self) -> &str { match self { Self::Terminal(t) => &t.title } }
    pub fn set_title(&mut self, title: String) { /* ... */ }
    
    // Geometría
    pub fn position(&self) -> Pos2 { /* ... */ }
    pub fn set_position(&mut self, pos: Pos2) { /* ... */ }
    pub fn size(&self) -> Vec2 { /* ... */ }
    pub fn rect(&self) -> Rect { /* ... */ }
    
    // Visual
    pub fn color(&self) -> Color32 { /* ... */ }
    pub fn z_index(&self) -> u32 { /* ... */ }
    pub fn set_z_index(&mut self, z: u32) { /* ... */ }
    
    // Estado
    pub fn focused(&self) -> bool { /* ... */ }
    pub fn set_focused(&mut self, focused: bool) { /* ... */ }
    pub fn is_alive(&self) -> bool { /* ... */ }
    
    // Drag/Resize
    pub fn drag_virtual_pos(&self) -> Option<Pos2> { /* ... */ }
    pub fn set_drag_virtual_pos(&mut self, pos: Option<Pos2>) { /* ... */ }
    pub fn resize_virtual_rect(&self) -> Option<Rect> { /* ... */ }
    pub fn set_resize_virtual_rect(&mut self, rect: Option<Rect>) { /* ... */ }
    pub fn apply_resize(&mut self, rect: Rect) { /* ... */ }
    
    // Renderizado
    pub fn show(&mut self, ...) -> PanelInteraction { /* ... */ }
    
    // Input
    pub fn handle_input(&mut self, ctx: &egui::Context) { /* ... */ }
    pub fn handle_scroll(&mut self, ...) { /* ... */ }
    pub fn sync_title(&mut self) { /* ... */ }
    pub fn scroll_hit_test(&self, ...) -> bool { /* ... */ }
    
    // Persistencia
    pub fn to_saved(&self) -> PanelState { /* ... */ }
}
```

### 9.2 TerminalPanel Completo

```rust
// src/terminal/panel.rs (1,423 líneas)

pub struct TerminalPanel {
    pub id: Uuid,                              // ID único
    pub title: String,                         // Título del terminal
    pub position: Pos2,                        // Posición en espacio canvas
    pub size: Vec2,                            // Tamaño en espacio canvas
    pub color: Color32,                        // Color de acento (asignado de paleta)
    pub z_index: u32,                          // Orden de pintado
    pub focused: bool,                         // Foco de teclado
    pty: Option<PtyHandle>,                    // Proceso PTY + alacritty Term
    last_cols: u16,                            // Última cantidad de columnas
    last_rows: u16,                            // Última cantidad de filas
    spawn_error: Option<String>,               // Error al crear PTY
    selection: Option<(usize, usize, usize, usize)>,  // (start_col, start_row, end_col, end_row)
    selection_display_offset: usize,           // Offset de display de selección
    selecting: bool,                           // Selección en progreso
    scroll_remainder: f32,                     // Remanente de scroll sub-línea
    scrollbar_grab_offset: Option<f32>,        // Offset del grab del scrollbar
    last_click_time: f64,                      // Para detección double/triple-click
    click_count: u8,                           // Contador de clicks
    pub drag_virtual_pos: Option<Pos2>,        // Posición virtual de drag (sin snap)
    pub resize_virtual_rect: Option<Rect>,     // Rect virtual de resize (sin snap)
    bell_flash_until: f64,                     // Timer del flash de bell
    pending_mode_reset: Option<f64>,           // Timer de limpieza de ALT_SCREEN
}
```

### 9.3 Constantes Visuales del Panel

```rust
const TITLE_BAR_HEIGHT: f32 = 36.0;       // Alto de la barra de título
const BORDER_RADIUS: f32 = 10.0;          // Radio de esquinas del panel
const MIN_WIDTH: f32 = 400.0;             // Ancho mínimo del panel
const MIN_HEIGHT: f32 = 280.0;            // Alto mínimo del panel
const SCROLLBAR_WIDTH: f32 = 8.0;         // Ancho del scrollbar
const PANEL_BG: Color32 = Color32::from_rgb(17, 17, 17);         // Fondo del panel
const BORDER_DEFAULT: Color32 = Color32::from_rgb(40, 40, 40);   // Borde sin foco
const BORDER_FOCUS: Color32 = Color32::from_rgb(70, 70, 70);     // Borde con foco
const FG: Color32 = Color32::from_rgb(200, 200, 200);            // Texto por defecto
const SELECTION_BG: Color32 = Color32::from_rgba_premultiplied(80, 130, 200, 80);  // Selección
const SCROLLBAR_THUMB: Color32 = Color32::from_rgb(78, 78, 78);  // Thumb del scrollbar
```

### 9.4 Método `show()` — Orden de Renderizado

El método `show()` es el "render" principal del panel. Se ejecuta en el siguiente orden:

1. **Check resize necesario** → actualizar dimensiones del grid PTY
2. **Detección de bell flash** (0.15s de pulso de borde naranja)
3. **Interacción del botón close** (hit-test en espacio canvas)
4. **Fill completo del panel en capa Tooltip** (background opaco — oculta paneles de z inferior)
5. **Renderizado de contenido terminal** vía `renderer::render_terminal()`
6. **Scrollbar**: track + thumb + interacción de drag
7. **Overlay de selección** (ajustado para offset de scroll)
8. **Chrome en capa Tooltip**: borde, separador, dot de color, texto de título, X de close, scrollbar, dots de resize grip
9. **Handles de resize**: 5 rectángulos invisibles (bottom-right, bottom-left, right edge, left edge, bottom edge) extendiéndose 6px fuera del panel
10. **Interacción del body**: click focus, double-click word select, triple-click line select, drag text selection
11. **Mouse forwarding**: eventos SGR mouse al PTY para apps TUI (htop, lazygit, vim)
12. **Title bar**: drag to move, click to focus
13. **Cursor icons**: flechas de resize, cursor I-beam de texto
14. **Context menu**: Copy, Paste, Select All, Clear Scrollback, Reset Terminal, Rename, Close

### 9.5 Drag y Resize con Virtual Positions

```rust
// Virtual position tracking previene que los paneles se "peguen" a los snap points

// Durante drag:
if dragging {
    // Actualizar posición virtual (sin snap)
    let virtual_pos = self.drag_virtual_pos.unwrap_or(self.position);
    let new_virtual = virtual_pos + delta;
    self.drag_virtual_pos = Some(new_virtual);
    
    // Calcular snap desde la posición virtual
    let snap_result = snap_drag(
        Rect::from_min_size(new_virtual, self.size),
        &other_panel_rects,
        SNAP_THRESHOLD,
    );
    
    // Aplicar posición snapped (visual) sin modificar la virtual
    self.position = Pos2::new(
        new_virtual.x + snap_result.delta.x,
        new_virtual.y + snap_result.delta.y,
    );
}

// Al soltar:
self.drag_virtual_pos = None;  // Reset
```

### 9.6 Z-Ordering

```rust
// Los paneles se ordenan por z_index para pintado
let mut order: Vec<usize> = (0..panels.len()).collect();
order.sort_by_key(|&i| panels[i].z_index());

// Al hacer click en un panel, se trae al frente
pub fn bring_to_front(&mut self, panel_id: Uuid) {
    self.next_z += 1;
    if let Some(panel) = self.panels.iter_mut().find(|p| p.id() == panel_id) {
        panel.set_z_index(self.next_z);
        panel.set_focused(true);
    }
    // Desenfoca todos los demás
    for p in &mut self.panels {
        if p.id() != panel_id {
            p.set_focused(false);
        }
    }
}
```

### 9.7 Text Selection

**Click simple**: Posiciona el cursor (y deselecciona).

**Double-click (selección de palabra):**
```rust
fn word_boundaries_at(content: &RenderableContent, col: usize, row: usize) -> (usize, usize) {
    let is_word_char = |c: char| -> bool {
        c.is_alphanumeric() || c == '_' || c == '-' || c == '.' || c == '/'
    };
    
    let line = get_line(content, row);
    let mut start = col;
    let mut end = col;
    
    // Expandir hacia la izquierda
    while start > 0 && is_word_char(line[start - 1]) {
        start -= 1;
    }
    // Expandir hacia la derecha
    while end < line.len() - 1 && is_word_char(line[end + 1]) {
        end += 1;
    }
    
    (start, end + 1)
}
```

**Triple-click (selección de línea):**
Selecciona desde la columna 0 hasta el final de la línea.

### 9.8 Context Menu

```rust
response.context_menu(|ui| {
    if ui.button("Copy").clicked() {
        // Copiar texto seleccionado al clipboard
        if let Some(text) = self.selected_text() {
            if let Ok(mut clipboard) = arboard::Clipboard::new() {
                let _ = clipboard.set_text(text);
            }
        }
        ui.close_menu();
    }
    if ui.button("Paste").clicked() {
        // Pegar del clipboard al PTY
        if let Ok(mut clipboard) = arboard::Clipboard::new() {
            if let Ok(text) = clipboard.get_text() {
                self.pty_write(text.as_bytes());
            }
        }
        ui.close_menu();
    }
    if ui.button("Select All").clicked() { /* ... */ }
    ui.separator();
    if ui.button("Clear Scrollback").clicked() {
        self.pty_write(b"\x1b[3J");  // CSI 3 J = clear scrollback
        ui.close_menu();
    }
    if ui.button("Reset Terminal").clicked() {
        self.pty_write(b"\x1bc");  // RIS = Reset to Initial State
        ui.close_menu();
    }
    ui.separator();
    if ui.button("Rename").clicked() { /* ... */ }
    if ui.button("Close").clicked() { /* ... */ }
});
```

### 9.9 Bell Flash

```rust
// Cuando el terminal envía el carácter BEL:
if pty.bell_fired.swap(false, Ordering::Relaxed) {
    self.bell_flash_until = ctx.input(|i| i.time) + 0.15;  // 150ms de flash
}

// Durante renderizado:
let is_bell = ctx.input(|i| i.time) < self.bell_flash_until;
let border_color = if is_bell {
    Color32::from_rgb(255, 200, 80)  // Naranja durante bell
} else if self.focused {
    BORDER_FOCUS  // rgb(70, 70, 70)
} else {
    BORDER_DEFAULT  // rgb(40, 40, 40)
};
```

### 9.10 Paleta de Colores de Panel (8 Colores)

```rust
const PANEL_COLORS: &[Color32] = &[
    Color32::from_rgb(90, 130, 200),   // Azul
    Color32::from_rgb(200, 90, 90),    // Rojo
    Color32::from_rgb(90, 180, 90),    // Verde
    Color32::from_rgb(200, 160, 60),   // Amarillo
    Color32::from_rgb(150, 90, 200),   // Morado
    Color32::from_rgb(200, 120, 160),  // Rosa
    Color32::from_rgb(80, 170, 200),   // Teal
    Color32::from_rgb(180, 180, 80),   // Olive
];

// Asignación rotativa:
let color = PANEL_COLORS[self.next_color % PANEL_COLORS.len()];
self.next_color += 1;
```

### 9.11 Recuperación de TUI Mode Estancado

Una feature única: cuando una app TUI (vim, htop) es matada con Ctrl+C, puede no enviar las secuencias de escape de limpieza:

```rust
// 1. Si se envía input mientras ALT_SCREEN o MOUSE_MODE están activos, y:
//    - El input es Ctrl+C (0x03), O
//    - No ha habido output del PTY por >500ms
// 2. Se establece un timer de 500ms

// 3. Cuando el timer se activa, si los modos siguen activos, se inyectan secuencias reset:
let reset_sequences = [
    b"\x1b[?1049l",   // Exit alt screen
    b"\x1b[?1000l",   // Disable click mouse tracking
    b"\x1b[?1002l",   // Disable button-event tracking
    b"\x1b[?1003l",   // Disable any-event tracking
    b"\x1b[?1006l",   // Disable SGR mouse encoding
    b"\x1b[?1l",      // Reset cursor keys
    b"\x1b[?2004l",   // Disable bracketed paste
];
```

---

## 10. Workspaces

### 10.1 Modelo de Datos

```rust
pub struct Workspace {
    pub id: Uuid,                  // UUID v4 generado al crear
    pub name: String,              // Nombre visible
    pub cwd: Option<PathBuf>,      // Directorio de trabajo del workspace
    pub panels: Vec<CanvasPanel>,  // Paneles del workspace
    pub viewport_pan: Vec2,        // Estado de pan guardado
    pub viewport_zoom: f32,        // Estado de zoom guardado
    pub next_z: u32,               // Contador global de z-index
    pub next_color: usize,         // Índice rotativo en la paleta de colores
}
```

### 10.2 Algoritmo de Posicionamiento Inteligente (Gap-Filling)

```rust
/// Encuentra la mejor posición para un panel nuevo
/// Llena huecos en L, mantiene layouts compactos
fn find_free_position(&self, size: Vec2) -> Pos2 {
    if self.panels.is_empty() {
        return Pos2::new(50.0, 50.0);
    }
    
    let gap = PANEL_GAP;  // 30px
    
    // PASO 1: Recolectar todos los bordes X e Y únicos de paneles existentes
    let mut x_edges: Vec<f32> = Vec::new();
    let mut y_edges: Vec<f32> = Vec::new();
    
    for panel in &self.panels {
        let rect = panel.rect();
        x_edges.extend_from_slice(&[
            rect.left(),
            rect.right(),
            rect.left() - size.x - gap,    // Alinear a la izquierda con gap
            rect.right() + gap,             // Alinear a la derecha con gap
        ]);
        y_edges.extend_from_slice(&[
            rect.top(),
            rect.bottom(),
            rect.top() - size.y - gap,      // Alinear arriba con gap
            rect.bottom() + gap,             // Alinear abajo con gap
        ]);
    }
    
    // PASO 2: Generar posiciones candidatas en cada intersección (x, y)
    let mut candidates: Vec<Pos2> = Vec::new();
    for &x in &x_edges {
        for &y in &y_edges {
            candidates.push(Pos2::new(x, y));
        }
    }
    
    // PASO 3: Filtrar posiciones que se superponen con paneles existentes
    let valid_candidates: Vec<Pos2> = candidates.into_iter()
        .filter(|pos| {
            let candidate_rect = Rect::from_min_size(*pos, size);
            !self.overlaps_any(candidate_rect, gap)
        })
        .collect();
    
    // PASO 4: Calcular bounding box actual
    let current_bbox = self.panels.iter()
        .map(|p| p.rect())
        .reduce(|a, b| a.union(b))
        .unwrap();
    let center = current_bbox.center();
    
    // PASO 5: Puntuar cada candidato
    // Score = bbox_growth * 2.0 + distance_to_center
    let best = valid_candidates.into_iter()
        .min_by(|a, b| {
            let score_a = {
                let new_bbox = current_bbox.union(Rect::from_min_size(*a, size));
                let growth = (new_bbox.width() * new_bbox.height()) 
                           - (current_bbox.width() * current_bbox.height());
                let dist = a.distance(center);
                growth * 2.0 + dist
            };
            let score_b = {
                let new_bbox = current_bbox.union(Rect::from_min_size(*b, size));
                let growth = (new_bbox.width() * new_bbox.height()) 
                           - (current_bbox.width() * current_bbox.height());
                let dist = b.distance(center);
                growth * 2.0 + dist
            };
            score_a.partial_cmp(&score_b).unwrap()
        });
    
    // PASO 6: Fallback: debajo de todos los paneles existentes
    best.unwrap_or_else(|| {
        Pos2::new(current_bbox.left(), current_bbox.bottom() + gap)
    })
}
```

### 10.3 Switching de Workspace

```rust
// Al cambiar de workspace:
// 1. Guardar viewport actual del workspace saliente
self.workspaces[self.active_ws].viewport_pan = self.viewport.pan;
self.workspaces[self.active_ws].viewport_zoom = self.viewport.zoom;

// 2. Cambiar al nuevo workspace
self.active_ws = new_ws_index;

// 3. Restaurar viewport del workspace entrante
self.viewport.pan = self.workspaces[self.active_ws].viewport_pan;
self.viewport.zoom = self.workspaces[self.active_ws].viewport_zoom;
```

### 10.4 CWD por Workspace

Cada workspace puede tener un directorio de trabajo. Los nuevos terminales heredan el CWD del workspace:

```rust
pub fn spawn_terminal(&mut self, ctx: &egui::Context) {
    let cwd = self.cwd.clone();  // CWD del workspace
    let position = self.find_free_position(default_size);
    let color = PANEL_COLORS[self.next_color % PANEL_COLORS.len()];
    
    let mut panel = TerminalPanel::new(position, default_size, color, self.next_z);
    panel.spawn_pty(ctx, cwd.as_deref());  // Pasa el CWD al PTY
    
    self.panels.push(CanvasPanel::Terminal(panel));
    self.next_z += 1;
    self.next_color += 1;
}
```

---

## 11. Sidebar

### 11.1 Layout Completo

```
Sidebar (260px de ancho)
├── Logo de marca (14px, tintado gris) + Versión / Botón Update (alineado derecha)
├── Barra de pestañas: Workspaces | Terminals (estilo pill, indicador animado)
├── ScrollArea
│   ├── (Pestaña Workspaces) → workspace_list::draw_workspace_tree()
│   └── (Pestaña Terminals) → terminal_list::draw_terminal_list()
└── Bottom: "Ctrl+Shift+T new · Ctrl+B sidebar · Ctrl+M minimap"
```

### 11.2 Paleta de Colores del Sidebar (Tailwind Zinc Inspired)

```rust
pub const SIDEBAR_BG: Color32 = Color32::from_rgb(23, 23, 23);        // zinc-900
pub const SIDEBAR_BORDER: Color32 = Color32::from_rgb(38, 38, 38);    // zinc-800
pub const INPUT_BG: Color32 = Color32::from_rgb(39, 39, 42);          // zinc-800
pub const ACTIVE_TAB_BG: Color32 = Color32::from_rgb(63, 63, 70);     // zinc-700
pub const TEXT_PRIMARY: Color32 = Color32::WHITE;                       // #ffffff
pub const TEXT_SECONDARY: Color32 = Color32::from_rgb(163, 163, 163);  // zinc-400
pub const TEXT_MUTED: Color32 = Color32::from_rgb(115, 115, 115);      // zinc-500
pub const HOVER_BG: Color32 = Color32::from_rgba_premultiplied(39, 39, 42, 120);
pub const ITEM_BG: Color32 = Color32::from_rgb(39, 39, 42);           // zinc-800
```

### 11.3 Tab Bar

- Fondo pill de ancho completo con rounding de 8px
- Dos slots de pestañas con indicador deslizante (rect relleno con stroke)
- Línea divisoria vertical entre pestañas (oculta cuando el indicador la cubre)
- Click para cambiar de pestaña

### 11.4 Workspace List (Tree View)

```
─────────────────── (divisor)
  Nombre Workspace      [+]
    ● terminal-1-título
    ● terminal-2-título
─────────────────── (divisor)
  Otro Workspace         [+]
─────────────────── 
  + Nuevo workspace
```

**Interacciones:**
- Click en header de workspace → cambiar workspace
- Click en botón "+" → spawn terminal en workspace
- Right-click header → "Delete workspace" en menú contextual
- Click en item terminal → foco + pan hacia el terminal
- Right-click terminal → Rename / Close en menú contextual
- Dot de color: color de acento del panel (gris si el proceso terminó)
- Hover: highlight de fila + indicador "···"

### 11.5 Terminal List (Flat List)

Cada item:
- Dot de color de 3px (color del panel, gris si muerto) con anillo glow de 4.5px si vivo
- Texto de título (truncado a 31 chars + "...")
- Estado enfocado: fondo relleno (`ITEM_BG`)
- Estado hover: fondo semi-transparente + indicador "···"
- Click → `SidebarResponse::FocusPanel`
- Menú contextual: Rename / Close

### 11.6 Update Indicator

| Estado | Display |
|---|---|
| Available | Icono de flecha verde (20×20 custom-drawn) → click para descargar |
| Downloading | "Downloading..." texto amarillo |
| Ready | Botón "Update" verde → click para instalar+reiniciar |
| Installing | "Installing..." texto verde |
| UpToDate / Checking | Número de versión en gris muted |
| Error | Número de versión en gris muted |

### 11.7 Todas las Constantes de Color del Sidebar

```rust
// Backgrounds
const SIDEBAR_BG: Color32 = Color32::from_rgb(23, 23, 23);
const SIDEBAR_BORDER: Color32 = Color32::from_rgb(38, 38, 38);
const INPUT_BG: Color32 = Color32::from_rgb(39, 39, 42);
const ACTIVE_TAB_BG: Color32 = Color32::from_rgb(63, 63, 70);
const HOVER_BG: Color32 = Color32::from_rgba_premultiplied(39, 39, 42, 120);
const ITEM_BG: Color32 = Color32::from_rgb(39, 39, 42);

// Text
const TEXT_PRIMARY: Color32 = Color32::WHITE;
const TEXT_SECONDARY: Color32 = Color32::from_rgb(163, 163, 163);
const TEXT_MUTED: Color32 = Color32::from_rgb(115, 115, 115);
```

---

## 12. Command Palette

### 12.1 UI y Layout

```
┌─────────────────────────────────────┐
│  > Type a command...                │
│─────────────────────────────────────│
│  ▸ New Terminal          Ctrl+Shift+T│
│    Close Terminal        Ctrl+Shift+W│
│    Rename Terminal       F2          │
│    ...                               │
└─────────────────────────────────────┘
```

**Constantes visuales:**
- Ancho: 500px (o ancho de pantalla - 40px si es menor), posicionado a 80px del tope
- Background: `rgb(24, 24, 28)` con borde de 1px `rgb(55, 55, 65)`, rounding de 10px
- Backdrop: overlay full-screen `rgba(0, 0, 0, 150)` (click para cerrar)
- Prompt de búsqueda: `>` en `rgb(130, 130, 200)`, monospace 14px input
- Alto de fila: 32px con highlight de selección inset de 6px
- Fila seleccionada: background `rgb(45, 45, 62)`
- Badges de shortcut: monospace 10.5px en pill redondeado `rgb(35, 35, 42)` con borde
- Máximo 10 items visibles

### 12.2 Fuzzy Matching Algorithm (Scoring Completo)

```rust
// src/command_palette/fuzzy.rs (68 líneas)

/// Calcula un score de coincidencia fuzzy
/// Retorna None si no todos los caracteres del query están en orden
pub fn fuzzy_score(query: &str, candidate: &str) -> Option<i32> {
    if query.is_empty() { return Some(0); }
    
    let query_lower: Vec<char> = query.to_lowercase().chars().collect();
    let candidate_chars: Vec<char> = candidate.chars().collect();
    let candidate_lower: Vec<char> = candidate.to_lowercase().chars().collect();
    
    let mut score: i32 = 0;
    let mut qi = 0;                // Índice en el query
    let mut prev_matched = false;  // ¿El carácter anterior fue match?
    let mut consecutive = 0;       // Longitud de la racha consecutiva actual
    
    // Separadores de palabra
    let word_separators = [' ', '/', '-', '_', ':', '\\'];
    
    for (ci, &cc) in candidate_lower.iter().enumerate() {
        if qi < query_lower.len() && cc == query_lower[qi] {
            // ¡Match!
            score += 1;  // Base: +1 por carácter matcheado
            
            // Bonus por match consecutivo
            if prev_matched {
                consecutive += 1;
                score += 5;  // +5 por cada consecutivo
            } else {
                // Penalidad por gap (romper racha consecutiva)
                if qi > 0 {
                    score -= 1;  // -1 al romper una racha
                }
                consecutive = 0;
            }
            
            // Bonus por límite de palabra
            if ci == 0 {
                score += 15;  // +15 si es inicio de string
            } else if word_separators.contains(&candidate_chars[ci - 1]) {
                score += 10;  // +10 si es inicio de palabra
            }
            
            // Bonus por coincidencia exacta de mayúsculas/minúsculas
            if candidate_chars[ci] == query.chars().nth(qi).unwrap() {
                score += 1;  // +1 por case match exacto
            }
            
            prev_matched = true;
            qi += 1;
        } else {
            prev_matched = false;
        }
    }
    
    // ¿Se matchearon todos los caracteres del query?
    if qi == query_lower.len() {
        Some(score)
    } else {
        None  // No es un match válido
    }
}
```

**Resumen de scoring:**
| Bonus/Penalidad | Puntos |
|---|---|
| Carácter matcheado (base) | +1 |
| Match consecutivo | +5 |
| Inicio de palabra (después de separador) | +10 |
| Inicio de string | +15 |
| Coincidencia exacta de case | +1 |
| Gap (romper racha consecutiva) | -1 |

### 12.3 Los 13 Comandos Registrados

| Comando | Shortcut | Acción |
|---|---|---|
| New Terminal | `Ctrl+Shift+T` | Spawn nuevo terminal en workspace activo |
| Close Terminal | `Ctrl+Shift+W` | Cerrar terminal enfocado |
| Rename Terminal | `F2` | Abrir diálogo de rename |
| Focus Next | `Ctrl+Shift+]` | Ciclar foco al siguiente |
| Focus Prev | `Ctrl+Shift+[` | Ciclar foco al anterior |
| Zoom to Fit All | `Ctrl+Shift+0` | Auto-fit todos los paneles en vista |
| Toggle Sidebar | `Ctrl+B` | Mostrar/ocultar sidebar |
| Toggle Minimap | `Ctrl+M` | Mostrar/ocultar minimap |
| Toggle Grid | `Ctrl+G` | Mostrar/ocultar grid de puntos |
| Zoom In | `Ctrl+=` | Incrementar zoom |
| Zoom Out | `Ctrl+-` | Decrementar zoom |
| Reset Zoom | `Ctrl+0` | Volver a zoom 1.0x |
| Toggle Fullscreen | `F11` | Fullscreen nativo |

### 12.4 Keyboard Navigation

- `↑/↓` — navegar items (wrap around)
- `Enter` — ejecutar comando seleccionado
- `Escape` — cerrar paleta
- Filtrado fuzzy en vivo en cada tecla

---

## 13. Minimap

### 13.1 Layout y Rendering

```rust
// src/canvas/minimap.rs (181 líneas)

// Constantes
const MINIMAP_WIDTH: f32 = 200.0;
const MINIMAP_HEIGHT: f32 = 150.0;
const MINIMAP_PADDING: f32 = 10.0;
const MINIMAP_BG: Color32 = Color32::from_rgba_premultiplied(15, 15, 15, 200);
const MINIMAP_VIEWPORT_BORDER: Color32 = Color32::from_rgb(100, 100, 100);
```

| Feature | Detalle |
|---|---|
| Tamaño | 200×150px, 10px padding de la esquina |
| Representación de paneles | Rectángulos coloreados (70% brillo del color del panel) |
| Indicador de viewport | Rectángulo con stroke mostrando el área visible actual |
| Navegación | Click/drag en minimap navega el canvas principal a esa posición |
| Label de zoom | Porcentaje de zoom actual mostrado abajo |
| Botón de ocultar | ✕ en esquina superior derecha |
| Auto-bounds | Computa bounding box de todos los paneles + viewport visible + 100px padding, preserva aspect ratio |

### 13.2 Resultado del Minimap

```rust
pub struct MinimapResult {
    pub navigate_to: Option<Pos2>,  // Posición canvas a la que navegar
    pub hide_clicked: bool,         // Si se clickeó el botón de cerrar
}
```

### 13.3 Click Navigation

```rust
// Convertir coordenadas del minimap a coordenadas del canvas
fn minimap_to_canvas(
    minimap_pos: Pos2,
    minimap_rect: Rect,
    canvas_bounds: Rect,
) -> Pos2 {
    let normalized = Pos2::new(
        (minimap_pos.x - minimap_rect.left()) / minimap_rect.width(),
        (minimap_pos.y - minimap_rect.top()) / minimap_rect.height(),
    );
    Pos2::new(
        canvas_bounds.left() + normalized.x * canvas_bounds.width(),
        canvas_bounds.top() + normalized.y * canvas_bounds.height(),
    )
}
```

---

## 14. Sistema de Persistencia

### 14.1 Esquema de Datos Completo

#### AppState (raíz)

```rust
#[derive(Serialize, Deserialize)]
pub struct AppState {
    pub workspaces: Vec<WorkspaceState>,
    pub active_ws: usize,
    pub sidebar_visible: bool,
    pub show_grid: bool,
    pub show_minimap: bool,
}
```

| Campo | Tipo | Descripción |
|---|---|---|
| `workspaces` | `Vec<WorkspaceState>` | Array de todos los workspaces |
| `active_ws` | `usize` | Índice del workspace activo |
| `sidebar_visible` | `bool` | Si el sidebar está visible |
| `show_grid` | `bool` | Si la grid está visible |
| `show_minimap` | `bool` | Si el minimap está visible |

#### WorkspaceState

```rust
#[derive(Serialize, Deserialize)]
pub struct WorkspaceState {
    pub id: String,
    pub name: String,
    pub cwd: Option<PathBuf>,
    pub panels: Vec<PanelState>,
    pub viewport_pan: [f32; 2],
    pub viewport_zoom: f32,
    pub next_z: u32,
    pub next_color: usize,
}
```

| Campo | Tipo | Descripción |
|---|---|---|
| `id` | `String` | UUID v4 del workspace |
| `name` | `String` | Nombre visible (ej. "Default", nombre de directorio) |
| `cwd` | `Option<PathBuf>` | Directorio de trabajo del workspace |
| `panels` | `Vec<PanelState>` | Array de paneles terminales |
| `viewport_pan` | `[f32; 2]` | Offset de pan `[x, y]` del viewport |
| `viewport_zoom` | `f32` | Nivel de zoom (0.125 a 4.0) |
| `next_z` | `u32` | Siguiente z-index a asignar |
| `next_color` | `usize` | Índice del siguiente color de panel |

#### PanelState

```rust
#[derive(Serialize, Deserialize)]
pub struct PanelState {
    pub title: String,
    pub position: [f32; 2],
    pub size: [f32; 2],
    pub color: [u8; 3],
    pub z_index: u32,
    pub focused: bool,
}
```

| Campo | Tipo | Descripción |
|---|---|---|
| `title` | `String` | Título del terminal |
| `position` | `[f32; 2]` | Posición `[x, y]` en espacio canvas |
| `size` | `[f32; 2]` | Tamaño `[width, height]` en espacio canvas |
| `color` | `[u8; 3]` | Color RGB de la barra de título |
| `z_index` | `u32` | Orden de apilamiento |
| `focused` | `bool` | Si el panel está enfocado |

#### Relaciones

```
AppState (1)
  ├── workspaces (1:N) → WorkspaceState
  │     ├── panels (1:N) → PanelState
  │     └── cwd (0:1) → PathBuf
  ├── active_ws → referencia por índice a workspaces
  └── configuración global (sidebar, grid, minimap)
```

### 14.2 Save/Load con serde_json

#### Lectura (al iniciar la app)

```rust
pub fn load_state() -> Option<AppState> {
    let path = state_file_path()?;
    let data = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&data).ok()
}
```

#### Escritura (al cerrar la app)

```rust
pub fn save_state(state: &AppState) {
    let Some(path) = state_file_path() else {
        log::warn!("Could not determine state file path");
        return;
    };
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            log::warn!("Failed to create state directory: {e}");
            return;
        }
    }
    match serde_json::to_string_pretty(state) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&path, json) {
                log::warn!("Failed to write state file: {e}");
            }
        }
        Err(e) => log::warn!("Failed to serialize state: {e}"),
    }
}
```

#### Generación del Snapshot

```rust
fn snapshot_state(&self) -> AppState {
    let workspaces: Vec<_> = self.workspaces.iter().enumerate().map(|(i, ws)| {
        let mut saved = ws.to_saved();
        if i == self.active_ws {
            saved.viewport_pan = [self.viewport.pan.x, self.viewport.pan.y];
            saved.viewport_zoom = self.viewport.zoom;
        }
        saved
    }).collect();
    
    AppState {
        workspaces,
        active_ws: self.active_ws,
        sidebar_visible: self.sidebar_visible,
        show_grid: self.show_grid,
        show_minimap: self.show_minimap,
    }
}
```

### 14.3 Rutas por Sistema Operativo

| Sistema Operativo | Ruta |
|---|---|
| Linux | `~/.local/share/terminal-app/layout.json` |
| macOS | `~/Library/Application Support/terminal-app/layout.json` |
| Windows | `C:\Users\<user>\AppData\Local\terminal-app\data\layout.json` |

La ruta se determina usando la crate `directories` v5:

```rust
fn state_file_path() -> Option<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "terminal-app")?;
    let data_dir = dirs.data_dir();
    Some(data_dir.join("layout.json"))
}
```

### 14.4 Qué se Guarda y Qué No

**Se guarda:**
- Posiciones, tamaños, colores, z-order de paneles
- Estado del viewport (pan + zoom) por workspace
- Nombres y CWD de workspaces
- Visibilidad de sidebar, grid, minimap

**NO se guarda:**
- Estado de procesos PTY (se respawn al restaurar)
- Contenido del terminal / scrollback
- Estado del shell
- Selección de texto activa
- Posiciones virtuales de drag/resize

### 14.5 JSON de Ejemplo

```json
{
  "workspaces": [{
    "id": "550e8400-e29b-41d4-a716-446655440000",
    "name": "Default",
    "cwd": null,
    "panels": [{
      "title": "Terminal",
      "position": [50.0, 50.0],
      "size": [1904.0, 720.0],
      "color": [90, 130, 200],
      "z_index": 0,
      "focused": true
    }],
    "viewport_pan": [100.0, 50.0],
    "viewport_zoom": 0.75,
    "next_z": 1,
    "next_color": 1
  }],
  "active_ws": 0,
  "sidebar_visible": true,
  "show_grid": true,
  "show_minimap": true
}
```

### 14.6 Sin Migraciones

No existe sistema de migraciones. El formato JSON se deserializa con `serde_json::from_str(&data).ok()` — si el formato cambia y la deserialización falla, retorna `None` y se crea un workspace por defecto.

---

## 15. Sistema de Auto-Update

### 15.1 GitHub Releases API Integration

```rust
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const RELEASES_URL: &str = "https://api.github.com/repos/{owner}/{repo}/releases/latest";
const REQUEST_TIMEOUT: u64 = 15;  // segundos

fn check_latest_release() -> Result<UpdateState, String> {
    let resp = minreq::get(RELEASES_URL)
        .with_header("User-Agent", "mi-terminal")
        .with_header("Accept", "application/vnd.github+json")
        .with_timeout(REQUEST_TIMEOUT)
        .send()
        .map_err(|e| format!("HTTP request failed: {e}"))?;

    if resp.status_code != 200 {
        return Err(format!("GitHub API returned {}", resp.status_code));
    }

    let json: serde_json::Value = serde_json::from_str(
        resp.as_str().map_err(|e| format!("UTF-8 error: {e}"))?
    ).map_err(|e| format!("JSON parse failed: {e}"))?;

    let tag = json.get("tag_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "No tag_name in response".to_string())?;

    let latest = tag.strip_prefix('v').unwrap_or(tag);
    let update_available = version_newer(latest, CURRENT_VERSION);
    let download_url = find_platform_asset(&json);

    Ok(UpdateState {
        latest_version: Some(latest.to_string()),
        download_url,
        installer_path: None,
        status: if update_available {
            UpdateStatus::Available
        } else {
            UpdateStatus::UpToDate
        },
    })
}
```

### 15.2 Platform Asset Detection

```rust
fn find_platform_asset(json: &serde_json::Value) -> Option<String> {
    let assets = json.get("assets")?.as_array()?;
    let arch = if cfg!(target_arch = "aarch64") { "aarch64" } else { "x86_64" };

    assets.iter().find_map(|asset| {
        let name = asset.get("name")?.as_str()?.to_lowercase();
        let url = asset.get("browser_download_url")
            .and_then(|u| u.as_str())
            .map(|s| s.to_string());

        if !name.contains(arch) || !name.contains("setup") { return None; }

        #[cfg(target_os = "windows")]
        { if name.ends_with(".exe") { return url; } }
        
        #[cfg(target_os = "macos")]
        { if name.ends_with(".dmg") && (name.contains("darwin") || name.contains("apple")) { return url; } }
        
        #[cfg(target_os = "linux")]
        { if name.contains("linux") && name.ends_with(".tar.gz") { return url; } }
        
        None
    })
}
```

### 15.3 SHA-256 Checksum Verification

```rust
fn verify_checksum(file_path: &std::path::Path, expected_hash: &str) -> bool {
    let Ok(mut file) = std::fs::File::open(file_path) else { return false; };
    let mut hasher = Sha256::new();
    if std::io::copy(&mut file, &mut hasher).is_err() { return false; }
    let computed = format!("{:x}", hasher.finalize());
    computed == expected_hash.trim().to_lowercase()
}

fn download_checksum(url: &str) -> Option<String> {
    let resp = minreq::get(url)
        .with_header("User-Agent", "mi-terminal")
        .with_timeout(REQUEST_TIMEOUT)
        .send().ok()?;
    
    if resp.status_code != 200 { return None; }
    
    let text = resp.as_str().ok()?.trim().to_string();
    // Validar formato: exactamente 64 caracteres hex
    let hash = text.split_whitespace().next()?;
    if hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(hash.to_lowercase())
    } else {
        None
    }
}
```

### 15.4 Instaladores por Plataforma

#### Windows (CMD Script)

```cmd
@echo off
:waitloop
tasklist /FI "PID eq {PID}" 2>NUL | find /I "{PID}" >NUL
if "%ERRORLEVEL%"=="0" (
    timeout /t 1 /nobreak >NUL
    goto waitloop
)
"{installer_path}" /S
start "" "{app_path}"
del "%~f0"
```

#### macOS (Bash Script)

```bash
#!/bin/bash
while kill -0 {PID} 2>/dev/null; do sleep 0.5; done
hdiutil attach "{dmg_path}" -nobrowse -quiet
cp -R "/Volumes/{volume_name}/{app_name}.app" "{install_dir}/"
hdiutil detach "/Volumes/{volume_name}" -quiet
open "{install_dir}/{app_name}.app"
rm -f "{dmg_path}"
rm -f "$0"
```

#### Linux (Bash Script)

```bash
#!/bin/bash
while kill -0 {PID} 2>/dev/null; do sleep 0.5; done
TMP=$(mktemp -d)
tar -xzf "{tar_path}" -C "$TMP"
cp "$TMP/mi-terminal" "{install_path}"
chmod +x "{install_path}"
nohup "{install_path}" &>/dev/null &
rm -rf "$TMP" "{tar_path}"
rm -f "$0"
```

### 15.5 State Machine del Updater

```
Checking → UpToDate
         → Available → Downloading → Ready → Installing → [app se reinicia]
         → Error(String)
```

```rust
pub enum UpdateStatus {
    Checking,
    UpToDate,
    Available,
    Downloading,
    Ready,
    Installing,
    Error(String),
}

pub struct UpdateState {
    pub latest_version: Option<String>,
    pub download_url: Option<String>,
    pub installer_path: Option<PathBuf>,
    pub status: UpdateStatus,
}
```

### 15.6 UI Integration

El updater se integra en el sidebar mostrando diferentes estados:
- **Checking**: Versión en texto gris muted
- **UpToDate**: Versión en texto gris muted
- **Available**: Icono de flecha verde (20×20 custom-drawn) que se puede clickear
- **Downloading**: "Downloading..." en texto amarillo
- **Ready**: Botón "Update" verde
- **Installing**: "Installing..." en texto verde
- **Error**: Versión en texto gris muted (degrada silenciosamente)

---

## 16. Fuentes y Tipografía

### 16.1 Sistema de Fuentes por Plataforma

```rust
fn setup_fonts(cc: &eframe::CreationContext) {
    let mut fonts = egui::FontDefinitions::default();
    
    // Fuentes del sistema como fallback para Unicode (box-drawing, símbolos)
    #[cfg(target_os = "windows")]
    {
        // Segoe UI Symbol, Segoe UI
        if let Ok(data) = std::fs::read("C:\\Windows\\Fonts\\seguisym.ttf") {
            fonts.font_data.insert("segoe_symbol".into(), egui::FontData::from_owned(data));
            fonts.families.get_mut(&FontFamily::Monospace).unwrap().push("segoe_symbol".into());
            fonts.families.get_mut(&FontFamily::Proportional).unwrap().push("segoe_symbol".into());
        }
    }
    
    #[cfg(target_os = "macos")]
    {
        // Apple Symbols, Menlo
        if let Ok(data) = std::fs::read("/System/Library/Fonts/Apple Symbols.ttf") {
            fonts.font_data.insert("apple_symbols".into(), egui::FontData::from_owned(data));
            fonts.families.get_mut(&FontFamily::Monospace).unwrap().push("apple_symbols".into());
            fonts.families.get_mut(&FontFamily::Proportional).unwrap().push("apple_symbols".into());
        }
    }
    
    #[cfg(target_os = "linux")]
    {
        // Noto Sans Mono, DejaVu Sans Mono (dos variantes de ruta)
        let paths = [
            "/usr/share/fonts/truetype/noto/NotoSansMono-Regular.ttf",
            "/usr/share/fonts/noto/NotoSansMono-Regular.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
            "/usr/share/fonts/dejavu/DejaVuSansMono.ttf",
        ];
        for path in &paths {
            if let Ok(data) = std::fs::read(path) {
                fonts.font_data.insert("system_mono".into(), egui::FontData::from_owned(data));
                fonts.families.get_mut(&FontFamily::Monospace).unwrap().push("system_mono".into());
                break;
            }
        }
    }
    
    cc.egui_ctx.set_fonts(fonts);
}
```

### 16.2 Tamaños de Fuente por Componente

| Componente | Familia | Tamaño |
|---|---|---|
| Terminal text | `FontId::monospace()` | 18.0 (`FONT_SIZE`) |
| Title bar text | `FontId::monospace()` | 13.0 (per-char rendering) |
| Sidebar labels | `FontId::proportional()` | 11.0–12.0 |
| Sidebar hints | `FontId::proportional()` | 9.5 |
| Command palette search | `FontId::monospace()` | 14.0 |
| Command palette labels | `FontId::proportional()` | 13.0 |
| Command palette shortcuts | `FontId::monospace()` | 10.5 |
| Status bar | Proporcional | 11.0 |

### 16.3 Métricas de Celda del Terminal

```rust
// Estimaciones para cálculo de grid
const CELL_WIDTH_FACTOR: f32 = 0.6;   // Ancho de celda ≈ 0.6 * font_size
const CELL_HEIGHT_FACTOR: f32 = 1.25;  // Alto de celda ≈ 1.25 * font_size

// Métricas reales medidas con ctx.fonts()
fn measure_cell(ctx: &egui::Context, font_size: f32) -> (f32, f32) {
    let font_id = FontId::monospace(font_size);
    let galley = ctx.fonts(|f| f.layout_no_wrap("M".into(), font_id, Color32::WHITE));
    (galley.size().x, galley.size().y)
}

fn compute_grid_size(content_width: f32, content_height: f32) -> (u16, u16) {
    let cell_w = FONT_SIZE * CELL_WIDTH_FACTOR;
    let cell_h = FONT_SIZE * CELL_HEIGHT_FACTOR;
    let cols = ((content_width - PAD_X * 2.0) / cell_w).floor().max(1.0) as u16;
    let rows = ((content_height - PAD_Y * 2.0) / cell_h).floor().max(1.0) as u16;
    (cols, rows)
}
```

---

## 17. Sistema de Colores Completo

### 17.1 Paleta ANSI 16 Colores (Valores RGB)

```
Normal:
  Black:   (0, 0, 0)           Bright Black:   (85, 87, 83)
  Red:     (204, 0, 0)         Bright Red:     (239, 41, 41)
  Green:   (78, 154, 6)        Bright Green:   (138, 226, 52)
  Yellow:  (196, 160, 0)       Bright Yellow:  (252, 233, 79)
  Blue:    (52, 101, 164)      Bright Blue:    (114, 159, 207)
  Magenta: (117, 80, 123)      Bright Magenta: (173, 127, 168)
  Cyan:    (6, 152, 154)       Bright Cyan:    (52, 226, 226)
  White:   (211, 215, 207)     Bright White:   (238, 238, 236)
```

### 17.2 256 Colores

- **Índices 0-15**: Paleta ANSI de 16 colores (ver arriba)
- **Índices 16-231**: Cubo de color 6×6×6
  - Fórmula: `r*40+55` para r>0, else 0 (igual para g y b)
  - `idx = 16 + r*36 + g*6 + b` donde r,g,b ∈ {0,1,2,3,4,5}
- **Índices 232-255**: Rampa de grises
  - Fórmula: `(idx - 232) * 10 + 8`
  - Rango: 8 a 238

### 17.3 Truecolor (24-bit)
Mapeo directo: `Color::Spec(rgb)` → `Color32::from_rgb(r, g, b)`

### 17.4 Colores de la UI

#### Jerarquía de Backgrounds

```
Window/Canvas:    rgb(10, 10, 10)    — fondo del canvas
Sidebar:          rgb(23, 23, 23)    — fondo del sidebar
Panel Terminal:   rgb(17, 17, 17)    — fondo del panel
Input:            rgb(39, 39, 42)    — barra de pestañas, fondos de items
Active Tab:       rgb(63, 63, 70)    — indicador de pestaña seleccionada
Command Palette:  rgb(24, 24, 28)    — fondo de la paleta
```

#### Jerarquía de Texto

```
Primary:      rgb(255, 255, 255)  — WHITE
Secondary:    rgb(163, 163, 163)  — labels, items inactivos
Muted:        rgb(115, 115, 115)  — hints, shortcuts
Terminal FG:  rgb(200, 200, 200)  — texto por defecto del terminal
Dim FG:       rgb(90, 90, 90)     — texto de terminal terminado
```

#### Bordes y Divisores

```
Default:      rgb(40, 40, 40)     — borde de panel sin foco
Focus:        rgb(70, 70, 70)     — borde de panel con foco
Bell:         rgb(255, 200, 80)   — borde durante bell flash
Divider:      rgb(38, 38, 38)     — separadores de sección del sidebar
Snap Guide:   rgba(100, 160, 255, 150)  — líneas de guía de snap
```

### 17.5 Colores de Panel (8 Colores)

```
Azul:     rgb(90, 130, 200)
Rojo:     rgb(200, 90, 90)
Verde:    rgb(90, 180, 90)
Amarillo: rgb(200, 160, 60)
Morado:   rgb(150, 90, 200)
Rosa:     rgb(200, 120, 160)
Teal:     rgb(80, 170, 200)
Olive:    rgb(180, 180, 80)
```

### 17.6 Dim Colors

Función para calcular colores dim (atributo DIM del terminal):

```rust
fn dim_color(color: Color32) -> Color32 {
    // Multiplicar cada canal por 2/3 (≈67% brillo)
    Color32::from_rgb(
        (color.r() as u16 * 2 / 3) as u8,
        (color.g() as u16 * 2 / 3) as u8,
        (color.b() as u16 * 2 / 3) as u8,
    )
}
```

### 17.7 Valores de Rounding

```
Panel border:      10px
Tab bar:            8px
Tab indicator:      6px
List items:         8px
Command palette:   10px
Minimap:            4px
Shortcut badges:    4px
```

---

## 18. Optimizaciones de Rendimiento

### 18.1 Run-Length Encoding de Backgrounds

El renderizador del terminal agrupa celdas consecutivas con el mismo color de fondo en una sola llamada de dibujo de rectángulo. Esto reduce dramáticamente el número de draw calls para salida típica de terminal.

```rust
// En vez de dibujar un rectángulo por celda:
// ████████████████  (N rectángulos para N celdas)

// Se agrupa en runs:
// ████████████████  (1 rectángulo para N celdas del mismo color)
```

### 18.2 Screen-Space Text Rendering

Todo el texto del terminal se renderiza en espacio de pantalla (no espacio canvas), usando `FONT_SIZE * zoom` como tamaño de fuente. Esto asegura texto nítido a cualquier nivel de zoom, eliminando la borrosidad que ocurriría con escalado de textura.

### 18.3 Viewport Frustum Culling

```rust
// Solo renderiza paneles visibles en el viewport
if !self.viewport.is_visible(self.ws().panels[idx].rect(), canvas_rect) {
    continue;  // Salta paneles fuera de pantalla
}
```

Los paneles fuera del viewport visible se saltan completamente durante el renderizado.

### 18.4 On-Demand Repainting

La aplicación **no** usa un loop de renderizado continuo. Se repinta solo cuando algo cambia:

- `ctx.request_repaint()` — disparado por output del PTY (reader thread)
- `ctx.request_repaint()` — disparado por eventos del PTY (title, bell, exit)
- `ctx.request_repaint()` — disparado por cambios del update checker
- `request_repaint_after(Duration::from_millis(200))` — para parpadeo del cursor

**La aplicación duerme cuando está idle** — no consume CPU/GPU sin necesidad.

### 18.5 Release Profile Optimization

```toml
[profile.release]
opt-level = 3       # Optimizaciones agresivas incluyendo vectorización
lto = true          # Optimización de todo el programa (Link-Time)
codegen-units = 1   # Máxima optimización inter-módulo (compilación más lenta)
strip = true        # Eliminar símbolos de debug (reduce tamaño del binario)
```

### 18.6 Grid Dot Limit

```rust
// Evita renderizar demasiados dots del grid al hacer zoom out
let count = ((end_x - start_x) as i64) * ((end_y - start_y) as i64);
if count > 15_000 {
    return;  // Salta el grid completo
}
```

### 18.7 Dev Profile con opt-level 1

```toml
[profile.dev]
opt-level = 1  # Optimización ligera incluso en builds de desarrollo
```

Incluso los builds de desarrollo tienen optimización nivel 1. Esto es necesario porque la aplicación renderiza una GUI a 60fps — sin optimización el rendimiento interactivo sería inaceptable.

### 18.8 Threading para I/O del PTY

Cada terminal usa 3 threads dedicados para I/O, asegurando que la lectura del PTY nunca bloquee el thread de renderizado:

```
Main Thread (egui render loop @ 60fps)
  ├── Reader Thread: PTY stdout → VTE parser → Term state machine
  ├── Event Thread: alacritty events → title/bell/clipboard/exit
  └── Waiter Thread: child.wait() → set alive=false
```

---

## 19. Manejo de Errores

### 19.1 anyhow para Propagación

El proyecto usa la crate `anyhow` para manejo de errores ergonómico:

```rust
// main() retorna anyhow::Result
fn main() -> Result<()> {
    env_logger::init();
    // ...
    eframe::run_native("Mi Terminal", options, /* ... */)
        .map_err(|e| anyhow::anyhow!("eframe error: {}", e))
}
```

### 19.2 Graceful Degradation por Escenario

| Escenario | Manejo |
|---|---|
| Icono de la app falla al cargar | `expect()` — crash al inicio (icono embebido, no debería fallar) |
| Archivo de estado faltante | Crea workspace por defecto con un terminal silenciosamente |
| Archivo de estado corrupto | `serde_json::from_str().ok()` retorna None → crea default |
| Directorio de estado no se puede crear | Log warning, continúa sin persistencia |
| PTY falla al crear | `anyhow::Result` propagado; creación de panel falla gracefully |
| Error de lectura PTY | `log::debug!`, marca terminal como muerto, UI muestra "[exited]" |
| Proceso hijo termina | Flag `alive` a false, waiter thread termina limpiamente |
| Verificación de update falla | Muestra estado de error en UI, app continúa normalmente |
| Descarga de update falla | Muestra error, app continúa con versión actual |
| Checksum no coincide | Elimina archivo descargado, retorna error, app continúa |
| Fuente del sistema no encontrada | Fallback a fuentes built-in de egui |
| Clipboard no disponible | `arboard::Clipboard::new()` retorna Err → operación omitida silenciosamente |
| GPU no disponible | Fallback a través de la cadena de backends wgpu (Vulkan → Metal → DX12 → GL) |

### 19.3 Logging con env_logger

```rust
env_logger::init();
log::info!("Starting terminal...");
```

**Niveles de log usados:**
- `log::info!` — Inicio, verificación exitosa de checksum
- `log::warn!` — Fallos no críticos (errores escritura archivo de estado, checksums faltantes)
- `log::debug!` — Errores de lectura PTY

**Habilitar con variable de entorno:**
```bash
RUST_LOG=void_terminal=debug cargo run
```

---

## 20. Testing

### 20.1 Infraestructura de Tests

- **Framework:** Sistema built-in de Rust `#[cfg(test)]` / `#[test]`
- **Sin frameworks de test externos** (no proptest, criterion, etc.)
- **Sin tests de integración** (directorio `tests/` no existe)
- **Sin benchmarks** (directorio `benches/` no existe)
- **CI ejecuta tests en las 3 plataformas:** Ubuntu, macOS, Windows

### 20.2 Tests por Módulo (5 Módulos, 15 Tests)

#### `update.rs` — 4 tests

| Test | Qué Verifica |
|---|---|
| `version_comparison` | Comparación semántica de versiones (newer/equal/older) |
| `checksum_verification_succeeds_for_matching_hash` | Verificación SHA-256 con hash conocido bueno |
| `checksum_verification_fails_for_wrong_hash` | SHA-256 rechaza datos alterados |
| `checksum_verification_handles_missing_file` | Manejo graceful de archivos inexistentes |

#### `terminal/input.rs` — 5 tests

| Test | Qué Verifica |
|---|---|
| `arrow_keys_follow_application_cursor_mode` | Flechas emiten secuencias correctas en modo app/normal |
| `modified_arrow_keys_stay_in_csi_form` | Shift+Arrow mantiene formato CSI incluso en app cursor mode |
| `copy_event_maps_to_sigint_on_non_macos` | Ctrl+C envía SIGINT cuando no hay selección (Linux/Windows) |
| `copy_event_prefers_selection_over_sigint` | Ctrl+C copia texto cuando hay selección |
| `ctrl_c_with_selection_is_copy_shortcut` | Lógica de copia aware de selección |

#### `terminal/renderer.rs` — 3 tests

| Test | Qué Verifica |
|---|---|
| `unfocused_cursor_is_hidden` | Cursor no se dibuja cuando panel no tiene foco |
| `blinking_cursor_turns_off_during_hidden_phase` | Timing de parpadeo (0.6s on / 0.4s off) |
| `streaming_output_hides_cursor` | Cursor oculto durante output rápido |

#### `canvas/viewport.rs` — 2 tests

| Test | Qué Verifica |
|---|---|
| `pan_to_center_places_canvas_point_at_screen_center` | Precisión de transformación de coordenadas |
| `zoom_around_keeps_anchor_position_stable` | Zoom preserva posición del puntero |

#### `canvas/snap.rs` — 1 test

| Test | Qué Verifica |
|---|---|
| `picks_the_closest_snap_candidate` | Motor de snap selecciona la alineación más cercana |

### 20.3 Cómo Ejecutar los Tests

```bash
# Ejecutar todos los tests
cargo test --locked

# Ejecutar con output visible
cargo test --locked -- --nocapture

# Ejecutar tests de un módulo específico
cargo test --locked update::tests
cargo test --locked terminal::input::tests
cargo test --locked terminal::renderer::tests
cargo test --locked canvas::viewport::tests
cargo test --locked canvas::snap::tests
```

---

## 21. CI/CD y Release

### 21.1 Branch Strategy

```
feature branch → PR a canary → merge en canary → merge en main → auto-release
```

| Branch | Rol |
|---|---|
| `main` | Estable. Cada push dispara release automático si la versión cambió |
| `canary` | Staging. PRs se mergean aquí primero, se prueban, luego se mergean a main |
| `fix/*`, `feat/*`, `chore/*` | Feature branches. PR hacia canary |

### 21.2 CI Workflow (`.github/workflows/ci.yml`)

Disparado en: push/PR a `main` o `canary`

**Control de concurrencia:**
```yaml
concurrency:
  group: ${{ github.workflow }}-${{ github.event.pull_request.number || github.ref }}
  cancel-in-progress: ${{ github.ref != 'refs/heads/main' }}
```

**Jobs:**

| Job | Runs On | Qué Hace | Timeout |
|---|---|---|---|
| `fmt` | `ubuntu-latest` | `cargo fmt --check` | — |
| `clippy` | ubuntu + macos + windows (matrix) | `cargo clippy --locked --all-targets --all-features -- -D warnings` | 15 min |
| `test` | ubuntu + macos + windows (matrix) | `cargo test --locked` | 15 min |
| `build` | ubuntu + macos + windows (solo push) | `cargo build --release --locked --target $TARGET` | 30 min |

- **Fail-fast:** deshabilitado (`fail-fast: false`) — todas las plataformas corren sin importar fallos
- **Caching:** `Swatinem/rust-cache@v2` con cache keys por target
- **Env:** `RUSTFLAGS: -D warnings` — todos los warnings son errores

### 21.3 Release Workflow (`.github/workflows/release.yml`)

Disparado en:
- Push a `main` (cuando `Cargo.toml` o `src/**` cambian) — **release automático**
- `workflow_dispatch` manual con override opcional de versión

**Arquitectura del pipeline:**

```
preflight → [build-windows, build-macos-arm64, build-macos-x64, build-linux] → publish
```

### 21.4 Build Matrix

| Job | Runner | Target | Artefacto |
|---|---|---|---|
| `build-windows` | `windows-latest` | `x86_64-pc-windows-msvc` | NSIS `.exe` installer |
| `build-macos-arm64` | `macos-latest` | `aarch64-apple-darwin` | `.dmg` |
| `build-macos-x64` | `macos-latest` | `x86_64-apple-darwin` | `.dmg` |
| `build-linux` | `ubuntu-22.04` | native x86_64 | `.deb` + `.tar.gz` |

### 21.5 macOS Packaging

Crea un bundle `.app` con `Info.plist`:
- Bundle ID: `com.mi.terminal`
- Minimum macOS: 11.0
- Luego empaqueta en DMG usando `hdiutil`

### 21.6 Linux Packaging

Crea ambos:
- `.tar.gz` (archivo simple de binario)
- `.deb` (con desktop entry, icono, dependencias runtime)

### 21.7 NSIS Installer (Windows)

Características del instalador:
- Wizard pages: Welcome / Directory / Install / Finish
- Shortcuts en Start Menu (`$SMPROGRAMS\MiTerminal\`)
- Desktop shortcut opcional (checkbox en página final)
- Entradas de registro Windows para Add/Remove Programs
- Desinstalador limpio
- Instalación a nivel usuario (no requiere admin)
- Icono de app incluido

### 21.8 Naming Convention de Artifacts

```
mi-terminal-{VERSION}-{ARCH}-setup.{ext}
```

Ejemplos:
- `mi-terminal-1.2.0-x86_64-setup.exe`
- `mi-terminal-1.2.0-aarch64-apple-darwin-setup.dmg`
- `mi-terminal-1.2.0-x86_64-apple-darwin-setup.dmg`
- `mi-terminal-1.2.0-x86_64-linux-setup.tar.gz`
- `mi-terminal-1.2.0-x86_64-linux-setup.deb`

### 21.9 Resilient Publishing

El job de publicación usa condición `if: always()` que publica aunque algunos builds de plataforma fallen:

```yaml
if: >-
  always() &&
  needs.preflight.outputs.skip != 'true' &&
  needs.preflight.result == 'success' &&
  (needs.build-windows.result == 'success' ||
   needs.build-macos-arm64.result == 'success' ||
   needs.build-linux.result == 'success')
```

Esto significa que se crea un release siempre que al menos una plataforma compile exitosamente.

### 21.10 Release Flow Completo

```
1. Bump versión en Cargo.toml en branch canary
2. cargo check (actualiza Cargo.lock)
3. Commit + push a canary
4. Esperar CI verde
5. Merge canary → main
6. Release workflow se dispara automáticamente
7. 4 jobs de build paralelos producen artefactos por plataforma
8. Job de publish crea GitHub Release con todos los artefactos
```

---

## 22. Guía Paso a Paso para Construir la App

### 22.1 Setup Inicial

```bash
# 1. Instalar Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env

# 2. Crear el proyecto
cargo new --name mi-terminal proyecto-terminal
cd proyecto-terminal

# 3. (Linux) Instalar dependencias del sistema
sudo apt-get update && sudo apt-get install -y \
    libasound2-dev libudev-dev libwayland-dev \
    libx11-dev libxcursor-dev libxi-dev libxinerama-dev \
    libxkbcommon-dev libxrandr-dev pkg-config

# 4. Crear estructura de directorios
mkdir -p src/{canvas,terminal,sidebar,command_palette,state,theme,shortcuts,utils}
mkdir -p assets
mkdir -p installer
mkdir -p .github/workflows
mkdir -p .cargo
```

### 22.2 Orden de Implementación Recomendado

El proyecto tiene dependencias entre módulos. Este es el orden recomendado de implementación:

#### Fase 1: Fundamentos (Core Framework)

1. **`Cargo.toml`** — Configurar todas las dependencias
2. **`src/main.rs`** — Entry point básico con eframe
3. **`src/app.rs`** — Struct `VoidApp` mínimo con `update()` vacío
4. **Assets** — Crear/obtener `icon.png` y `brand.png`

**Resultado:** Una ventana nativa vacía que se abre con icono.

#### Fase 2: Canvas Infinito

5. **`src/canvas/config.rs`** — Constantes del canvas
6. **`src/canvas/viewport.rs`** — Cámara con pan/zoom y transformaciones
7. **`src/canvas/scene.rs`** — Input del canvas (middle-click pan, ctrl+scroll zoom)
8. **`src/canvas/grid.rs`** — Grid de puntos de referencia
9. **`src/canvas/mod.rs`** — Declaraciones de módulo

**Resultado:** Un canvas infinito con grid de puntos, pan y zoom funcional.

#### Fase 3: Terminal Básico

10. **`src/terminal/colors.rs`** — Sistema de colores ANSI
11. **`src/terminal/pty.rs`** — Spawn de PTY con 3 threads
12. **`src/terminal/renderer.rs`** — Renderizado two-pass de celdas
13. **`src/terminal/input.rs`** — Mapeo de teclas a bytes terminal
14. **`src/terminal/panel.rs`** — TerminalPanel con chrome básico
15. **`src/terminal/mod.rs`** — Declaraciones de módulo

**Resultado:** Un terminal funcional renderizado en un panel del canvas.

#### Fase 4: Sistema de Paneles

16. **`src/panel.rs`** — CanvasPanel enum wrapper
17. **Integrar panels en `app.rs`** — Renderizar paneles en canvas con z-ordering
18. **Drag y resize** — Implementar en `terminal/panel.rs`
19. **`src/canvas/snap.rs`** — Motor de snap guides

**Resultado:** Múltiples paneles de terminal arrastrables y redimensionables con snap.

#### Fase 5: Workspaces

20. **`src/state/workspace.rs`** — Modelo Workspace con placement inteligente
21. **Integrar workspaces en `app.rs`** — Múltiples workspaces con switching

**Resultado:** Workspaces independientes con sus propios paneles y viewport.

#### Fase 6: UI Chrome

22. **`src/sidebar/mod.rs`** — Sidebar principal con tab bar
23. **`src/sidebar/workspace_list.rs`** — Vista de árbol de workspaces
24. **`src/sidebar/terminal_list.rs`** — Lista de terminales
25. **`src/command_palette/commands.rs`** — Registro de comandos
26. **`src/command_palette/fuzzy.rs`** — Algoritmo de fuzzy matching
27. **`src/command_palette/mod.rs`** — UI de la command palette
28. **`src/canvas/minimap.rs`** — Overlay del minimap

**Resultado:** UI completa con sidebar, command palette y minimap.

#### Fase 7: Persistencia y Update

29. **`src/state/persistence.rs`** — Save/load de estado JSON
30. **`src/update.rs`** — Sistema de auto-actualización

**Resultado:** La app guarda su estado y puede auto-actualizarse.

#### Fase 8: Polish

31. **Font fallback** — Sistema de fuentes por plataforma
32. **Context menus** — En paneles y sidebar
33. **Text selection** — Click, double-click, triple-click
34. **Bell flash** — Animación de flash en borde
35. **Stale mode recovery** — Limpieza de TUI modes
36. **Mouse forwarding** — SGR mouse events para apps TUI

**Resultado:** Aplicación pulida y completa.

### 22.3 Dependencias Entre Módulos

```
main.rs
  └── app.rs
        ├── canvas/ (viewport, scene, grid, minimap, snap, config)
        ├── terminal/ (panel, renderer, input, pty, colors)
        ├── panel.rs (depende de terminal/)
        ├── state/ (workspace, persistence)
        ├── sidebar/ (depende de state/, panel)
        ├── command_palette/ (depende de app.rs para execute_command)
        └── update.rs (independiente)
```

### 22.4 Tips y Consideraciones

1. **Empezar simple:** Implementar primero un terminal único sin canvas, luego agregar el canvas
2. **El rendering two-pass es crucial:** Sin RLE de backgrounds, el rendimiento será inaceptable
3. **Screen-space text:** Si renderizas texto en canvas-space, se verá borroso al hacer zoom
4. **Virtual positions para snap:** Sin posiciones virtuales, los paneles se "pegarán" a los snap points
5. **3 threads por PTY es necesario:** Un solo thread para I/O del PTY causará bloqueos del UI
6. **`request_repaint()` desde background threads:** Sin esto, el terminal no mostrará output en tiempo real
7. **Ctrl+C inteligente:** En Linux/Windows, Ctrl+C con selección debe copiar, sin selección debe enviar SIGINT
8. **`opt-level = 1` en dev:** Sin esto, el rendimiento en desarrollo es terrible
9. **`lto = true` en release:** Crucial para tamaño y rendimiento del binario final
10. **Testear en las 3 plataformas:** El manejo de PTY difiere significativamente entre Windows, macOS y Linux

---

## 23. Roadmap y Features Pendientes

### 23.1 TODOs Encontrados en el Código

Los siguientes módulos están marcados como placeholder/TODO:

| Módulo | TODO |
|---|---|
| `canvas/layout.rs` | Algoritmos de auto-layout (grid, filas, cascada) |
| `theme/builtin.rs` | Temas built-in (custom-dark, catppuccin, etc.) |
| `theme/colors.rs` | Tipos de paleta de colores + conversión |
| `theme/fonts.rs` | Carga de fuentes, gestión de atlas |
| `shortcuts/default_bindings.rs` | Mapa de keybindings por defecto configurable |
| `state/panel_state.rs` | Gestión de estado de paneles más avanzada |
| `utils/platform.rs` | Detección de plataforma "Phase 2" |

### 23.2 Features Potenciales del Roadmap

Basado en el análisis de la arquitectura y los TODOs:

1. **Sistema de Temas**
   - Temas built-in (dark, catppuccin, dracula, etc.)
   - Personalización de colores del terminal
   - Configuración de fuentes

2. **Auto-Layout**
   - Layout en grid automático
   - Layout en filas
   - Layout en cascada

3. **Nuevos Tipos de Panel**
   - Webview panel
   - Notes panel
   - El enum `CanvasPanel` ya está diseñado para extensibilidad

4. **Configuración**
   - Archivo de configuración TOML (la dependencia `toml` ya está incluida)
   - Keybindings personalizables
   - Configuración de colores del terminal

5. **Testing Adicional**
   - Tests de integración
   - Benchmarks con criterion
   - Fuzzing del parser de input
   - Tests de UI (screenshot comparison)

6. **Distribución Adicional**
   - AppImage / Flatpak para Linux
   - AUR package para Arch Linux
   - Homebrew tap para macOS
   - winget / Chocolatey para Windows

7. **Seguridad**
   - Code signing para macOS (notarización), Windows (Authenticode)
   - Certificate pinning para updates
   - Opción para deshabilitar OSC 52 clipboard
   - Permisos explícitos en archivo de estado

8. **Mejoras de UX**
   - Scrollback persistence (guardar contenido del terminal)
   - Restauración de sesión de shell
   - Búsqueda dentro del terminal (Ctrl+F)
   - Split panels (dividir un panel en dos)
   - Tabs dentro de paneles
   - Drag de paneles entre workspaces

---

## Apéndice A: Todos los Widgets egui Utilizados

| Widget egui | Usado En | Propósito |
|---|---|---|
| `Area` | app.rs, command_palette | Regiones flotantes posicionadas (capas canvas, overlays) |
| `SidePanel::left` | app.rs | Sidebar izquierdo de ancho fijo |
| `CentralPanel` | app.rs | Área principal del canvas |
| `Frame` | app.rs, sidebar, command_palette | Contenedor con estilo (fill, stroke, rounding, margin) |
| `ScrollArea` | sidebar/mod.rs | Región de contenido scrollable |
| `TextEdit::singleline` | app.rs (rename), command_palette (search) | Campos de input de texto |
| `Button` | app.rs (rename), sidebar (update), context menus | Botones click |
| `Image` | sidebar (brand logo) | Display de texturas |
| `RichText` | sidebar, command_palette | Texto con estilo (color, tamaño, peso) |
| `Painter` | everywhere | Dibujo de bajo nivel (rects, líneas, círculos, texto) |
| `interact()` | terminal/panel, sidebar | Hit-testing para regiones interactivas custom |
| `context_menu()` | terminal/panel, sidebar | Menús popup de right-click |

## Apéndice B: Todos los Tipos de Interacción

| Tipo | Ubicación | Propósito |
|---|---|---|
| `PanelInteraction` | terminal/panel.rs | Retorno de `show()`: clicked, dragging, resizing, action |
| `PanelAction` | terminal/panel.rs | Enum: `Close`, `Rename` |
| `SidebarResponse` | sidebar/mod.rs | Enum: `SwitchWorkspace`, `CreateWorkspace`, `DeleteWorkspace`, `FocusPanel`, `SpawnTerminal`, `RenamePanel`, `ClosePanel` |
| `SidebarTab` | sidebar/mod.rs | Enum: `Workspaces`, `Terminals` |
| `Command` | command_palette/commands.rs | Enum: 13 variantes para todas las acciones de la paleta |
| `CommandEntry` | command_palette/commands.rs | Entrada estática: command + label + string de shortcut |
| `InputResult` | terminal/input.rs | Output: `bytes: Vec<u8>`, `copy_selection: bool` |
| `InputMode` | terminal/input.rs | Flags de modo terminal: `app_cursor`, `bracketed_paste` |
| `SnapResult` | canvas/snap.rs | Computación de snap: `delta: Vec2`, `guides: Vec<SnapGuide>` |
| `SnapGuide` | canvas/snap.rs | Guía visual: vertical/horizontal, posición, inicio/fin |
| `MinimapResult` | canvas/minimap.rs | Navegación: `navigate_to: Option<Pos2>`, `hide_clicked: bool` |
| `ScrollbarState` | terminal/panel.rs | Computado: `history_size`, `display_offset`, `screen_lines` |
| `UpdateState` | update.rs | Estado del updater: version, url, path, status |
| `UpdateStatus` | update.rs | Enum: `Checking`, `UpToDate`, `Available`, `Downloading`, `Ready`, `Installing`, `Error(String)` |

## Apéndice C: Patrones Arquitectónicos Clave

### 1. Renderizado Dual-Layer
Los backgrounds del terminal se renderizan en espacio canvas-transformado (escalado por GPU), pero el texto se renderiza en una capa compartida `Tooltip` en espacio de pantalla (pixel-perfect a cualquier zoom). Esto elimina el jitter entre el chrome del panel y el contenido.

### 2. Acumulación de Posición Virtual
Las operaciones de drag y resize rastrean una posición "virtual" (sin snap) por separado. Los ajustes de snap se computan desde esta posición virtual, permitiendo que el movimiento acumulado del mouse escape naturalmente de las zonas de snap.

### 3. Z-Index vía Orden de Pintado
Los paneles se ordenan por `z_index` y se pintan en orden dentro de la capa compartida `Tooltip`. Como las capas de egui son FIFO, los paneles pintados después ocultan a los anteriores — sin necesidad de depth buffers.

### 4. Sistema de Paneles Extensible
El patrón del enum `CanvasPanel` permite agregar nuevos tipos de panel (Webview, Notes, etc.) sin modificar la lógica del canvas ni de interacción. Todas las operaciones agnósticas de panel pasan por el wrapper.

### 5. Placement con Gap-Filling
Los nuevos paneles de terminal usan un algoritmo de scoring basado en crecimiento del bounding box + distancia al centro, testeando cada intersección de borde como posición candidata. Esto produce layouts compactos que llenan huecos automáticamente.

---

*Este documento contiene toda la información necesaria para construir una aplicación idéntica a la analizada. Todos los fragmentos de código, constantes, colores, tamaños y algoritmos han sido extraídos directamente del análisis del código fuente.*
