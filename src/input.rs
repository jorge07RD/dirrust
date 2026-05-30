//! Traducción de eventos de teclado y mouse a acciones sobre el estado.
//!
//! Centralizar aquí el mapeo evento→acción mantiene el bucle de `main.rs` simple
//! y documenta en un solo sitio qué hace cada tecla y cada gesto del mouse.

use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use crate::app::{App, Focus, Modal, ViewMode};

/// Mapeo de teclado. Incluye estilo vim además de las flechas.
pub fn handle_key(app: &mut App, code: KeyCode, mods: KeyModifiers) {
    // Si hay un modal abierto, ABSORBE toda la entrada: la navegación normal
    // queda bloqueada hasta cerrarlo. Así nada se borra por un atajo accidental.
    if app.modal.is_some() {
        handle_modal_key(app, code, mods);
        return;
    }

    // La vista de duplicados tiene su propia navegación.
    if app.view == ViewMode::Duplicates {
        handle_dup_key(app, code, mods);
        return;
    }

    match code {
        // --- Salir ---
        KeyCode::Char('q') => app.should_quit = true,
        // Ctrl-C también sale, por costumbre.
        KeyCode::Char('c') if mods.contains(KeyModifiers::CONTROL) => app.should_quit = true,

        // --- Mover selección ---
        KeyCode::Up | KeyCode::Char('k') => app.move_selection(-1),
        KeyCode::Down | KeyCode::Char('j') => app.move_selection(1),
        KeyCode::PageUp => app.move_selection(-10),
        KeyCode::PageDown => app.move_selection(10),
        KeyCode::Char('g') => app.select_first(),
        KeyCode::Char('G') => app.select_last(),

        // --- Navegar por el árbol ---
        KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => app.enter_selected(),
        // 'subir' también con Esc, para que sea intuitivo salir hacia arriba.
        KeyCode::Left | KeyCode::Char('h') | KeyCode::Backspace | KeyCode::Esc => app.go_up(),

        // --- Cambiar de panel ---
        KeyCode::Tab => app.toggle_focus(),

        // --- Ordenar / modo / re-escanear ---
        KeyCode::Char('s') => app.cycle_sort(),
        KeyCode::Char('a') => app.toggle_size_mode(),
        KeyCode::Char('r') => app.rescan(),

        // --- Borrar: abre el modal de confirmación (no borra nada todavía) ---
        KeyCode::Char('d') => app.open_delete_modal(),

        // --- Vista de duplicados ---
        KeyCode::Char('f') => app.toggle_duplicates(),

        _ => {}
    }
}

/// Teclado en la vista de duplicados.
fn handle_dup_key(app: &mut App, code: KeyCode, mods: KeyModifiers) {
    match code {
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Char('c') if mods.contains(KeyModifiers::CONTROL) => app.should_quit = true,

        // Mover la selección entre archivos duplicados.
        KeyCode::Up | KeyCode::Char('k') => app.dup_move(-1),
        KeyCode::Down | KeyCode::Char('j') => app.dup_move(1),
        KeyCode::PageUp => app.dup_move(-10),
        KeyCode::PageDown => app.dup_move(10),

        // Borrar el archivo duplicado seleccionado (flujo de borrado seguro).
        KeyCode::Char('d') => app.open_delete_modal(),

        // Volver a la vista principal.
        KeyCode::Char('f') | KeyCode::Esc => app.toggle_duplicates(),

        _ => {}
    }
}

/// Teclado cuando hay un modal abierto.
///
/// REVISAR (borrado): este es el único camino por el que se dispara un borrado,
/// y siempre tras una pulsación explícita del usuario en el modal.
fn handle_modal_key(app: &mut App, code: KeyCode, mods: KeyModifiers) {
    // Ctrl-C sigue funcionando como salida de emergencia.
    if let KeyCode::Char('c') = code {
        if mods.contains(KeyModifiers::CONTROL) {
            app.should_quit = true;
            return;
        }
    }

    // Discriminamos el tipo de modal sin retener el préstamo de `app`.
    enum Tipo {
        Confirm,
        Message,
    }
    let tipo = match &app.modal {
        Some(Modal::ConfirmDelete(_)) => Tipo::Confirm,
        Some(Modal::Message { .. }) => Tipo::Message,
        None => return,
    };

    match tipo {
        // En un mensaje, cualquier tecla lo cierra.
        Tipo::Message => app.close_modal(),
        // En la confirmación de borrado, solo P / X / Esc tienen efecto.
        Tipo::Confirm => match code {
            KeyCode::Char('p') | KeyCode::Char('P') => app.confirm_trash(),
            KeyCode::Char('x') | KeyCode::Char('X') => app.confirm_permanent(),
            KeyCode::Esc => app.close_modal(),
            _ => {}
        },
    }
}

/// Mapeo de mouse: clic, doble clic, scroll y hover.
///
/// REVISAR: enrutado del mouse. Según en qué panel cae la celda (col, row) —
/// usando los rectángulos guardados por la UI— el evento actúa sobre la tabla o
/// sobre el treemap. El doble clic se detecta por tiempo+posición en `App`.
pub fn handle_mouse(app: &mut App, me: MouseEvent) {
    // Con un modal abierto, ignoramos el mouse: la confirmación de borrado se
    // resuelve solo por teclado (P / X / Esc) para evitar borrados por un clic.
    if app.modal.is_some() {
        return;
    }
    let (col, row) = (me.column, me.row);
    match me.kind {
        // Clic izquierdo: seleccionar; doble clic: entrar al directorio.
        MouseEventKind::Down(MouseButton::Left) => {
            let doble = app.register_click(col, row);
            if let Some(idx) = app.table_index_at(col, row) {
                // Clic en una fila de la tabla.
                app.focus = Focus::Table;
                app.selected = idx;
                if doble {
                    app.enter_selected();
                }
            } else if let Some(node) = app.treemap_node_at(col, row) {
                // Clic en un rectángulo del treemap.
                app.focus = Focus::Treemap;
                app.select_node(node);
                if doble {
                    app.enter_selected();
                }
            }
        }

        // Rueda: desplaza la selección (la tabla y el treemap comparten selección).
        MouseEventKind::ScrollDown => app.move_selection(3),
        MouseEventKind::ScrollUp => app.move_selection(-3),

        // Movimiento sin botón: hover. Resalta el tile del treemap bajo el cursor
        // (o lo limpia si el cursor está fuera del treemap).
        MouseEventKind::Moved => {
            app.hover_node = app.treemap_node_at(col, row);
        }

        _ => {}
    }
}
