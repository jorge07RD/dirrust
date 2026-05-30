//! Capa de presentación (UI).
//!
//! Layout general: una cabecera (ruta actual + modo + orden + estado), un cuerpo
//! que muestra la tabla de ranking cuando el escaneo está listo (o el progreso /
//! el error en otro caso) y un pie con los atajos. La tabla vive en `table.rs`;
//! las siguientes fases añadirán aquí el treemap y el desglose por extensión.

mod breakdown;
mod duplicates;
mod modal;
mod table;
pub mod theme;
mod treemap_view;

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{App, Focus, ScanState, ViewMode};
use crate::scanner::SizeMode;
use crate::util::{format_count, format_size};

/// Punto de entrada del dibujado: reparte la pantalla y delega en cada panel.
pub fn draw(frame: &mut Frame, app: &mut App) {
    // Recalcula el desglose por extensión si hace falta (cacheado por directorio).
    app.ensure_breakdown();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // cabecera
            Constraint::Min(1),    // cuerpo
            Constraint::Length(3), // pie de atajos
        ])
        .split(frame.area());

    draw_header(frame, app, chunks[0]);
    draw_body(frame, app, chunks[1]);
    draw_footer(frame, chunks[2]);

    // El modal se dibuja al final para quedar POR ENCIMA del resto.
    modal::draw(frame, app);
}

/// Cabecera: ruta del directorio actual, modo de tamaño, orden y estado.
fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let modo = match app.size_mode {
        SizeMode::Apparent => "aparente",
        SizeMode::Disk => "en disco",
    };

    // Ruta del directorio que se está viendo (no solo la raíz): si hay árbol,
    // la reconstruimos subiendo desde el nodo actual.
    let ruta = match app.tree() {
        Some(tree) => tree
            .path_of(app.current, &app.root_path)
            .display()
            .to_string(),
        None => app.root_path.display().to_string(),
    };

    let estado = match &app.scan {
        ScanState::Scanning { files, .. } => Span::styled(
            format!(" escaneando… {} archivos ", format_count(*files)),
            Style::default().fg(theme::SEL_FG).bg(theme::WARN),
        ),
        ScanState::Ready { tree, skipped } => {
            // Resumen global del análisis, estilo WinDirStat: total + archivos.
            let base = format!(
                " {} · {} archivos ",
                format_size(tree.total_size()),
                format_count(tree.total_files())
            );
            let txt = if *skipped > 0 {
                format!("{base}· {} omitidas ", format_count(*skipped))
            } else {
                base
            };
            Span::styled(txt, Style::default().fg(theme::SEL_FG).bg(theme::GOOD))
        }
        ScanState::Failed(_) => {
            Span::styled(" error ", Style::default().fg(Color::White).bg(theme::BAD))
        }
    };

    let linea = Line::from(vec![
        Span::styled(
            "Ruta: ",
            Style::default().fg(theme::DIM).add_modifier(Modifier::BOLD),
        ),
        Span::styled(ruta, Style::default().fg(theme::TEXT)),
        Span::raw("   "),
        Span::styled(format!("[tamaño: {modo}]"), Style::default().fg(theme::ACCENT)),
        Span::raw(" "),
        Span::styled(
            format!("[orden: {}]", app.sort.label()),
            Style::default().fg(theme::ACCENT),
        ),
        Span::raw(" "),
        estado,
    ]);

    let header = Paragraph::new(linea).block(theme::panel("dirrust", true));
    frame.render_widget(header, area);
}

/// Cuerpo central: muestra la vista principal en cuanto hay árbol (parcial en
/// vivo o completo); si aún no llegó ningún snapshot, muestra el progreso.
fn draw_body(frame: &mut Frame, app: &mut App, area: Rect) {
    // En cuanto hay un árbol disponible (parcial o final) pintamos la UI normal.
    if app.tree().is_some() {
        // La vista de duplicados ocupa todo el cuerpo (solo con escaneo completo).
        if app.view == ViewMode::Duplicates {
            duplicates::draw(frame, app, area);
            return;
        }
        // Vista principal: fila superior (tabla + desglose) y treemap debajo.
        let partes = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
            .split(area);
        // La fila superior se divide en tabla (izquierda) y desglose (derecha).
        let arriba = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
            .split(partes[0]);
        let foco_treemap = app.focus == Focus::Treemap;
        table::draw(frame, app, arriba[0], !foco_treemap);
        breakdown::draw(frame, app, arriba[1]);
        treemap_view::draw(frame, app, partes[1], foco_treemap);
        return;
    }

    // Aún sin árbol: progreso (o error).
    match &app.scan {
        ScanState::Scanning { files, bytes, .. } => {
            let contenido = vec![
                Line::from(""),
                Line::from(Span::styled(
                    "Escaneando el árbol de directorios en segundo plano…",
                    Style::default().add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(format!("Archivos vistos: {}", format_count(*files))),
                Line::from(format!("Tamaño acumulado: {}", format_size(*bytes))),
                Line::from(""),
                Line::from(Span::styled(
                    "La interfaz no se bloquea: estas cifras se actualizan en vivo.",
                    Style::default().fg(theme::DIM),
                )),
            ];
            let body = Paragraph::new(contenido)
                .block(theme::panel("escaneando", true))
                .wrap(Wrap { trim: true });
            frame.render_widget(body, area);
        }
        ScanState::Failed(msg) => {
            let contenido = vec![
                Line::from(""),
                Line::from(Span::styled(
                    "No se pudo completar el escaneo:",
                    Style::default().fg(theme::BAD).add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(msg.clone()),
            ];
            let body = Paragraph::new(contenido)
                .block(theme::panel_colored(
                    Span::styled(" error ", Style::default().fg(theme::BAD)),
                    theme::BAD,
                ))
                .wrap(Wrap { trim: true });
            frame.render_widget(body, area);
        }
        // Ready siempre tiene árbol y se atendió en el retorno anticipado.
        ScanState::Ready { .. } => {}
    }
}

/// Pie con los atajos, con teclas resaltadas al estilo btop.
fn draw_footer(frame: &mut Frame, area: Rect) {
    let lbl = |t: &'static str| Span::styled(t, Style::default().fg(theme::DIM));
    let atajos = Line::from(vec![
        theme::key("↑↓"),
        lbl(" mover  "),
        theme::key("⏎"),
        lbl(" entrar  "),
        theme::key("⌫"),
        lbl(" subir  "),
        theme::key("Tab"),
        lbl(" panel  "),
        theme::key("s"),
        lbl(" orden  "),
        theme::key("a"),
        lbl(" apar/disco  "),
        theme::key("d"),
        lbl(" borrar  "),
        theme::key("f"),
        lbl(" duplicados  "),
        theme::key("r"),
        lbl(" rescan  "),
        theme::key("q"),
        lbl(" salir"),
    ]);
    let footer = Paragraph::new(atajos)
        .alignment(Alignment::Left)
        .block(theme::panel("atajos", false));
    frame.render_widget(footer, area);
}
