//! Panel de desglose por extensión del subárbol actual.
//!
//! Muestra las extensiones que más ocupan, con un cuadradito del MISMO color que
//! usa el treemap (paleta consistente), su tamaño total y una barra proporcional.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::theme;
use crate::app::App;
use crate::util::{format_count, format_size, palette_rgb};

/// Ancho (en celdas) de la barra proporcional.
const ANCHO_BARRA: usize = 10;

/// Dibuja el desglose por extensión en `area`.
pub fn draw(frame: &mut Frame, app: &App, area: Rect) {
    let block = theme::panel("por extensión", false);

    if app.breakdown.is_empty() {
        frame.render_widget(block, area);
        return;
    }

    // El mayor marca la escala de las barras.
    let max = app.breakdown.first().map(|e| e.size).unwrap_or(1).max(1);

    // Cuántas filas caben (descontando bordes).
    let filas = area.height.saturating_sub(2) as usize;

    let mut lineas: Vec<Line> = Vec::new();
    for ext in app.breakdown.iter().take(filas) {
        let (r, g, b) = palette_rgb(ext.ext.as_deref());
        let color = Color::Rgb(r, g, b);

        // Etiqueta de la extensión: ".mp4" o "(sin ext)".
        let etiqueta = match &ext.ext {
            Some(e) => format!(".{e}"),
            None => "(sin ext)".to_string(),
        };

        // Barra proporcional con el color de la extensión.
        let llenos = ((ext.size as f64 / max as f64) * ANCHO_BARRA as f64).round() as usize;
        let barra: String = "█".repeat(llenos.min(ANCHO_BARRA));

        lineas.push(Line::from(vec![
            Span::styled("■ ", Style::default().fg(color)),
            Span::styled(format!("{etiqueta:<10}"), Style::default().fg(color)),
            Span::raw(" "),
            Span::styled(
                format!("{:>10}", format_size(ext.size)),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" ({})", format_count(ext.count)),
                Style::default().fg(Color::Rgb(150, 150, 150)),
            ),
            Span::raw("  "),
            Span::styled(barra, Style::default().fg(color)),
        ]));
    }

    let p = Paragraph::new(lineas).block(block);
    frame.render_widget(p, area);
}
