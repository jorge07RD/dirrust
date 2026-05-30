//! Modales: confirmación de borrado y mensajes informativos.
//!
//! Se dibujan centrados y por ENCIMA del resto de la UI (usando `Clear` para
//! limpiar la zona). El contenido cambia según el estado del borrado: ruta
//! protegida, directorio no vacío a la espera de segunda confirmación, etc.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{App, BatchPrompt, DeletePrompt, Modal};
use crate::util::{format_count, format_size};

/// Dibuja el modal activo, si lo hay.
pub fn draw(frame: &mut Frame, app: &App) {
    let Some(modal) = &app.modal else { return };
    match modal {
        Modal::ConfirmDelete(p) => draw_confirm(frame, p),
        Modal::ConfirmBatch(b) => draw_batch(frame, b),
        Modal::Message {
            titulo,
            cuerpo,
            error,
        } => draw_message(frame, titulo, cuerpo, *error),
    }
}

/// Modal de confirmación de borrado por LOTES (lista de marcados).
fn draw_batch(frame: &mut Frame, b: &BatchPrompt) {
    let area = centrado(64, 45, frame.area());
    frame.render_widget(Clear, area);

    let borrables = b.items.len() - b.protegidos;
    let mut lineas: Vec<Line> = vec![
        Line::from(vec![
            Span::styled(
                "Elementos marcados: ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!("{}", b.items.len())),
        ]),
        Line::from(vec![
            Span::styled(
                "Tamaño total:       ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format_size(b.total_size),
                Style::default().fg(Color::Rgb(120, 230, 140)),
            ),
        ]),
        Line::from(format!(
            "Se borrarán {borrables}, se omiten {} protegidos.",
            b.protegidos
        )),
        Line::from(""),
    ];

    let (color, acciones) = if b.awaiting_second {
        lineas.push(Line::from(Span::styled(
            "⚠  Hay directorios NO vacíos. Pulsa [X] OTRA VEZ para BORRADO PERMANENTE.",
            Style::default()
                .fg(Color::Rgb(255, 80, 80))
                .add_modifier(Modifier::BOLD),
        )));
        lineas.push(Line::from(""));
        (
            Color::Rgb(255, 80, 80),
            Line::from(vec![
                tecla("X"),
                Span::raw(" Confirmar permanente   "),
                tecla("Esc"),
                Span::raw(" Cancelar"),
            ]),
        )
    } else {
        (
            Color::Rgb(255, 180, 60),
            Line::from(vec![
                tecla("P"),
                Span::raw(" Papelera   "),
                tecla("X"),
                Span::raw(" Permanente   "),
                tecla("Esc"),
                Span::raw(" Cancelar"),
            ]),
        )
    };
    lineas.push(acciones);

    let modal = Paragraph::new(lineas)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(color))
                .title(" Borrar marcados "),
        )
        .wrap(Wrap { trim: true });
    frame.render_widget(modal, area);
}

/// Modal de confirmación de borrado.
fn draw_confirm(frame: &mut Frame, p: &DeletePrompt) {
    let area = centrado(64, 40, frame.area());
    frame.render_widget(Clear, area); // limpia lo que haya debajo

    let mut lineas: Vec<Line> = vec![
        Line::from(vec![
            Span::styled("Elemento: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(p.path.display().to_string()),
        ]),
        Line::from(vec![
            Span::styled("Tamaño:   ", Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(
                format_size(p.size),
                Style::default().fg(Color::Rgb(120, 230, 140)),
            ),
        ]),
    ];
    if p.is_dir {
        lineas.push(Line::from(vec![
            Span::styled("Contiene: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(format!("{} archivos", format_count(p.file_count))),
        ]));
    }
    lineas.push(Line::from(""));

    // Color del marco y leyenda de acciones según el estado.
    let (color_marco, acciones): (Color, Line) = if p.protected {
        // REVISAR (guardas de seguridad): ruta protegida → borrado bloqueado.
        lineas.push(Line::from(Span::styled(
            "⚠  RUTA PROTEGIDA — el borrado está bloqueado por seguridad.",
            Style::default()
                .fg(Color::Rgb(255, 80, 80))
                .add_modifier(Modifier::BOLD),
        )));
        lineas.push(Line::from(""));
        (
            Color::Rgb(255, 80, 80),
            Line::from(vec![tecla("Esc"), Span::raw(" Cancelar")]),
        )
    } else if p.awaiting_second {
        // REVISAR (borrado): segunda confirmación para directorio no vacío.
        lineas.push(Line::from(Span::styled(
            "⚠  Directorio NO vacío. Pulsa [X] OTRA VEZ para BORRADO PERMANENTE.",
            Style::default()
                .fg(Color::Rgb(255, 80, 80))
                .add_modifier(Modifier::BOLD),
        )));
        lineas.push(Line::from(""));
        (
            Color::Rgb(255, 80, 80),
            Line::from(vec![
                tecla("X"),
                Span::raw(" Confirmar permanente   "),
                tecla("Esc"),
                Span::raw(" Cancelar"),
            ]),
        )
    } else {
        (
            Color::Rgb(255, 180, 60),
            Line::from(vec![
                tecla("P"),
                Span::raw(" Papelera   "),
                tecla("X"),
                Span::raw(" Permanente   "),
                tecla("Esc"),
                Span::raw(" Cancelar"),
            ]),
        )
    };

    lineas.push(acciones);

    let modal = Paragraph::new(lineas)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(color_marco))
                .title(" Confirmar borrado "),
        )
        .wrap(Wrap { trim: true });
    frame.render_widget(modal, area);
}

/// Modal de mensaje informativo (resultado o error).
fn draw_message(frame: &mut Frame, titulo: &str, cuerpo: &str, error: bool) {
    let area = centrado(60, 30, frame.area());
    frame.render_widget(Clear, area);

    let color = if error {
        Color::Rgb(255, 80, 80)
    } else {
        Color::Rgb(120, 230, 140)
    };
    let lineas = vec![
        Line::from(Span::styled(
            titulo.to_string(),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(cuerpo.to_string()),
        Line::from(""),
        Line::from(vec![tecla("Esc"), Span::raw(" cerrar (o cualquier tecla)")]),
    ];
    let modal = Paragraph::new(lineas)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(color))
                .title(" Aviso "),
        )
        .wrap(Wrap { trim: true });
    frame.render_widget(modal, area);
}

/// Estiliza una tecla en la leyenda de acciones.
fn tecla(k: &str) -> Span<'_> {
    Span::styled(
        format!("[{k}]"),
        Style::default()
            .fg(Color::Black)
            .bg(Color::Gray)
            .add_modifier(Modifier::BOLD),
    )
}

/// Calcula un rectángulo centrado de `pct_x` × `pct_y` por ciento sobre `area`.
fn centrado(pct_x: u16, pct_y: u16, area: Rect) -> Rect {
    let vert = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - pct_y) / 2),
            Constraint::Percentage(pct_y),
            Constraint::Percentage((100 - pct_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - pct_x) / 2),
            Constraint::Percentage(pct_x),
            Constraint::Percentage((100 - pct_x) / 2),
        ])
        .split(vert[1])[1]
}
