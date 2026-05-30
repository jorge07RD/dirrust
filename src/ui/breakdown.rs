//! Panel de desglose por extensión del subárbol actual.
//!
//! Muestra las extensiones que más ocupan, con un cuadradito del MISMO color que
//! usa el treemap (paleta consistente), su tamaño total y una barra proporcional
//! con gradiente. Si en la tabla hay un ARCHIVO seleccionado, su extensión se
//! resalta aquí (fila completa), para conectar ambas vistas de un vistazo.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState};
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

    // Extensión del archivo seleccionado en la tabla (si lo que hay seleccionado
    // es un archivo). `Some(ext)` donde `ext` es la extensión (None = sin ext).
    let sel_ext: Option<Option<String>> = app.tree().and_then(|tree| {
        app.selected_node().and_then(|n| {
            let node = &tree.nodes[n];
            if node.is_dir {
                None
            } else {
                Some(node.extension.clone())
            }
        })
    });

    // El mayor marca la escala de las barras.
    let max = app.breakdown.first().map(|e| e.size).unwrap_or(1).max(1);

    // Cuántas filas caben (descontando bordes).
    let filas = area.height.saturating_sub(2) as usize;

    let mut items: Vec<ListItem> = Vec::new();
    let mut resaltada: Option<usize> = None;

    for (i, ext) in app.breakdown.iter().take(filas).enumerate() {
        let (r, g, b) = palette_rgb(ext.ext.as_deref());
        let color = Color::Rgb(r, g, b);

        // ¿Es la extensión del archivo seleccionado? Marcamos su fila.
        if sel_ext.as_ref() == Some(&ext.ext) {
            resaltada = Some(i);
        }

        // Etiqueta de la extensión: ".mp4" o "(sin ext)".
        let etiqueta = match &ext.ext {
            Some(e) => format!(".{e}"),
            None => "(sin ext)".to_string(),
        };

        // Barra proporcional con gradiente verde→amarillo→rojo (estilo btop).
        let frac = ext.size as f64 / max as f64;
        let llenos = (frac * ANCHO_BARRA as f64).round() as usize;
        let barra: String = "█".repeat(llenos.min(ANCHO_BARRA));

        items.push(ListItem::new(Line::from(vec![
            Span::styled("■ ", Style::default().fg(color)),
            Span::styled(format!("{etiqueta:<10}"), Style::default().fg(color)),
            Span::raw(" "),
            Span::styled(
                format!("{:>10}", format_size(ext.size)),
                Style::default()
                    .fg(theme::TEXT)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" ({})", format_count(ext.count)),
                Style::default().fg(theme::DIM),
            ),
            Span::raw("  "),
            Span::styled(barra, Style::default().fg(theme::gradient(frac))),
        ])));
    }

    // El resaltado de `List` rellena toda la fila con `HILITE_BG` sin tapar los
    // colores del texto, así que la extensión seleccionada destaca con claridad.
    let list = List::new(items).block(block).highlight_style(
        Style::default()
            .bg(theme::HILITE_BG)
            .add_modifier(Modifier::BOLD),
    );
    let mut state = ListState::default();
    state.select(resaltada);
    frame.render_stateful_widget(list, area, &mut state);
}
