// maverick/src/backend/atoms.rs
// EWMH and ICCCM atom definitions and helpers.
//
// Every atom here is actually referenced by the WM logic (manage, arrange,
// focus, struts, client-messages). Atoms that were interned but never read or
// written were removed: this struct mirrors exactly what the code speaks, and
// `_NET_SUPPORTED` only advertises atoms we really handle.

use x11rb::connection::Connection;
use x11rb::errors::ReplyError;
use x11rb::protocol::xproto::ConnectionExt as XConnExt;

/// All atoms used by `maverick`, grouped by protocol for clarity.
#[derive(Debug, Clone, Copy)]
pub struct Atoms {
    // ICCCM atoms
    pub wm_protocols: u32,
    pub wm_delete_window: u32,
    pub wm_state: u32,
    pub wm_take_focus: u32,
    pub wm_hints: u32,

    // EWMH _NET atoms
    pub net_supported: u32,
    pub net_client_list: u32,
    pub net_client_list_stacking: u32,
    pub net_number_of_desktops: u32,
    pub net_desktop_geometry: u32,
    pub net_current_desktop: u32,
    pub net_desktop_names: u32,
    pub net_active_window: u32,
    pub net_workarea: u32,
    pub net_supporting_wm_check: u32,
    pub net_wm_name: u32,
    pub net_wm_desktop: u32,
    pub net_wm_window_type: u32,
    pub net_wm_window_type_desktop: u32,
    pub net_wm_window_type_dock: u32,
    pub net_wm_window_type_toolbar: u32,
    pub net_wm_window_type_menu: u32,
    pub net_wm_window_type_utility: u32,
    pub net_wm_window_type_splash: u32,
    pub net_wm_window_type_dialog: u32,
    pub net_wm_state: u32,
    pub net_wm_state_modal: u32,
    pub net_wm_state_maximized_vert: u32,
    pub net_wm_state_maximized_horiz: u32,
    pub net_wm_state_fullscreen: u32,
    pub net_wm_state_demands_attention: u32,
    pub net_wm_strut: u32,
    pub net_wm_strut_partial: u32,
    pub net_wm_window_opacity: u32,
    pub net_close_window: u32,
    pub net_frame_extents: u32,
    pub net_wm_bypass_compositor: u32,

    // Maverick-private persistence atoms (across `--replace` / restart).
    // `_MAVERICK_FLOAT` is 1 when the window was floating, `_MAVERICK_GEOM`
    // remembers its location as `[x, y, w, h]` CARDINAL, so readopting an
    // existing window tree returns windows to exactly where they were.
    pub maverick_float: u32,
    pub maverick_geom: u32,

    // Misc
    pub utf8_string: u32,
}

impl Atoms {
    /// Initialize all atoms with a single batch intern
    pub fn new<C: Connection>(conn: &C) -> Result<Self, ReplyError> {
        // We intern all atoms in parallel, then collect
        // This is faster than sequential because X11 pipelining
        macro_rules! intern {
            ($name:literal) => {
                conn.intern_atom(false, $name.as_bytes())
            };
        }

        // Fire all requests
        let r_wm_protocols = intern!("WM_PROTOCOLS")?;
        let r_wm_delete = intern!("WM_DELETE_WINDOW")?;
        let r_wm_state = intern!("WM_STATE")?;
        let r_wm_focus = intern!("WM_TAKE_FOCUS")?;
        let r_wm_hints = intern!("WM_HINTS")?;

        let r_net_supported = intern!("_NET_SUPPORTED")?;
        let r_net_client_list = intern!("_NET_CLIENT_LIST")?;
        let r_net_client_list_stacking = intern!("_NET_CLIENT_LIST_STACKING")?;
        let r_net_num_desks = intern!("_NET_NUMBER_OF_DESKTOPS")?;
        let r_net_desk_geom = intern!("_NET_DESKTOP_GEOMETRY")?;
        let r_net_cur_desk = intern!("_NET_CURRENT_DESKTOP")?;
        let r_net_desk_names = intern!("_NET_DESKTOP_NAMES")?;
        let r_net_active = intern!("_NET_ACTIVE_WINDOW")?;
        let r_net_workarea = intern!("_NET_WORKAREA")?;
        let r_net_wm_check = intern!("_NET_SUPPORTING_WM_CHECK")?;
        let r_net_wm_name = intern!("_NET_WM_NAME")?;
        let r_net_wm_desktop = intern!("_NET_WM_DESKTOP")?;
        let r_net_wm_wtype = intern!("_NET_WM_WINDOW_TYPE")?;
        let r_net_wt_desktop = intern!("_NET_WM_WINDOW_TYPE_DESKTOP")?;
        let r_net_wt_dock = intern!("_NET_WM_WINDOW_TYPE_DOCK")?;
        let r_net_wt_toolbar = intern!("_NET_WM_WINDOW_TYPE_TOOLBAR")?;
        let r_net_wt_menu = intern!("_NET_WM_WINDOW_TYPE_MENU")?;
        let r_net_wt_utility = intern!("_NET_WM_WINDOW_TYPE_UTILITY")?;
        let r_net_wt_splash = intern!("_NET_WM_WINDOW_TYPE_SPLASH")?;
        let r_net_wt_dialog = intern!("_NET_WM_WINDOW_TYPE_DIALOG")?;
        let r_net_wm_state = intern!("_NET_WM_STATE")?;
        let r_net_wm_modal = intern!("_NET_WM_STATE_MODAL")?;
        let r_net_wm_max_v = intern!("_NET_WM_STATE_MAXIMIZED_VERT")?;
        let r_net_wm_max_h = intern!("_NET_WM_STATE_MAXIMIZED_HORZ")?;
        let r_net_wm_fullscr = intern!("_NET_WM_STATE_FULLSCREEN")?;
        let r_net_wm_demands = intern!("_NET_WM_STATE_DEMANDS_ATTENTION")?;
        let r_net_wm_strut = intern!("_NET_WM_STRUT")?;
        let r_net_wm_strut_p = intern!("_NET_WM_STRUT_PARTIAL")?;
        let r_net_wm_opacity = intern!("_NET_WM_WINDOW_OPACITY")?;
        let r_net_close = intern!("_NET_CLOSE_WINDOW")?;
        let r_net_frame_ext = intern!("_NET_FRAME_EXTENTS")?;
        let r_net_bypass_comp = intern!("_NET_WM_BYPASS_COMPOSITOR")?;

        let r_maverick_float = intern!("_MAVERICK_FLOAT")?;
        let r_maverick_geom = intern!("_MAVERICK_GEOM")?;

        let r_utf8_string = intern!("UTF8_STRING")?;

        // Now collect all replies (pipelined)
        Ok(Atoms {
            wm_protocols: r_wm_protocols.reply()?.atom,
            wm_delete_window: r_wm_delete.reply()?.atom,
            wm_state: r_wm_state.reply()?.atom,
            wm_take_focus: r_wm_focus.reply()?.atom,
            wm_hints: r_wm_hints.reply()?.atom,
            net_supported: r_net_supported.reply()?.atom,
            net_client_list: r_net_client_list.reply()?.atom,
            net_client_list_stacking: r_net_client_list_stacking.reply()?.atom,
            net_number_of_desktops: r_net_num_desks.reply()?.atom,
            net_desktop_geometry: r_net_desk_geom.reply()?.atom,
            net_current_desktop: r_net_cur_desk.reply()?.atom,
            net_desktop_names: r_net_desk_names.reply()?.atom,
            net_active_window: r_net_active.reply()?.atom,
            net_workarea: r_net_workarea.reply()?.atom,
            net_supporting_wm_check: r_net_wm_check.reply()?.atom,
            net_wm_name: r_net_wm_name.reply()?.atom,
            net_wm_desktop: r_net_wm_desktop.reply()?.atom,
            net_wm_window_type: r_net_wm_wtype.reply()?.atom,
            net_wm_window_type_desktop: r_net_wt_desktop.reply()?.atom,
            net_wm_window_type_dock: r_net_wt_dock.reply()?.atom,
            net_wm_window_type_toolbar: r_net_wt_toolbar.reply()?.atom,
            net_wm_window_type_menu: r_net_wt_menu.reply()?.atom,
            net_wm_window_type_utility: r_net_wt_utility.reply()?.atom,
            net_wm_window_type_splash: r_net_wt_splash.reply()?.atom,
            net_wm_window_type_dialog: r_net_wt_dialog.reply()?.atom,
            net_wm_state: r_net_wm_state.reply()?.atom,
            net_wm_state_modal: r_net_wm_modal.reply()?.atom,
            net_wm_state_maximized_vert: r_net_wm_max_v.reply()?.atom,
            net_wm_state_maximized_horiz: r_net_wm_max_h.reply()?.atom,
            net_wm_state_fullscreen: r_net_wm_fullscr.reply()?.atom,
            net_wm_state_demands_attention: r_net_wm_demands.reply()?.atom,
            net_wm_strut: r_net_wm_strut.reply()?.atom,
            net_wm_strut_partial: r_net_wm_strut_p.reply()?.atom,
            net_wm_window_opacity: r_net_wm_opacity.reply()?.atom,
            net_close_window: r_net_close.reply()?.atom,
            net_frame_extents: r_net_frame_ext.reply()?.atom,
            net_wm_bypass_compositor: r_net_bypass_comp.reply()?.atom,
            maverick_float: r_maverick_float.reply()?.atom,
            maverick_geom: r_maverick_geom.reply()?.atom,
            utf8_string: r_utf8_string.reply()?.atom,
        })
    }

    /// All EWMH atoms we support (for _`NET_SUPPORTED` property)
    pub fn supported_list(&self) -> Vec<u32> {
        vec![
            self.net_supported,
            self.net_client_list,
            self.net_client_list_stacking,
            self.net_number_of_desktops,
            self.net_desktop_geometry,
            self.net_current_desktop,
            self.net_desktop_names,
            self.net_active_window,
            self.net_workarea,
            self.net_supporting_wm_check,
            self.net_wm_name,
            self.net_wm_desktop,
            self.net_wm_window_type,
            self.net_wm_window_type_desktop,
            self.net_wm_window_type_dock,
            self.net_wm_window_type_toolbar,
            self.net_wm_window_type_menu,
            self.net_wm_window_type_utility,
            self.net_wm_window_type_splash,
            self.net_wm_window_type_dialog,
            self.net_wm_state,
            self.net_wm_state_modal,
            self.net_wm_state_maximized_vert,
            self.net_wm_state_maximized_horiz,
            self.net_wm_state_fullscreen,
            self.net_wm_state_demands_attention,
            self.net_wm_strut,
            self.net_wm_strut_partial,
            self.net_wm_window_opacity,
            self.net_close_window,
            self.net_frame_extents,
            self.net_wm_bypass_compositor,
        ]
    }
}
