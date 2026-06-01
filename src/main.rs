//! dirrust — analizador de uso de disco en terminal (TUI), estilo WinDirStat.
//!
//! Este archivo contiene: el parseo de la CLI (clap), el montaje/desmontaje de
//! la terminal mediante un guard RAII (que también se restaura ante un panic),
//! y el bucle de eventos principal que orquesta el escáner y el dibujado.

mod actions;
mod app;
mod dedup;
mod input;
mod model;
mod scanner;
mod treemap;
mod ui;
mod util;

use std::io::{self, Stdout};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use app::App;
use scanner::{ScanConfig, SizeMode};

/// Argumentos de línea de comandos.
#[derive(Parser, Debug)]
#[command(
    name = "dirrust",
    version,
    about = "Analizador de uso de disco TUI estilo WinDirStat"
)]
struct Cli {
    /// Ruta a analizar (por defecto: el directorio actual).
    #[arg(value_name = "RUTA")]
    path: Option<PathBuf>,

    /// Usar tamaño aparente (lo que "dice" pesar el archivo). Es el modo por
    /// defecto.
    #[arg(long, conflicts_with = "disk")]
    apparent: bool,

    /// Usar tamaño en disco (bloques realmente ocupados, st_blocks * 512).
    #[arg(long)]
    disk: bool,

    /// Seguir enlaces simbólicos durante el escaneo (por defecto: desactivado).
    #[arg(long)]
    follow_symlinks: bool,

    /// No cruzar a otros sistemas de archivos / montajes. Activado por defecto.
    #[arg(long, default_value_t = true)]
    one_file_system: bool,

    /// Permite cruzar a otros sistemas de archivos (desactiva --one-file-system).
    #[arg(long, conflicts_with = "one_file_system")]
    cross_file_systems: bool,

    /// Desactiva la captura de mouse (clics, scroll, hover).
    #[arg(long)]
    no_mouse: bool,

    /// Activa el sombreado "cushion" del treemap (degradado 3D estilo WinDirStat).
    #[arg(long)]
    cushion: bool,

    /// Número de hilos de escaneo (0 = automático según los núcleos).
    #[arg(long, default_value_t = 0)]
    threads: usize,

    /// Relanza la app con `sudo` (para leer archivos sin permiso). Usa la ruta
    /// absoluta del propio binario, así que funciona aunque `~/.cargo/bin` no esté
    /// en el PATH de sudo. Pedirá tu contraseña antes de entrar en la interfaz.
    #[arg(long)]
    sudo: bool,
}

impl Cli {
    /// Traduce los argumentos a la configuración del escáner.
    fn into_scan_config(self) -> Result<ScanConfig> {
        // Ruta: la indicada o el directorio actual. La canonicalizamos para
        // tener una raíz absoluta y estable (las rutas de los nodos se derivan
        // de ella).
        let raw = self.path.unwrap_or_else(|| PathBuf::from("."));
        let root = std::fs::canonicalize(&raw)
            .with_context(|| format!("no se puede resolver la ruta '{}'", raw.display()))?;

        // El modo de tamaño: --disk gana; en cualquier otro caso, aparente.
        let size_mode = if self.disk {
            SizeMode::Disk
        } else {
            SizeMode::Apparent
        };

        Ok(ScanConfig {
            root,
            follow_symlinks: self.follow_symlinks,
            // one_file_system está activo salvo que se pida cruzar explícitamente.
            one_file_system: self.one_file_system && !self.cross_file_systems,
            size_mode,
            threads: self.threads,
        })
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Auto-elevación: si se pidió `--sudo` y no somos ya root, nos relanzamos a
    // través de sudo ANTES de montar la terminal (para que el prompt de
    // contraseña salga con normalidad). `reejecutar_con_sudo` reemplaza el
    // proceso, así que en el camino normal no vuelve de aquí.
    if cli.sudo && !es_root() {
        return reejecutar_con_sudo();
    }

    let no_mouse = cli.no_mouse;
    let cushion = cli.cushion;
    let config = cli.into_scan_config()?;

    // REVISAR: guard RAII de terminal. Pasamos a pantalla alternativa + raw mode
    // (y, en fases posteriores, captura de mouse). El guard restaura la terminal
    // en su `Drop`, ocurra lo que ocurra (salida normal, `?` que propaga error,
    // o panic — para el panic instalamos además un hook explícito más abajo).
    install_panic_hook(!no_mouse);
    let mut terminal = setup_terminal(!no_mouse)?;

    // Arrancamos la app (lanza el escaneo en segundo plano) y corremos el bucle.
    let mut app = App::new(config);
    app.cushion = cushion;
    let result = run_loop(&mut terminal, &mut app, !no_mouse);

    // Restauración explícita además del Drop del guard, para devolver la terminal
    // a un estado usable antes de imprimir cualquier error.
    restore_terminal(!no_mouse)?;
    result
}

extern "C" {
    /// EUID del proceso (de libc). La declaramos directamente para no añadir la
    /// dependencia `libc` solo por esto.
    fn geteuid() -> u32;
}

/// ¿Estamos corriendo como root (EUID 0)?
fn es_root() -> bool {
    // SAFETY: `geteuid` no recibe punteros ni puede fallar; solo lee el EUID.
    unsafe { geteuid() == 0 }
}

/// Relanza este mismo binario bajo `sudo`, usando su ruta ABSOLUTA (así sudo lo
/// encuentra aunque su `secure_path` no incluya `~/.cargo/bin`). Reemplaza el
/// proceso actual con `exec`: en el camino normal no retorna; solo devuelve `Err`
/// si no se pudo lanzar sudo (p. ej. no está instalado).
fn reejecutar_con_sudo() -> Result<()> {
    use std::os::unix::process::CommandExt;

    let exe =
        std::env::current_exe().context("no se pudo determinar la ruta del propio ejecutable")?;
    // Reconstruimos los argumentos del usuario QUITANDO `--sudo`, para no entrar
    // en un bucle de re-ejecución una vez ya somos root.
    let args: Vec<std::ffi::OsString> = std::env::args_os()
        .skip(1)
        .filter(|a| a != "--sudo")
        .collect();

    let err = std::process::Command::new("sudo")
        .arg(exe)
        .args(args)
        .exec();
    Err(anyhow::anyhow!(
        "no se pudo relanzar con sudo (¿está instalado?): {err}"
    ))
}

/// Guard RAII que garantiza la restauración de la terminal al destruirse.
struct TerminalGuard {
    /// Si la captura de mouse estaba activa (para desactivarla al salir).
    mouse: bool,
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        // Ignoramos errores: estamos saliendo y no hay mucho más que hacer.
        let _ = restore_terminal(self.mouse);
    }
}

/// Tipo concreto del terminal de ratatui sobre crossterm/stdout.
type Tui = Terminal<CrosstermBackend<Stdout>>;

/// Entra en modo TUI: raw mode + pantalla alternativa y, si `mouse`, captura de
/// mouse (clics, scroll, movimiento). Devuelve el `Terminal` listo para dibujar.
fn setup_terminal(mouse: bool) -> Result<Tui> {
    enable_raw_mode().context("no se pudo activar el modo raw de la terminal")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context("no se pudo entrar en pantalla alternativa")?;
    if mouse {
        // REVISAR: con la captura activa, el terminal envía eventos de mouse a
        // la app en vez de gestionarlos él (p. ej. la selección de texto deja de
        // funcionar; por eso existe la opción --no-mouse para desactivarla).
        execute!(stdout, EnableMouseCapture).context("no se pudo activar el mouse")?;
    }
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend).context("no se pudo inicializar la terminal")?;
    Ok(terminal)
}

/// Restaura la terminal al estado normal. Idempotente y tolerante a errores.
///
/// REVISAR: esta función es el punto único de restauración; la llaman el guard,
/// el hook de panic y la salida normal. Debe ser segura de invocar varias veces.
fn restore_terminal(mouse: bool) -> Result<()> {
    let mut stdout = io::stdout();
    // Desactivamos la captura de mouse antes de salir de la pantalla alternativa.
    if mouse {
        execute!(stdout, DisableMouseCapture).ok();
    }
    execute!(stdout, LeaveAlternateScreen).ok();
    disable_raw_mode().ok();
    Ok(())
}

/// Instala un hook de panic que restaura la terminal ANTES de imprimir el panic,
/// de modo que el mensaje sea legible y la terminal quede usable.
fn install_panic_hook(mouse: bool) {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Primero devolvemos la terminal a la normalidad...
        let _ = restore_terminal(mouse);
        // ...y luego dejamos que el hook por defecto imprima el panic.
        default_hook(info);
    }));
}

/// Bucle de eventos principal: dibuja, drena el escáner y procesa la entrada.
fn run_loop(terminal: &mut Tui, app: &mut App, mouse: bool) -> Result<()> {
    // El guard vive durante todo el bucle: si `?` propaga un error, su Drop
    // restaura la terminal (incluida la captura de mouse si estaba activa).
    let _guard = TerminalGuard { mouse };

    loop {
        // 1) Drenar mensajes del escáner y del cómputo de duplicados.
        app.poll_scanner();
        app.poll_dedup();

        // 2) Dibujar el frame actual.
        terminal
            .draw(|frame| ui::draw(frame, app))
            .context("fallo al dibujar el frame")?;

        // 3) Esperar eventos con un timeout. El timeout marca la cadencia de
        //    refresco (~10 fps) para ver el progreso del escaneo en vivo.
        if event::poll(Duration::from_millis(100)).context("fallo al sondear eventos")? {
            match event::read().context("fallo al leer un evento")? {
                // Solo reaccionamos a pulsaciones (no a 'release'/'repeat') para
                // evitar acciones duplicadas en terminales que las reportan.
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    input::handle_key(app, key.code, key.modifiers);
                }
                Event::Mouse(me) => input::handle_mouse(app, me),
                _ => {}
            }
        }

        if app.should_quit {
            break;
        }
    }
    Ok(())
}
