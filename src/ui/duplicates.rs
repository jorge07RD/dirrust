//! Vista de archivos duplicados.
//!
//! Lista los grupos de archivos idénticos (mismo contenido) con el espacio
//! recuperable, y permite seleccionar un archivo concreto para borrarlo con el
//! flujo de borrado seguro (tecla `d`). El cómputo corre en segundo plano, así
//! que aquí solo presentamos su estado (calculando / resultados / vacío).

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, Paragraph};
use ratatui::Frame;

use super::theme;
use crate::app::App;
use crate::util::{format_count, format_size};

use theme::{SEL_BG, SEL_FG};

/// Dibuja la vista de duplicados en `area`.
pub fn draw(frame: &mut Frame, app: &mut App, area: Rect) {
    let recuperable = app.dup_total_recoverable();
    let titulo = format!("duplicados — recuperable: {}", format_size(recuperable));
    let block = theme::panel(&titulo, true);

    // Estados sin lista que mostrar.
    if app.dedup_running {
        let p = Paragraph::new("Calculando duplicados en segundo plano… (hash de contenido)")
            .block(block);
        frame.render_widget(p, area);
        return;
    }
    let Some(groups) = &app.dedup_groups else {
        let p = Paragraph::new("Pulsa 'f' para calcular los duplicados.").block(block);
        frame.render_widget(p, area);
        return;
    };
    if groups.is_empty() {
        let p = Paragraph::new("No se encontraron archivos duplicados. 🎉").block(block);
        frame.render_widget(p, area);
        return;
    }

    // Construimos los items: una cabecera por grupo y una línea por archivo.
    // Llevamos un contador `file_idx` que avanza igual que `app.dup_files`, para
    // localizar qué fila de la lista corresponde al archivo seleccionado.
    let mut items: Vec<ListItem> = Vec::new();
    let mut fila_seleccionada: Option<usize> = None;
    let mut file_idx = 0usize;

    for grupo in groups.iter() {
        // Cabecera del grupo.
        items.push(ListItem::new(Line::from(vec![
            Span::styled(
                format!("▼ {} copias", format_count(grupo.paths.len() as u64)),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(
                "  ·  {} c/u  ·  recuperable {}",
                format_size(grupo.size),
                format_size(grupo.recoverable())
            )),
        ])));

        // Archivos del grupo.
        for path in grupo.paths.iter() {
            if file_idx == app.dup_sel {
                fila_seleccionada = Some(items.len());
            }
            let estilo = if file_idx == app.dup_sel {
                Style::default()
                    .bg(SEL_BG)
                    .fg(SEL_FG)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Rgb(210, 210, 210))
            };
            items.push(ListItem::new(Line::from(Span::styled(
                format!("    {}", path.display()),
                estilo,
            ))));
            file_idx += 1;
        }
    }

    let list = List::new(items).block(block);
    // Sincronizamos el scroll para que el archivo seleccionado quede visible.
    app.dup_list_state.select(fila_seleccionada);
    frame.render_stateful_widget(list, area, &mut app.dup_list_state);
}
