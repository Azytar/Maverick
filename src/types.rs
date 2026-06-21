// maverick/src/types.rs
// Core state — niri-style columnar layout, clean coordinates, no drift.

use std::collections::HashMap;
use x11rb::protocol::xproto::Window;

pub type TagMask = u32;

// ─── Geometry ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    #[inline] pub fn new(x: i32, y: i32, w: u32, h: u32) -> Self { Self { x, y, w, h } }
    #[inline] pub fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x && px < self.x + self.w as i32
            && py >= self.y && py < self.y + self.h as i32
    }
    #[inline] pub fn area(&self) -> u64 { self.w as u64 * self.h as u64 }
    #[inline] pub fn right(&self) -> i32 { self.x + self.w as i32 }
    #[inline] pub fn bottom(&self) -> i32 { self.y + self.h as i32 }
}

// ─── Window flags ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Default)]
pub struct WinFlags(u8);
impl WinFlags {
    pub const FLOAT:      u8 = 1 << 0;
    pub const FULLSCREEN: u8 = 1 << 1;
    pub const URGENT:     u8 = 1 << 2;
    pub const NO_FOCUS:   u8 = 1 << 3;
    pub const FIXED:      u8 = 1 << 4;

    #[inline] pub fn set(&mut self, f: u8)    { self.0 |= f; }
    #[inline] pub fn clear(&mut self, f: u8)  { self.0 &= !f; }
    #[inline] pub fn toggle(&mut self, f: u8) { self.0 ^= f; }
    #[inline] pub fn has(&self, f: u8) -> bool { self.0 & f != 0 }
}

// ─── Size hints ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Default)]
pub struct SizeHints {
    pub base_w:     i32,
    pub base_h:     i32,
    pub inc_w:      i32,
    pub inc_h:      i32,
    pub max_w:      i32,
    pub max_h:      i32,
    pub min_w:      i32,
    pub min_h:      i32,
    pub min_aspect: f32,
    pub max_aspect: f32,
    pub valid:      bool,
}

// ─── Column (niri-style) ──────────────────────────────────────────────────────
//
// Each workspace has N columns. Every column holds one or more windows
// stacked vertically. The layout engine assigns absolute screen coords
// based on the column's logical offset and the current scroll position.
//
// This means coordinates are ALWAYS derived from (col_x + scroll, row_y)
// and never stored as mutable state — no drift possible.

#[derive(Debug, Clone)]
pub struct Column {
    pub windows:   Vec<Window>,   // top-to-bottom
    pub width:     u32,           // pixel width of this column
    pub focused:   usize,         // index into `windows` that has focus
}

impl Column {
    pub fn new(width: u32) -> Self {
        Self { windows: Vec::with_capacity(4), width, focused: 0 }
    }
    pub fn focused_win(&self) -> Option<Window> {
        self.windows.get(self.focused).copied()
    }
}

// ─── Workspace ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Focus {
    pub column_idx: usize,
    pub window_idx: usize,
}

#[derive(Debug, Clone)]
pub struct Workspace {
    pub tag:      u32,
    pub columns:  Vec<Column>,
    pub focus:    Focus,
    pub scroll:   i32,
    pub floats:   Vec<Window>,
}

impl Workspace {
    pub fn new(tag: u32) -> Self {
        Self { 
            tag, 
            columns: Vec::new(), 
            focus: Focus { column_idx: 0, window_idx: 0 }, 
            scroll: 0, 
            floats: Vec::new() 
        }
    }

    pub fn empty(tag: u32) -> Self { Self::new(tag) }

    pub fn is_empty(&self) -> bool {
        self.columns.is_empty() && self.floats.is_empty()
    }

    pub fn focused_win(&self) -> Option<Window> {
        self.columns.get(self.focus.column_idx)?.focused_win()
    }

    pub fn add_tiled(mut self, window: Window, default_col_width: u32, workarea_w: u32) -> Self {
        if self.columns.is_empty() {
            // Primera ventana: ocupa TODO el ancho disponible del área de trabajo
            let mut col = Column::new(workarea_w);
            col.windows.push(window);
            self.columns.push(col);
            self.focus.column_idx = 0;
            self.focus.window_idx = 0;
        } else {
            // Ventanas siguientes: la columna existente se encoge al tamaño por defecto
            // y la nueva columna toma el espacio restante a la derecha.
            if self.columns.len() == 1 {
                self.columns[0].width = default_col_width;
            }
            
            let new_w = workarea_w.saturating_sub(default_col_width).max(default_col_width);
            let mut new_col = Column::new(new_w);
            new_col.windows.push(window);
            self.columns.push(new_col);
            
            self.focus.column_idx = self.columns.len() - 1;
            self.focus.window_idx = 0;
        }
        self
    }

    pub fn move_window_right(mut self, default_col_w: u32) -> Self {
        let col_idx = self.focus.column_idx;
        let win_idx = self.focus.window_idx;

        if col_idx >= self.columns.len() { return self; }
        let window = self.columns[col_idx].windows.remove(win_idx);

        if col_idx + 1 < self.columns.len() {
            self.columns[col_idx + 1].windows.insert(0, window);
            self.focus.column_idx = col_idx + 1;
            self.focus.window_idx = 0;
            self.columns[col_idx + 1].focused = 0;
        } else {
            self.columns.push(Column { windows: vec![window], width: default_col_w, focused: 0 });
            self.focus.column_idx = col_idx + 1;
            self.focus.window_idx = 0;
        }

        self = self.cleanup_empty_columns();
        self
    }

    pub fn remove_window(mut self, win: Window) -> Self {
        if let Some(fi) = self.floats.iter().position(|&w| w == win) {
            self.floats.remove(fi);
            return self;
        }
        
        let mut columns = self.columns;
        for col in columns.iter_mut() {
            if let Some(wi) = col.windows.iter().position(|&w| w == win) {
                col.windows.remove(wi);
                if col.focused >= col.windows.len() && !col.windows.is_empty() {
                    col.focused = col.windows.len() - 1;
                }
                break;
            }
        }
        
        self.columns = columns;
        self = self.cleanup_empty_columns();
        self
    }

    /// Limpia columnas vacías y ajusta el foco para que no quede fuera de rango.
    pub fn cleanup_empty_columns(mut self) -> Self {
        self.columns.retain(|col| !col.windows.is_empty());

        if self.columns.is_empty() {
            self.focus.column_idx = 0;
            self.focus.window_idx = 0;
        } else if self.focus.column_idx >= self.columns.len() {
            self.focus.column_idx = self.columns.len() - 1;
            // Sync window_idx with the column's own focused pointer
            let col = &self.columns[self.focus.column_idx];
            self.focus.window_idx = col.focused.min(col.windows.len().saturating_sub(1));
        } else {
            // Column still exists — keep window_idx in sync with col.focused
            let col = &self.columns[self.focus.column_idx];
            self.focus.window_idx = col.focused.min(col.windows.len().saturating_sub(1));
        }

        self
    }
}

// ─── Client ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Client {
    pub window:       Window,
    pub name:         String,
    pub class:        String,
    pub instance:     String,
    pub geom:         Rect,
    pub saved_geom:   Rect,
    pub border_w:     u32,
    pub old_border_w: u32,
    pub tags:         TagMask,
    pub flags:        WinFlags,
    pub hints:        SizeHints,
    pub monitor:      usize,
    pub workspace:    usize,   // index into Monitor::workspaces
    pub focus_serial: u64,
    pub is_dialog:    bool,
    pub is_unmanaged: bool,
    pub wants_input:  bool,
    pub wm_hidden:    bool,
}

impl Client {
    pub fn new(win: Window, mon: usize, ws: usize) -> Self {
        Self {
            window: win, name: String::new(), class: String::new(),
            instance: String::new(), geom: Rect::default(), saved_geom: Rect::default(),
            border_w: 2, old_border_w: 2, tags: 1, flags: WinFlags::default(),
            hints: SizeHints::default(), monitor: mon, workspace: ws,
            focus_serial: 0, is_dialog: false, is_unmanaged: false,
            wants_input: true, wm_hidden: false,
        }
    }

    #[inline] pub fn is_float(&self)      -> bool { self.flags.has(WinFlags::FLOAT) }
    #[inline] pub fn is_fullscreen(&self) -> bool { self.flags.has(WinFlags::FULLSCREEN) }
    #[inline] pub fn no_focus(&self)      -> bool { self.flags.has(WinFlags::NO_FOCUS) }
}

// ─── Monitor ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Monitor {
    pub screen:     Rect,
    pub workarea:   Rect,    // screen minus bar
    pub bar_win:    Option<Window>,
    pub bar_gc:     Option<u32>,   // GC id
    pub show_bar:   bool,
    pub top_bar:    bool,
    pub workspaces: Vec<Workspace>,
    pub active_ws:  usize,
    pub focused:    Option<Window>,
    pub focus_stack: Vec<Window>,
}

impl Monitor {
    pub fn new(screen: Rect, bar_height: u32, top_bar: bool, n_tags: usize) -> Self {
        let workarea = if top_bar {
            Rect::new(screen.x, screen.y + bar_height as i32, screen.w, screen.h.saturating_sub(bar_height))
        } else {
            Rect::new(screen.x, screen.y, screen.w, screen.h.saturating_sub(bar_height))
        };
        let workspaces = (0..n_tags).map(|i| Workspace::new(i as u32)).collect();
        Self {
            screen, workarea, bar_win: None, bar_gc: None,
            show_bar: true, top_bar, workspaces,
            active_ws: 0, focused: None, focus_stack: Vec::with_capacity(16),
        }
    }

    pub fn bar_y(&self) -> i32 {
        if self.top_bar { self.screen.y }
        else { self.screen.y + self.screen.h as i32 - self.bar_height() as i32 }
    }

    pub fn bar_height(&self) -> u32 {
        if self.show_bar { self.screen.h - self.workarea.h } else { 0 }
    }

    pub fn ws(&self) -> &Workspace { &self.workspaces[self.active_ws] }
    pub fn ws_mut(&mut self) -> &mut Workspace { &mut self.workspaces[self.active_ws] }

    pub fn recalc_workarea(&mut self, bar_h: u32) {
        if self.show_bar {
            self.workarea.h = self.screen.h.saturating_sub(bar_h);
            if self.top_bar {
                self.workarea.y = self.screen.y + bar_h as i32;
                self.workarea.x = self.screen.x;
                self.workarea.w = self.screen.w;
            } else {
                self.workarea.y = self.screen.y;
                self.workarea.x = self.screen.x;
                self.workarea.w = self.screen.w;
            }
        } else {
            self.workarea = self.screen;
        }
    }
}

// ─── Direction ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Dir { Next, Prev, Left, Right, Up, Down }

// ─── Layout kind ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutKind {
    Column,     // niri-style: one or more windows per column, columns side by side
    Monocle,    // one window fills workarea
    Grid,       // equal grid
}

impl LayoutKind {
    pub fn from_str(s: &str) -> Self {
        match s { "monocle" => Self::Monocle, "grid" => Self::Grid, _ => Self::Column }
    }
    pub fn symbol(&self) -> &'static str {
        match self { Self::Column => "[|]", Self::Monocle => "[M]", Self::Grid => "[#]" }
    }
}

// ─── Key actions ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Action {
    Spawn(Vec<String>),
    Kill,
    FocusDir(Dir),
    MoveDir(Dir),
    ToggleFloat,
    ToggleFullscreen,
    ToggleBar,
    SetLayout(LayoutKind),
    CycleLayout,
    GrowCol(i32),      // pixels to grow/shrink column width
    NewColumn,         // move focused window into a new column to the right
    CollapseColumn,    // merge column into previous
    View(usize),       // switch to workspace n
    MoveToWs(usize),   // move window to workspace n
    FocusMon(Dir),
    MoveMon(Dir),
    Restart,
    Quit,
    /// Show a native confirmation dialog before quitting.
    QuitConfirm,
}

// ─── Global state ────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct State {
    pub clients:      HashMap<Window, Client>,
    pub monitors:     Vec<Monitor>,
    pub sel_mon:      usize,
    pub focus_serial: u64,
    pub running:      bool,
    pub status:       String,
    pub layout:       LayoutKind,
}

impl State {
    pub fn new() -> Self {
        Self {
            clients: HashMap::new(),
            monitors: Vec::new(),
            sel_mon: 0,
            focus_serial: 0,
            running: true,
            status: String::from("maverick"),
            layout: LayoutKind::Column,
        }
    }

    pub fn mon(&self) -> &Monitor {
        let i = self.sel_mon.min(self.monitors.len().saturating_sub(1));
        &self.monitors[i]
    }
    pub fn mon_mut(&mut self) -> &mut Monitor {
        let i = self.sel_mon.min(self.monitors.len().saturating_sub(1));
        &mut self.monitors[i]
    }

    pub fn mon_at(&self, x: i32, y: i32) -> usize {
        for (i, m) in self.monitors.iter().enumerate() {
            if m.screen.contains(x, y) { return i; }
        }
        self.sel_mon
    }

    pub fn next_serial(&mut self) -> u64 {
        self.focus_serial += 1;
        self.focus_serial
    }

    pub fn add_client(&mut self, c: Client) {
        let win = c.window;
        self.clients.insert(win, c);
    }

    pub fn remove_client(&mut self, win: Window) -> Option<Client> {
        let c = self.clients.remove(&win)?;
        let mon = &mut self.monitors[c.monitor];
        mon.focus_stack.retain(|&w| w != win);
        if mon.focused == Some(win) {
            mon.focused = mon.focus_stack.last().copied();
        }
        if c.workspace < mon.workspaces.len() {
            let mut ws = mon.workspaces[c.workspace].clone();
            ws = ws.remove_window(win);
            mon.workspaces[c.workspace] = ws;
        }
        Some(c)
    }

    /// Pure workspace rearrangement for MoveDir — no X11 calls.
    /// Call this from x11.rs then follow up with arrange/focus.
    /// Returns false if there was nothing to do (float, empty workspace, boundary no-op).
    pub fn apply_move_dir(&mut self, dir: Dir, default_col_w: u32) -> bool {
        let mi   = self.sel_mon;
        let ws_i = match self.monitors.get(mi) { Some(m) => m.active_ws, None => return false };
        let focused = match self.monitors[mi].focused { Some(w) => w, None => return false };

        if self.clients.get(&focused).map(|c| c.is_float()).unwrap_or(false) { return false; }

        let ci = match self.monitors[mi].workspaces.get(ws_i) {
            Some(ws) => ws.focus.column_idx,
            None => return false,
        };
        let n_cols  = self.monitors[mi].workspaces[ws_i].columns.len();
        let col_len = self.monitors[mi].workspaces[ws_i].columns
            .get(ci).map(|c| c.windows.len()).unwrap_or(0);

        let mut ws = self.monitors[mi].workspaces[ws_i].clone();

        match dir {
            Dir::Left | Dir::Right => {
                if col_len <= 1 {
                    // ── Single-window column: swap with neighbour ──────────────
                    match dir {
                        Dir::Left if ci > 0 => {
                            ws.columns.swap(ci, ci - 1);
                            ws.focus.column_idx = ci - 1;
                            ws.focus.window_idx = 0;
                        }
                        Dir::Right if ci + 1 < n_cols => {
                            ws.columns.swap(ci, ci + 1);
                            ws.focus.column_idx = ci + 1;
                            ws.focus.window_idx = 0;
                        }
                        _ => return false, // at boundary, no-op
                    }
                } else {
                    // ── Multi-window column: extract into adjacent column ──────
                    ws = ws.remove_window(focused); // source col survives (col_len > 1)
                    let insert_pos = (if dir == Dir::Left { ci } else { ci + 1 })
                        .min(ws.columns.len());
                    let mut new_col = Column::new(default_col_w);
                    new_col.windows.push(focused);
                    new_col.focused = 0;
                    ws.columns.insert(insert_pos, new_col);
                    ws.focus.column_idx = insert_pos;
                    ws.focus.window_idx = 0;
                }
            }
            Dir::Up | Dir::Down => {
                if let Some(col) = ws.columns.get_mut(ci) {
                    let n = col.windows.len();
                    if n < 2 { return false; }
                    let ri     = col.focused;
                    let new_ri = if dir == Dir::Up { (ri + n - 1) % n } else { (ri + 1) % n };
                    col.windows.swap(ri, new_ri);
                    col.focused     = new_ri;
                    ws.focus.window_idx = new_ri;
                } else {
                    return false;
                }
            }
            _ => return false,
        }

        self.monitors[mi].workspaces[ws_i] = ws;
        true
    }
}
