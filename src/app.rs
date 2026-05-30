//! Estado global de la aplicación.
//!
//! Centralizar el estado en `App` mantiene `main.rs` (el bucle de eventos)
//! delgado y facilita razonar sobre las transiciones. En la Fase 2 añadimos el
//! estado de navegación (directorio actual, selección de la tabla y criterio de
//! orden) y la capacidad de re-escanear (necesaria para `r` y para alternar el
//! modo de tamaño con `a`, que cambia los bytes computados por archivo).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use ratatui::layout::Rect;

use crate::actions::{self, DeleteMode};
use crate::dedup::{self, DedupMsg, DupGroup};
use crate::model::Tree;
use crate::scanner::{ScanConfig, ScanMsg, SizeMode};
use crate::treemap::TileRect;

/// Agregado de tamaño por extensión (para el panel de desglose).
pub struct ExtAgg {
    /// Extensión (sin punto) o `None` para archivos sin extensión.
    pub ext: Option<String>,
    /// Tamaño total agregado de esa extensión en el subárbol actual.
    pub size: u64,
    /// Número de archivos de esa extensión.
    pub count: u64,
}

/// Vista activa de la aplicación.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    /// Vista principal: tabla + desglose + treemap.
    Main,
    /// Vista de duplicados.
    Duplicates,
}

/// Ventana temporal para considerar dos clics como un doble clic.
const DOBLE_CLIC_MS: u64 = 400;

/// Datos del elemento que se va a borrar, mostrados en el modal de confirmación.
pub struct DeletePrompt {
    /// Nodo del árbol a borrar, si se conoce (en duplicados puede no resolverse).
    pub node: Option<usize>,
    /// `true` si el borrado se pidió desde la vista de duplicados (para refrescar
    /// la lista de duplicados tras borrar).
    pub from_dedup: bool,
    /// Ruta completa del elemento.
    pub path: PathBuf,
    /// Tamaño agregado.
    pub size: u64,
    /// Número de archivos que contiene (1 si es un archivo).
    pub file_count: u64,
    /// ¿Es un directorio?
    pub is_dir: bool,
    /// ¿Es un directorio NO vacío? (Requiere segunda confirmación si es permanente.)
    pub non_empty_dir: bool,
    /// ¿La ruta está protegida? (Entonces el borrado está bloqueado.)
    pub protected: bool,
    /// ¿Estamos esperando la SEGUNDA pulsación de [X] para el borrado permanente
    /// de un directorio no vacío?
    pub awaiting_second: bool,
}

/// Modal activo sobre la interfaz (bloquea la navegación normal).
pub enum Modal {
    /// Confirmación de borrado (papelera / permanente / cancelar).
    ConfirmDelete(DeletePrompt),
    /// Mensaje informativo (resultado de una acción o un error).
    Message {
        titulo: String,
        cuerpo: String,
        /// `true` si es un error (cambia el color del marco).
        error: bool,
    },
}

/// Criterio de ordenación de la tabla. `s` cicla entre ellos.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    /// Por tamaño agregado, descendente (lo más grande arriba). Por defecto.
    Size,
    /// Por nombre, ascendente (insensible a mayúsculas).
    Name,
    /// Por número de archivos, descendente.
    Files,
}

impl SortKey {
    /// Devuelve el siguiente criterio en el ciclo.
    pub fn next(self) -> SortKey {
        match self {
            SortKey::Size => SortKey::Name,
            SortKey::Name => SortKey::Files,
            SortKey::Files => SortKey::Size,
        }
    }

    /// Etiqueta legible para la barra de estado.
    pub fn label(self) -> &'static str {
        match self {
            SortKey::Size => "tamaño ↓",
            SortKey::Name => "nombre ↑",
            SortKey::Files => "archivos ↓",
        }
    }
}

/// Panel que tiene el foco del teclado. `Tab` alterna entre ellos.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// La tabla de ranking.
    Table,
    /// El treemap.
    Treemap,
}

impl Focus {
    /// Alterna al otro panel.
    pub fn toggle(self) -> Focus {
        match self {
            Focus::Table => Focus::Treemap,
            Focus::Treemap => Focus::Table,
        }
    }
}

/// Estado del escaneo actual.
pub enum ScanState {
    /// Escaneo en curso. Guardamos las cifras parciales y, en cuanto llega el
    /// primer snapshot, un árbol PARCIAL para mostrar resultados en vivo.
    Scanning {
        files: u64,
        bytes: u64,
        tree: Option<Box<Tree>>,
    },
    /// Escaneo terminado: tenemos el árbol completo.
    Ready { tree: Box<Tree>, skipped: u64 },
    /// El escaneo falló antes de poder construir el árbol.
    Failed(String),
}

/// Estado completo de la aplicación.
pub struct App {
    /// Configuración del escaneo; se conserva para poder re-escanear (`r`, `a`).
    config: ScanConfig,
    /// Ruta raíz que se está analizando.
    pub root_path: PathBuf,
    /// Modo de tamaño actual (aparente / en disco).
    pub size_mode: SizeMode,
    /// Estado del escaneo.
    pub scan: ScanState,
    /// Extremo receptor del canal del escáner (mientras escanea).
    pub scan_rx: Option<Receiver<ScanMsg>>,
    /// Señal para salir del bucle principal.
    pub should_quit: bool,

    // --- Estado de navegación (válido cuando `scan` es `Ready`) ---
    /// Índice del nodo del directorio que se está visualizando.
    pub current: usize,
    /// Fila seleccionada dentro de la lista ORDENADA de hijos del directorio.
    pub selected: usize,
    /// Criterio de orden activo.
    pub sort: SortKey,
    /// Panel con el foco del teclado.
    pub focus: Focus,
    /// Estado de la tabla de ratatui (conserva el desplazamiento entre frames).
    pub table_state: ratatui::widgets::TableState,

    // --- Estado del mouse / hit-testing (lo actualiza la UI al renderizar) ---
    /// Nodo bajo el cursor en el treemap (para resaltar y mostrar su info).
    pub hover_node: Option<usize>,
    /// Área (en celdas) donde se dibujan las FILAS de datos de la tabla.
    pub table_rows_area: Rect,
    /// Área interior (en celdas) del panel del treemap.
    pub treemap_area: Rect,
    /// Tiles de primer nivel del último treemap renderizado (para hit-testing).
    pub treemap_tiles: Vec<(usize, TileRect)>,
    /// Último clic registrado (instante + columna + fila), para el doble clic.
    last_click: Option<(Instant, u16, u16)>,
    /// Modal activo (confirmación de borrado o mensaje), si lo hay.
    pub modal: Option<Modal>,

    // --- Desglose por extensión (Fase 6) ---
    /// Para qué directorio está calculado el desglose cacheado (`None` = inválido).
    breakdown_for: Option<usize>,
    /// Desglose por extensión del subárbol actual, ordenado por tamaño desc.
    pub breakdown: Vec<ExtAgg>,

    // --- Vista y detección de duplicados (Fase 6) ---
    /// Vista activa.
    pub view: ViewMode,
    /// Grupos de duplicados ya calculados (`None` = aún no calculado).
    pub dedup_groups: Option<Vec<DupGroup>>,
    /// Receptor del cómputo de duplicados en curso.
    dedup_rx: Option<Receiver<DedupMsg>>,
    /// ¿Hay un cómputo de duplicados en marcha?
    pub dedup_running: bool,
    /// Lista plana de archivos duplicados seleccionables: (grupo, índice en grupo).
    pub dup_files: Vec<(usize, usize)>,
    /// Archivo seleccionado en la vista de duplicados (índice en `dup_files`).
    pub dup_sel: usize,
    /// Estado de la lista de duplicados (conserva el scroll entre frames).
    pub dup_list_state: ratatui::widgets::ListState,

    /// Sombreado "cushion" del treemap activado (look 3D estilo WinDirStat).
    pub cushion: bool,
}

impl App {
    /// Construye la app y lanza el primer escaneo en segundo plano.
    pub fn new(config: ScanConfig) -> Self {
        let root_path = config.root.clone();
        let size_mode = config.size_mode;
        let rx = crate::scanner::spawn(config.clone());
        App {
            config,
            root_path,
            size_mode,
            scan: ScanState::Scanning {
                files: 0,
                bytes: 0,
                tree: None,
            },
            scan_rx: Some(rx),
            should_quit: false,
            current: 0,
            selected: 0,
            sort: SortKey::Size,
            focus: Focus::Table,
            table_state: ratatui::widgets::TableState::default(),
            hover_node: None,
            table_rows_area: Rect::default(),
            treemap_area: Rect::default(),
            treemap_tiles: Vec::new(),
            last_click: None,
            modal: None,
            breakdown_for: None,
            breakdown: Vec::new(),
            view: ViewMode::Main,
            dedup_groups: None,
            dedup_rx: None,
            dedup_running: false,
            dup_files: Vec::new(),
            dup_sel: 0,
            dup_list_state: ratatui::widgets::ListState::default(),
            cushion: false,
        }
    }

    /// Alterna el panel con el foco del teclado.
    pub fn toggle_focus(&mut self) {
        self.focus = self.focus.toggle();
    }

    /// ¿El escaneo terminó por completo? (Borrado y duplicados lo requieren,
    /// porque el árbol parcial todavía está cambiando.)
    pub fn scan_complete(&self) -> bool {
        matches!(self.scan, ScanState::Ready { .. })
    }

    /// Acceso al árbol disponible: el completo si terminó, o el parcial en vivo
    /// durante el escaneo (si ya llegó el primer snapshot).
    pub fn tree(&self) -> Option<&Tree> {
        match &self.scan {
            ScanState::Ready { tree, .. } => Some(tree),
            ScanState::Scanning {
                tree: Some(tree), ..
            } => Some(tree),
            _ => None,
        }
    }

    /// Lista de hijos del directorio actual, ya ordenada según `self.sort`.
    ///
    /// Se recalcula bajo demanda: solo ordena los hijos directos del directorio
    /// visible (no todo el árbol), así que es barato incluso en árboles enormes.
    pub fn sorted_children(&self) -> Vec<usize> {
        let Some(tree) = self.tree() else {
            return Vec::new();
        };
        let mut hijos = tree.nodes[self.current].children.clone();
        let nodes = &tree.nodes;
        match self.sort {
            SortKey::Size => {
                // Tamaño desc; a igualdad, nombre asc para un orden estable.
                hijos.sort_by(|&a, &b| {
                    nodes[b].size.cmp(&nodes[a].size).then_with(|| {
                        nodes[a]
                            .name
                            .to_lowercase()
                            .cmp(&nodes[b].name.to_lowercase())
                    })
                });
            }
            SortKey::Name => {
                hijos.sort_by(|&a, &b| {
                    nodes[a]
                        .name
                        .to_lowercase()
                        .cmp(&nodes[b].name.to_lowercase())
                });
            }
            SortKey::Files => {
                hijos.sort_by(|&a, &b| {
                    nodes[b]
                        .file_count
                        .cmp(&nodes[a].file_count)
                        .then_with(|| nodes[b].size.cmp(&nodes[a].size))
                });
            }
        }
        hijos
    }

    /// Nodo actualmente seleccionado en la tabla, si lo hay.
    pub fn selected_node(&self) -> Option<usize> {
        self.sorted_children().get(self.selected).copied()
    }

    /// Mueve la selección `delta` filas, con saturación en los extremos.
    pub fn move_selection(&mut self, delta: isize) {
        let len = self.sorted_children().len();
        if len == 0 {
            self.selected = 0;
            return;
        }
        let max = len - 1;
        // Calculamos con i64 para no desbordar y luego acotamos a [0, max].
        let nuevo = (self.selected as isize + delta).clamp(0, max as isize);
        self.selected = nuevo as usize;
    }

    /// Selecciona la primera fila.
    pub fn select_first(&mut self) {
        self.selected = 0;
    }

    /// Selecciona la última fila.
    pub fn select_last(&mut self) {
        let len = self.sorted_children().len();
        self.selected = len.saturating_sub(1);
    }

    /// Entra en el directorio seleccionado (si es un directorio).
    pub fn enter_selected(&mut self) {
        let Some(idx) = self.selected_node() else {
            return;
        };
        let Some(tree) = self.tree() else { return };
        if tree.nodes[idx].is_dir {
            self.current = idx;
            self.selected = 0;
            // Al cambiar de directorio el hover anterior ya no es válido.
            self.hover_node = None;
        }
    }

    /// Sube un nivel (al directorio padre), conservando la selección sobre el
    /// directorio del que venimos para una navegación cómoda.
    pub fn go_up(&mut self) {
        let Some(tree) = self.tree() else { return };
        let Some(parent) = tree.nodes[self.current].parent else {
            return; // ya estamos en la raíz
        };
        let previo = self.current;
        self.current = parent;
        self.hover_node = None;
        // Reposicionamos la selección sobre el directorio del que subimos.
        let pos = self
            .sorted_children()
            .iter()
            .position(|&c| c == previo)
            .unwrap_or(0);
        self.selected = pos;
    }

    // --- Hit-testing del mouse ---
    // REVISAR: hit-testing. Estas funciones traducen coordenadas de celda del
    // terminal a índices de fila / nodos. Dependen de los rectángulos que la UI
    // guarda en `table_rows_area` y `treemap_area` al renderizar cada frame.

    /// Índice (en la lista ordenada) de la fila de la tabla en (col, row), si la
    /// hay. Tiene en cuenta el desplazamiento (scroll) actual de la tabla.
    pub fn table_index_at(&self, col: u16, row: u16) -> Option<usize> {
        let a = self.table_rows_area;
        if !dentro(a, col, row) {
            return None;
        }
        let rel = (row - a.y) as usize;
        let idx = self.table_state.offset() + rel;
        if idx < self.sorted_children().len() {
            Some(idx)
        } else {
            None
        }
    }

    /// Nodo del treemap bajo la celda (col, row), si lo hay.
    ///
    /// REVISAR: cada celda cubre dos píxeles verticales (medio bloque); usamos
    /// el píxel SUPERIOR (`* 2`) para localizar el tile. Basta para tiles del
    /// tamaño habitual y mantiene la correspondencia clic→nodo intuitiva.
    pub fn treemap_node_at(&self, col: u16, row: u16) -> Option<usize> {
        let a = self.treemap_area;
        if !dentro(a, col, row) {
            return None;
        }
        let px = (col - a.x) as usize;
        let py = (row - a.y) as usize * 2;
        self.treemap_tiles
            .iter()
            .find(|(_, r)| r.contains(px, py))
            .map(|(n, _)| *n)
    }

    /// Coloca la selección sobre un nodo concreto (hijo del directorio actual).
    pub fn select_node(&mut self, node: usize) {
        if let Some(pos) = self.sorted_children().iter().position(|&c| c == node) {
            self.selected = pos;
        }
    }

    /// Registra un clic y devuelve `true` si forma un doble clic con el anterior
    /// (misma celda y dentro de la ventana temporal).
    pub fn register_click(&mut self, col: u16, row: u16) -> bool {
        let now = Instant::now();
        let doble = matches!(
            self.last_click,
            Some((t, c, r))
                if c == col && r == row && now.duration_since(t) < Duration::from_millis(DOBLE_CLIC_MS)
        );
        // Tras un doble clic reiniciamos para no encadenar un triple como doble.
        self.last_click = if doble { None } else { Some((now, col, row)) };
        doble
    }

    // --- Borrado (Fase 5) ---

    /// Acceso mutable al árbol (para actualizarlo tras un borrado).
    fn tree_mut(&mut self) -> Option<&mut Tree> {
        match &mut self.scan {
            ScanState::Ready { tree, .. } => Some(tree),
            _ => None,
        }
    }

    /// Abre el modal de confirmación de borrado para el elemento seleccionado.
    ///
    /// REVISAR (borrado): nada se borra aquí; solo se recopila la información y se
    /// comprueba si la ruta está protegida para mostrarlo en el modal. Funciona
    /// tanto en la vista principal (nodo seleccionado) como en la de duplicados
    /// (archivo seleccionado por ruta).
    pub fn open_delete_modal(&mut self) {
        // No permitimos borrar hasta que el escaneo termine (el árbol parcial
        // aún cambia y los agregados no son definitivos).
        if !self.scan_complete() {
            return;
        }
        let prompt = match self.view {
            ViewMode::Main => self.delete_prompt_for_selected(),
            ViewMode::Duplicates => self.delete_prompt_for_dup(),
        };
        if let Some(p) = prompt {
            self.modal = Some(Modal::ConfirmDelete(p));
        }
    }

    /// Construye el prompt de borrado para el nodo seleccionado en la tabla.
    fn delete_prompt_for_selected(&self) -> Option<DeletePrompt> {
        let node = self.selected_node()?;
        let tree = self.tree()?;
        let path = tree.path_of(node, &self.root_path);
        let n = &tree.nodes[node];
        Some(DeletePrompt {
            node: Some(node),
            from_dedup: false,
            size: n.size,
            file_count: n.file_count,
            is_dir: n.is_dir,
            non_empty_dir: n.is_dir && n.file_count > 0,
            protected: actions::is_protected(&path),
            awaiting_second: false,
            path,
        })
    }

    /// Construye el prompt de borrado para el archivo duplicado seleccionado.
    fn delete_prompt_for_dup(&self) -> Option<DeletePrompt> {
        let (g, f) = self.dup_files.get(self.dup_sel).copied()?;
        let groups = self.dedup_groups.as_ref()?;
        let path = groups.get(g)?.paths.get(f)?.clone();
        let size = groups[g].size;
        // Intentamos localizar el nodo en el árbol para actualizar agregados.
        let node = self
            .tree()
            .and_then(|t| t.find_by_path(&self.root_path, &path));
        Some(DeletePrompt {
            node,
            from_dedup: true,
            size,
            file_count: 1,
            is_dir: false, // los duplicados son siempre archivos
            non_empty_dir: false,
            protected: actions::is_protected(&path),
            awaiting_second: false,
            path,
        })
    }

    /// Cierra el modal actual.
    pub fn close_modal(&mut self) {
        self.modal = None;
    }

    /// Confirma el envío a la papelera del elemento del modal.
    pub fn confirm_trash(&mut self) {
        // Extraemos los datos ANTES de mutar para no chocar con el préstamo.
        let info = match &self.modal {
            Some(Modal::ConfirmDelete(p)) if !p.protected => {
                Some((p.node, p.from_dedup, p.path.clone(), p.is_dir))
            }
            _ => None,
        };
        let Some((node, from_dedup, path, is_dir)) = info else {
            return;
        };
        // REVISAR (borrado): la papelera es reversible, por eso no exige segunda
        // confirmación ni siquiera para directorios no vacíos.
        match actions::delete_path(&path, is_dir, DeleteMode::Trash) {
            Ok(()) => {
                self.after_delete(node, from_dedup, &path);
                self.set_message("Enviado a la papelera", &path.display().to_string(), false);
            }
            Err(e) => self.set_message("Error al borrar", &e.to_string(), true),
        }
    }

    /// Confirma el borrado PERMANENTE del elemento del modal.
    ///
    /// REVISAR (borrado): para un directorio NO vacío exigimos una SEGUNDA
    /// pulsación de [X]; la primera solo arma la confirmación (awaiting_second).
    pub fn confirm_permanent(&mut self) {
        let info = match &self.modal {
            Some(Modal::ConfirmDelete(p)) if !p.protected => Some((
                p.node,
                p.from_dedup,
                p.path.clone(),
                p.is_dir,
                p.non_empty_dir,
                p.awaiting_second,
            )),
            _ => None,
        };
        let Some((node, from_dedup, path, is_dir, non_empty, awaiting)) = info else {
            return;
        };

        if non_empty && !awaiting {
            // Primera pulsación sobre un directorio no vacío: pedimos confirmar
            // otra vez en lugar de borrar ya.
            if let Some(Modal::ConfirmDelete(p)) = &mut self.modal {
                p.awaiting_second = true;
            }
            return;
        }

        match actions::delete_path(&path, is_dir, DeleteMode::Permanent) {
            Ok(()) => {
                self.after_delete(node, from_dedup, &path);
                self.set_message("Borrado permanente", &path.display().to_string(), false);
            }
            Err(e) => self.set_message("Error al borrar", &e.to_string(), true),
        }
    }

    /// Tareas comunes tras un borrado con éxito: actualizar árbol y, si procede,
    /// la lista de duplicados; además invalida el desglose cacheado.
    fn after_delete(&mut self, node: Option<usize>, from_dedup: bool, path: &Path) {
        if let Some(n) = node {
            self.remove_node_from_tree(n);
        }
        if from_dedup {
            self.remove_dup_path(path);
        }
        // El desglose por extensión ya no es válido tras cambiar el árbol.
        self.breakdown_for = None;
    }

    /// Sustituye el modal por un mensaje informativo.
    fn set_message(&mut self, titulo: &str, cuerpo: &str, error: bool) {
        self.modal = Some(Modal::Message {
            titulo: titulo.to_string(),
            cuerpo: cuerpo.to_string(),
            error,
        });
    }

    /// Quita un nodo del árbol y actualiza los agregados SIN re-escanear.
    ///
    /// REVISAR (actualización in-memory): desligamos el nodo de la lista de hijos
    /// de su padre y restamos su tamaño y conteo a TODOS los ancestros. No
    /// compactamos el arena (eso invalidaría otros índices `usize`); el nodo
    /// queda huérfano y simplemente deja de mostrarse.
    fn remove_node_from_tree(&mut self, node: usize) {
        let Some(tree) = self.tree_mut() else { return };
        let (size, count, parent) = {
            let n = &tree.nodes[node];
            (n.size, n.file_count, n.parent)
        };
        if let Some(p) = parent {
            tree.nodes[p].children.retain(|&c| c != node);
        }
        // Restamos hacia arriba por toda la cadena de ancestros.
        let mut cur = parent;
        while let Some(p) = cur {
            tree.nodes[p].size = tree.nodes[p].size.saturating_sub(size);
            tree.nodes[p].file_count = tree.nodes[p].file_count.saturating_sub(count);
            cur = tree.nodes[p].parent;
        }

        // La lista visible encogió: reajustamos la selección y limpiamos el hover.
        self.hover_node = None;
        let len = self.sorted_children().len();
        self.selected = self.selected.min(len.saturating_sub(1));
    }

    // --- Desglose por extensión (Fase 6) ---

    /// Recalcula el desglose por extensión del subárbol actual si está obsoleto.
    ///
    /// Se cachea por directorio: solo recorre el subárbol cuando cambia `current`
    /// o cuando el árbol se modifica (borrado/re-escaneo invalidan la caché).
    pub fn ensure_breakdown(&mut self) {
        if self.breakdown_for == Some(self.current) && self.tree().is_some() {
            return;
        }
        let Some(tree) = self.tree() else {
            self.breakdown.clear();
            return;
        };

        // Recorremos el subárbol con una pila (sin recursión) sumando por extensión.
        let mut acc: std::collections::HashMap<Option<String>, (u64, u64)> =
            std::collections::HashMap::new();
        let mut pila = vec![self.current];
        while let Some(idx) = pila.pop() {
            let n = &tree.nodes[idx];
            if n.is_dir {
                pila.extend(n.children.iter().copied());
            } else {
                let e = acc.entry(n.extension.clone()).or_insert((0, 0));
                e.0 += n.size;
                e.1 += 1;
            }
        }

        let mut v: Vec<ExtAgg> = acc
            .into_iter()
            .map(|(ext, (size, count))| ExtAgg { ext, size, count })
            .collect();
        v.sort_by_key(|a| std::cmp::Reverse(a.size));
        self.breakdown = v;
        self.breakdown_for = Some(self.current);
    }

    // --- Duplicados (Fase 6) ---

    /// Alterna entre la vista principal y la de duplicados. Al entrar por primera
    /// vez, lanza el cómputo en segundo plano.
    pub fn toggle_duplicates(&mut self) {
        match self.view {
            ViewMode::Main => {
                // La detección de duplicados necesita el árbol completo.
                if !self.scan_complete() {
                    return;
                }
                self.view = ViewMode::Duplicates;
                if self.dedup_groups.is_none() && !self.dedup_running {
                    self.start_dedup();
                }
            }
            ViewMode::Duplicates => self.view = ViewMode::Main,
        }
    }

    /// Lanza la detección de duplicados sobre todos los archivos del árbol.
    fn start_dedup(&mut self) {
        let Some(files) = self.collect_files() else {
            return;
        };
        self.dedup_rx = Some(dedup::spawn(files));
        self.dedup_running = true;
    }

    /// Recolecta (ruta, tamaño) de todos los archivos del árbol escaneado.
    fn collect_files(&self) -> Option<Vec<(PathBuf, u64)>> {
        let tree = self.tree()?;
        let mut out = Vec::new();
        for (idx, n) in tree.nodes.iter().enumerate() {
            if !n.is_dir {
                out.push((tree.path_of(idx, &self.root_path), n.size));
            }
        }
        Some(out)
    }

    /// Drena el resultado del cómputo de duplicados sin bloquear.
    pub fn poll_dedup(&mut self) {
        let Some(rx) = &self.dedup_rx else { return };
        if let Ok(DedupMsg::Done(grupos)) = rx.try_recv() {
            self.dedup_groups = Some(grupos);
            self.dedup_running = false;
            self.dedup_rx = None;
            self.dup_sel = 0;
            self.rebuild_dup_files();
        }
    }

    /// Reconstruye la lista plana de archivos duplicados seleccionables.
    fn rebuild_dup_files(&mut self) {
        self.dup_files.clear();
        if let Some(groups) = &self.dedup_groups {
            for (g, grupo) in groups.iter().enumerate() {
                for f in 0..grupo.paths.len() {
                    self.dup_files.push((g, f));
                }
            }
        }
        self.dup_sel = self.dup_sel.min(self.dup_files.len().saturating_sub(1));
    }

    /// Mueve la selección en la vista de duplicados.
    pub fn dup_move(&mut self, delta: isize) {
        if self.dup_files.is_empty() {
            self.dup_sel = 0;
            return;
        }
        let max = self.dup_files.len() - 1;
        self.dup_sel = (self.dup_sel as isize + delta).clamp(0, max as isize) as usize;
    }

    /// Espacio total recuperable sumando todos los grupos.
    pub fn dup_total_recoverable(&self) -> u64 {
        self.dedup_groups
            .as_ref()
            .map(|g| g.iter().map(|x| x.recoverable()).sum())
            .unwrap_or(0)
    }

    /// Quita una ruta ya borrada de los grupos de duplicados y recompone la lista.
    fn remove_dup_path(&mut self, path: &Path) {
        if let Some(groups) = &mut self.dedup_groups {
            for g in groups.iter_mut() {
                g.paths.retain(|p| p != path);
            }
            // Un grupo con una sola copia ya no es un duplicado; lo quitamos.
            groups.retain(|g| g.paths.len() >= 2);
        }
        self.rebuild_dup_files();
    }

    /// Cicla el criterio de orden y reajusta la selección al inicio.
    pub fn cycle_sort(&mut self) {
        self.sort = self.sort.next();
        // Tras reordenar, el índice anterior ya no tiene el mismo significado;
        // lo más predecible es volver arriba.
        self.selected = 0;
    }

    /// Alterna entre tamaño aparente y en disco, relanzando el escaneo (los
    /// bytes por archivo se calculan de forma distinta en cada modo).
    pub fn toggle_size_mode(&mut self) {
        self.size_mode = match self.size_mode {
            SizeMode::Apparent => SizeMode::Disk,
            SizeMode::Disk => SizeMode::Apparent,
        };
        self.config.size_mode = self.size_mode;
        self.restart_scan();
    }

    /// Re-escanea el árbol desde la raíz con la configuración actual.
    pub fn rescan(&mut self) {
        self.restart_scan();
    }

    /// Lanza un nuevo escaneo y resetea el estado de navegación.
    fn restart_scan(&mut self) {
        let rx = crate::scanner::spawn(self.config.clone());
        self.scan = ScanState::Scanning {
            files: 0,
            bytes: 0,
            tree: None,
        };
        self.scan_rx = Some(rx);
        // La navegación se reinicia: los índices del árbol antiguo no son válidos.
        self.current = 0;
        self.selected = 0;
        // El árbol cambia: invalidamos desglose y duplicados y volvemos a la
        // vista principal.
        self.breakdown_for = None;
        self.dedup_groups = None;
        self.dedup_rx = None;
        self.dedup_running = false;
        self.dup_files.clear();
        self.view = ViewMode::Main;
    }

    /// Drena los mensajes pendientes del escáner sin bloquear.
    ///
    /// Se llama una vez por iteración del bucle de eventos. Procesa todos los
    /// mensajes disponibles (`try_recv`) para no quedarse atrás cuando el
    /// escáner produce rápido.
    pub fn poll_scanner(&mut self) {
        let Some(rx) = &self.scan_rx else {
            return;
        };
        loop {
            match rx.try_recv() {
                Ok(ScanMsg::Progress { files, bytes }) => {
                    // Solo actualizamos las cifras; conservamos el árbol parcial.
                    if let ScanState::Scanning {
                        files: f, bytes: b, ..
                    } = &mut self.scan
                    {
                        *f = files;
                        *b = bytes;
                    }
                }
                Ok(ScanMsg::Partial { tree, files, bytes }) => {
                    // Snapshot en vivo: mostramos ya el árbol parcial. Como creció,
                    // invalidamos el desglose por extensión cacheado.
                    if matches!(self.scan, ScanState::Scanning { .. }) {
                        self.scan = ScanState::Scanning {
                            files,
                            bytes,
                            tree: Some(tree),
                        };
                        self.breakdown_for = None;
                    }
                }
                Ok(ScanMsg::Done { tree, skipped }) => {
                    // El árbol final usa los mismos índices que los snapshots
                    // (se construye añadiendo, sin reordenar), así que la posición
                    // a la que el usuario hubiera navegado en vivo sigue siendo
                    // válida; solo la acotamos por seguridad.
                    let n = tree.nodes.len();
                    self.scan = ScanState::Ready { tree, skipped };
                    self.scan_rx = None;
                    if self.current >= n {
                        self.current = 0;
                    }
                    self.breakdown_for = None;
                    let len = self.sorted_children().len();
                    self.selected = self.selected.min(len.saturating_sub(1));
                    break;
                }
                Ok(ScanMsg::Error(e)) => {
                    self.scan = ScanState::Failed(e);
                    self.scan_rx = None;
                    break;
                }
                Err(crossbeam_channel::TryRecvError::Empty) => break,
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    // El hilo murió sin enviar Done/Error: lo marcamos como fallo
                    // si aún estábamos escaneando.
                    if matches!(self.scan, ScanState::Scanning { .. }) {
                        self.scan = ScanState::Failed("el escaneo se interrumpió".into());
                    }
                    self.scan_rx = None;
                    break;
                }
            }
        }
    }
}

/// ¿La celda (col, row) cae dentro del rectángulo `a`?
fn dentro(a: Rect, col: u16, row: u16) -> bool {
    a.width > 0
        && a.height > 0
        && col >= a.x
        && col < a.x + a.width
        && row >= a.y
        && row < a.y + a.height
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_bytes(path: &std::path::Path, n: usize) {
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(&vec![b'x'; n]).unwrap();
    }

    /// Construye una App, espera a que termine el escaneo y devuelve la app lista.
    fn app_ready(root: PathBuf) -> App {
        let config = ScanConfig {
            root,
            follow_symlinks: false,
            one_file_system: true,
            size_mode: SizeMode::Apparent,
            threads: 0,
        };
        let mut app = App::new(config);
        // Bombeamos el canal hasta que el árbol esté listo (con un tope de
        // seguridad para no colgar el test si algo va mal).
        for _ in 0..10_000 {
            app.poll_scanner();
            if app.tree().is_some() {
                return app;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("el escaneo no terminó a tiempo");
    }

    #[test]
    fn orden_y_navegacion() {
        // root/
        //   big.bin   (500)
        //   small.txt (100)
        //   mid/      (= 250: m1=200 + m2=50)
        let dir = std::env::temp_dir().join(format!("dirrust_nav_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("mid")).unwrap();
        write_bytes(&dir.join("big.bin"), 500);
        write_bytes(&dir.join("small.txt"), 100);
        write_bytes(&dir.join("mid").join("m1"), 200);
        write_bytes(&dir.join("mid").join("m2"), 50);

        let mut app = app_ready(dir.clone());

        // Orden por tamaño desc en la raíz: big (500) > mid (250) > small (100).
        let nombres: Vec<String> = app
            .sorted_children()
            .iter()
            .map(|&i| app.tree().unwrap().nodes[i].name.clone())
            .collect();
        assert_eq!(nombres, vec!["big.bin", "mid", "small.txt"]);

        // big.bin es un archivo: entrar no debe cambiar el directorio actual.
        app.selected = 0;
        let raiz = app.current;
        app.enter_selected();
        assert_eq!(app.current, raiz, "entrar en un archivo no navega");

        // Entrar en 'mid' (índice 1) sí navega.
        app.selected = 1;
        app.enter_selected();
        let dir_actual = &app.tree().unwrap().nodes[app.current];
        assert_eq!(dir_actual.name, "mid");

        // Dentro de mid: m1 (200) antes que m2 (50).
        let dentro: Vec<String> = app
            .sorted_children()
            .iter()
            .map(|&i| app.tree().unwrap().nodes[i].name.clone())
            .collect();
        assert_eq!(dentro, vec!["m1", "m2"]);

        // Subir vuelve a la raíz y deja la selección sobre 'mid'.
        app.go_up();
        assert_eq!(app.current, raiz);
        assert_eq!(
            app.selected_node(),
            Some(
                app.tree().unwrap().nodes[raiz]
                    .children
                    .iter()
                    .copied()
                    .find(|&i| app.tree().unwrap().nodes[i].name == "mid")
                    .unwrap()
            )
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hit_testing_y_doble_clic() {
        use crate::treemap::TileRect;

        // Reusamos un fixture mínimo con un par de archivos.
        let dir = std::env::temp_dir().join(format!("dirrust_hit_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        write_bytes(&dir.join("a.bin"), 300);
        write_bytes(&dir.join("b.bin"), 100);
        let mut app = app_ready(dir.clone());

        // --- Tabla: una celda dentro del área de filas mapea a su índice. ---
        app.table_rows_area = Rect {
            x: 1,
            y: 2,
            width: 30,
            height: 10,
        };
        // Fila 0 (y == 2) → índice 0; fila 1 → índice 1; fuera → None.
        assert_eq!(app.table_index_at(5, 2), Some(0));
        assert_eq!(app.table_index_at(5, 3), Some(1));
        assert_eq!(app.table_index_at(5, 9), None); // no hay tantas filas
        assert_eq!(app.table_index_at(50, 2), None); // fuera en x

        // --- Treemap: un clic dentro de un tile devuelve su nodo. ---
        let nodo_a = app.sorted_children()[0]; // 'a.bin' (el mayor)
        app.treemap_area = Rect {
            x: 0,
            y: 20,
            width: 50,
            height: 10,
        };
        app.treemap_tiles = vec![(
            nodo_a,
            TileRect {
                x0: 0,
                y0: 0,
                x1: 10,
                y1: 4,
            },
        )];
        // col=2,row=20 → px=2, py=0 → dentro del tile.
        assert_eq!(app.treemap_node_at(2, 20), Some(nodo_a));
        // Fuera del tile (px=40) → None.
        assert_eq!(app.treemap_node_at(40, 20), None);

        // --- Doble clic: dos clics seguidos en la misma celda. ---
        assert!(!app.register_click(2, 20), "el primer clic no es doble");
        assert!(app.register_click(2, 20), "el segundo seguido sí es doble");
        // Tras el doble, se reinicia: el siguiente vuelve a ser simple.
        assert!(!app.register_click(2, 20));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn borrado_actualiza_agregados_in_memory() {
        // root/  (a.bin=600, b.bin=300, c.bin=100)  → total 1000, 3 archivos.
        let dir = std::env::temp_dir().join(format!("dirrust_del_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        write_bytes(&dir.join("a.bin"), 600);
        write_bytes(&dir.join("b.bin"), 300);
        write_bytes(&dir.join("c.bin"), 100);
        let mut app = app_ready(dir.clone());

        assert_eq!(app.tree().unwrap().total_size(), 1000);
        assert_eq!(app.tree().unwrap().total_files(), 3);

        // Seleccionamos 'a.bin' (el mayor) y abrimos el modal de borrado.
        app.selected = 0;
        let nodo_a = app.selected_node().unwrap();
        app.open_delete_modal();
        assert!(matches!(app.modal, Some(Modal::ConfirmDelete(_))));

        // Confirmamos el borrado permanente (un archivo no exige segunda
        // confirmación; usa `remove_file`, determinista en cualquier entorno).
        app.confirm_permanent();

        // El árbol se actualizó SIN re-escanear: total y conteo descontados.
        assert_eq!(app.tree().unwrap().total_size(), 400, "1000 - 600");
        assert_eq!(app.tree().unwrap().total_files(), 2);
        // 'a.bin' ya no figura entre los hijos visibles de la raíz.
        assert!(!app.sorted_children().contains(&nodo_a));
        // Y se mostró un mensaje de resultado (no error).
        assert!(matches!(
            app.modal,
            Some(Modal::Message { error: false, .. })
        ));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn desglose_agrupa_por_extension() {
        // root/  a.txt=100, b.txt=200, c.bin=300
        let dir = std::env::temp_dir().join(format!("dirrust_ext_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        write_bytes(&dir.join("a.txt"), 100);
        write_bytes(&dir.join("b.txt"), 200);
        write_bytes(&dir.join("c.bin"), 300);
        let mut app = app_ready(dir.clone());

        app.ensure_breakdown();
        // Dos extensiones: txt (300, 2 archivos) y bin (300, 1 archivo).
        assert_eq!(app.breakdown.len(), 2);
        let txt = app
            .breakdown
            .iter()
            .find(|e| e.ext.as_deref() == Some("txt"))
            .unwrap();
        assert_eq!(txt.size, 300);
        assert_eq!(txt.count, 2);
        let bin = app
            .breakdown
            .iter()
            .find(|e| e.ext.as_deref() == Some("bin"))
            .unwrap();
        assert_eq!(bin.size, 300);
        assert_eq!(bin.count, 1);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
