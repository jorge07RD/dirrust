//! Panel esbelto de la lista de MARCADOS.
//!
//! Solo se dibuja cuando hay al menos un elemento marcado (el llamador decide el
//! alto del área). Muestra los elementos marcados (los mayores primero) con su
//! color de extensión, el total acumulado y los atajos para borrarlos o quitarlos.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::theme;
use crate::app::App;
use crate::util::{format_size, palette_rgb};

/// Dibuja el panel de marcados en `area`.
pub fn draw(frame: &mut Frame, app: &App, area: Rect) {
    let Some(tree) = app.tree() else { return };
    let (n, total) = app.marked_summary();

    // Título con el resumen y los atajos disponibles (lista "esvelta": todo a la
    // vista sin paneles extra).
    let titulo = format!(
        "marcados ({n}) · {} · [D] borrar · [Espacio] quitar",
        format_size(total)
    );

    // Cuántas filas de elementos caben (descontando los dos bordes).
    let filas = area.height.saturating_sub(2) as usize;
    let nodos = app.marked_sorted();

    let mut lineas: Vec<Line> = Vec::new();
    for &idx in nodos.iter().take(filas) {
        let Some(node) = tree.nodes.get(idx) else {
            continue;
        };
        // Color por extensión (archivos) o acento (carpetas), igual que la tabla.
        let color = if node.is_dir {
            theme::ACCENT
        } else {
            let (r, g, b) = palette_rgb(node.extension.as_deref());
            Color::Rgb(r, g, b)
        };
        let nombre = if node.is_dir {
            format!("{}/", node.name)
        } else {
            node.name.clone()
        };
        lineas.push(Line::from(vec![
            Span::styled("● ", Style::default().fg(theme::WARN)),
            Span::styled(nombre, Style::default().fg(color)),
            Span::raw("  "),
            Span::styled(format_size(node.size), Style::default().fg(theme::DIM)),
        ]));
    }
    // Si hay más de los que caben, lo indicamos en la última línea.
    if nodos.len() > filas && filas > 0 {
        lineas.pop();
        lineas.push(Line::from(Span::styled(
            format!("  … y {} más", nodos.len() - filas + 1),
            Style::default()
                .fg(theme::DIM)
                .add_modifier(Modifier::ITALIC),
        )));
    }

    let panel = Paragraph::new(lineas).block(theme::panel_colored(
        Span::styled(
            format!(" {titulo} "),
            Style::default()
                .fg(theme::WARN)
                .add_modifier(Modifier::BOLD),
        ),
        theme::WARN,
    ));
    frame.render_widget(panel, area);
}
