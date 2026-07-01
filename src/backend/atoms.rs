// rustwm/src/atoms.rs
// EWMH and ICCCM atom definitions and helpers
// This is where RustWM beats dwm: full EWMH compliance
// ensures file upload dialogs, Flameshot, portals work correctly

use x11rb::connection::Connection;
use x11rb::errors::ReplyError;
use x11rb::protocol::xproto::ConnectionExt as XConnExt;

/// All atoms used by RustWM
/// Grouped by protocol for clarity
// All atoms are fetched for _NET_SUPPORTED even if not directly used in WM logic
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub struct Atoms {
    // ICCCM atoms
    pub wm_protocols: u32,
    pub wm_delete_window: u32,
    pub wm_state: u32,
    pub wm_take_focus: u32,
    pub wm_class: u32,
    pub wm_name: u32,
    pub wm_transient_for: u32,
    pub wm_hints: u32,
    pub wm_normal_hints: u32,

    // EWMH _NET atoms
    pub net_supported: u32,
    pub net_client_list: u32,
    pub net_client_list_stacking: u32,
    pub net_number_of_desktops: u32,
    pub net_desktop_geometry: u32,
    pub net_desktop_viewport: u32,
    pub net_current_desktop: u32,
    pub net_desktop_names: u32,
    pub net_active_window: u32,
    pub net_workarea: u32,
    pub net_supporting_wm_check: u32,
    pub net_virtual_roots: u32,
    pub net_wm_name: u32,
    pub net_wm_visible_name: u32,
    pub net_wm_desktop: u32,
    pub net_wm_window_type: u32,
    pub net_wm_window_type_desktop: u32,
    pub net_wm_window_type_dock: u32,
    pub net_wm_window_type_toolbar: u32,
    pub net_wm_window_type_menu: u32,
    pub net_wm_window_type_utility: u32,
    pub net_wm_window_type_splash: u32,
    pub net_wm_window_type_dialog: u32,
    pub net_wm_window_type_normal: u32,
    pub net_wm_state: u32,
    pub net_wm_state_modal: u32,
    pub net_wm_state_sticky: u32,
    pub net_wm_state_maximized_vert: u32,
    pub net_wm_state_maximized_horiz: u32,
    pub net_wm_state_shaded: u32,
    pub net_wm_state_skip_taskbar: u32,
    pub net_wm_state_skip_pager: u32,
    pub net_wm_state_hidden: u32,
    pub net_wm_state_fullscreen: u32,
    pub net_wm_state_above: u32,
    pub net_wm_state_below: u32,
    pub net_wm_state_demands_attention: u32,
    pub net_wm_allowed_actions: u32,
    pub net_wm_strut: u32,
    pub net_wm_strut_partial: u32,
    pub net_wm_pid: u32,
    pub net_close_window: u32,
    pub net_moveresize_window: u32,
    pub net_wm_fullscreen_monitors: u32,
    pub net_frame_extents: u32,

    // XDG portal / file chooser support
    /// _GTK_SHOW_WINDOW_MENU - GTK apps send this for context menus
    pub gtk_show_window_menu: u32,
    /// _GTK_FRAME_EXTENTS - GTK shadow/CSD support
    pub gtk_frame_extents: u32,
    /// _NET_WM_BYPASS_COMPOSITOR - for compositor hints
    pub net_wm_bypass_compositor: u32,

    // Motif WM hints (used by some toolkits for decoration hints)
    pub motif_wm_hints: u32,

    // XDND (drag and drop) - needed for file uploads
    pub xdnd_aware: u32,
    pub xdnd_enter: u32,
    pub xdnd_position: u32,
    pub xdnd_status: u32,
    pub xdnd_type_list: u32,
    pub xdnd_action_copy: u32,
    pub xdnd_action_move: u32,
    pub xdnd_action_link: u32,
    pub xdnd_drop: u32,
    pub xdnd_finished: u32,
    pub xdnd_selection: u32,
    pub xdnd_proxy: u32,

    // UTF-8 string type
    pub utf8_string: u32,

    // Clipboard atoms
    pub clipboard: u32,
    pub targets: u32,
    pub multiple: u32,
    pub timestamp: u32,
    pub incr: u32,
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
        let r_wm_class = intern!("WM_CLASS")?;
        let r_wm_name = intern!("WM_NAME")?;
        let r_wm_transient = intern!("WM_TRANSIENT_FOR")?;
        let r_wm_hints = intern!("WM_HINTS")?;
        let r_wm_nhints = intern!("WM_NORMAL_HINTS")?;

        let r_net_supported = intern!("_NET_SUPPORTED")?;
        let r_net_client_list = intern!("_NET_CLIENT_LIST")?;
        let r_net_cl_stacking = intern!("_NET_CLIENT_LIST_STACKING")?;
        let r_net_num_desks = intern!("_NET_NUMBER_OF_DESKTOPS")?;
        let r_net_desk_geom = intern!("_NET_DESKTOP_GEOMETRY")?;
        let r_net_desk_vp = intern!("_NET_DESKTOP_VIEWPORT")?;
        let r_net_cur_desk = intern!("_NET_CURRENT_DESKTOP")?;
        let r_net_desk_names = intern!("_NET_DESKTOP_NAMES")?;
        let r_net_active = intern!("_NET_ACTIVE_WINDOW")?;
        let r_net_workarea = intern!("_NET_WORKAREA")?;
        let r_net_wm_check = intern!("_NET_SUPPORTING_WM_CHECK")?;
        let r_net_virt_roots = intern!("_NET_VIRTUAL_ROOTS")?;
        let r_net_wm_name = intern!("_NET_WM_NAME")?;
        let r_net_wm_visname = intern!("_NET_WM_VISIBLE_NAME")?;
        let r_net_wm_desktop = intern!("_NET_WM_DESKTOP")?;
        let r_net_wm_wtype = intern!("_NET_WM_WINDOW_TYPE")?;
        let r_net_wt_desktop = intern!("_NET_WM_WINDOW_TYPE_DESKTOP")?;
        let r_net_wt_dock = intern!("_NET_WM_WINDOW_TYPE_DOCK")?;
        let r_net_wt_toolbar = intern!("_NET_WM_WINDOW_TYPE_TOOLBAR")?;
        let r_net_wt_menu = intern!("_NET_WM_WINDOW_TYPE_MENU")?;
        let r_net_wt_utility = intern!("_NET_WM_WINDOW_TYPE_UTILITY")?;
        let r_net_wt_splash = intern!("_NET_WM_WINDOW_TYPE_SPLASH")?;
        let r_net_wt_dialog = intern!("_NET_WM_WINDOW_TYPE_DIALOG")?;
        let r_net_wt_normal = intern!("_NET_WM_WINDOW_TYPE_NORMAL")?;
        let r_net_wm_state = intern!("_NET_WM_STATE")?;
        let r_net_wm_modal = intern!("_NET_WM_STATE_MODAL")?;
        let r_net_wm_sticky = intern!("_NET_WM_STATE_STICKY")?;
        let r_net_wm_max_v = intern!("_NET_WM_STATE_MAXIMIZED_VERT")?;
        let r_net_wm_max_h = intern!("_NET_WM_STATE_MAXIMIZED_HORZ")?;
        let r_net_wm_shaded = intern!("_NET_WM_STATE_SHADED")?;
        let r_net_wm_notask = intern!("_NET_WM_STATE_SKIP_TASKBAR")?;
        let r_net_wm_nopager = intern!("_NET_WM_STATE_SKIP_PAGER")?;
        let r_net_wm_hidden = intern!("_NET_WM_STATE_HIDDEN")?;
        let r_net_wm_fullscr = intern!("_NET_WM_STATE_FULLSCREEN")?;
        let r_net_wm_above = intern!("_NET_WM_STATE_ABOVE")?;
        let r_net_wm_below = intern!("_NET_WM_STATE_BELOW")?;
        let r_net_wm_demands = intern!("_NET_WM_STATE_DEMANDS_ATTENTION")?;
        let r_net_wm_allowed = intern!("_NET_WM_ALLOWED_ACTIONS")?;
        let r_net_wm_strut = intern!("_NET_WM_STRUT")?;
        let r_net_wm_strut_p = intern!("_NET_WM_STRUT_PARTIAL")?;
        let r_net_wm_pid = intern!("_NET_WM_PID")?;
        let r_net_close = intern!("_NET_CLOSE_WINDOW")?;
        let r_net_moveresize = intern!("_NET_MOVERESIZE_WINDOW")?;
        let r_net_fullscr_mon = intern!("_NET_WM_FULLSCREEN_MONITORS")?;
        let r_net_frame_ext = intern!("_NET_FRAME_EXTENTS")?;

        let r_gtk_show_menu = intern!("_GTK_SHOW_WINDOW_MENU")?;
        let r_gtk_frame_ext = intern!("_GTK_FRAME_EXTENTS")?;
        let r_net_bypass_comp = intern!("_NET_WM_BYPASS_COMPOSITOR")?;
        let r_motif_hints = intern!("_MOTIF_WM_HINTS")?;

        let r_xdnd_aware = intern!("XdndAware")?;
        let r_xdnd_enter = intern!("XdndEnter")?;
        let r_xdnd_position = intern!("XdndPosition")?;
        let r_xdnd_status = intern!("XdndStatus")?;
        let r_xdnd_type_list = intern!("XdndTypeList")?;
        let r_xdnd_act_copy = intern!("XdndActionCopy")?;
        let r_xdnd_act_move = intern!("XdndActionMove")?;
        let r_xdnd_act_link = intern!("XdndActionLink")?;
        let r_xdnd_drop = intern!("XdndDrop")?;
        let r_xdnd_finished = intern!("XdndFinished")?;
        let r_xdnd_selection = intern!("XdndSelection")?;
        let r_xdnd_proxy = intern!("XdndProxy")?;

        let r_utf8_string = intern!("UTF8_STRING")?;
        let r_clipboard = intern!("CLIPBOARD")?;
        let r_targets = intern!("TARGETS")?;
        let r_multiple = intern!("MULTIPLE")?;
        let r_timestamp = intern!("TIMESTAMP")?;
        let r_incr = intern!("INCR")?;

        // Now collect all replies (pipelined)
        Ok(Atoms {
            wm_protocols: r_wm_protocols.reply()?.atom,
            wm_delete_window: r_wm_delete.reply()?.atom,
            wm_state: r_wm_state.reply()?.atom,
            wm_take_focus: r_wm_focus.reply()?.atom,
            wm_class: r_wm_class.reply()?.atom,
            wm_name: r_wm_name.reply()?.atom,
            wm_transient_for: r_wm_transient.reply()?.atom,
            wm_hints: r_wm_hints.reply()?.atom,
            wm_normal_hints: r_wm_nhints.reply()?.atom,
            net_supported: r_net_supported.reply()?.atom,
            net_client_list: r_net_client_list.reply()?.atom,
            net_client_list_stacking: r_net_cl_stacking.reply()?.atom,
            net_number_of_desktops: r_net_num_desks.reply()?.atom,
            net_desktop_geometry: r_net_desk_geom.reply()?.atom,
            net_desktop_viewport: r_net_desk_vp.reply()?.atom,
            net_current_desktop: r_net_cur_desk.reply()?.atom,
            net_desktop_names: r_net_desk_names.reply()?.atom,
            net_active_window: r_net_active.reply()?.atom,
            net_workarea: r_net_workarea.reply()?.atom,
            net_supporting_wm_check: r_net_wm_check.reply()?.atom,
            net_virtual_roots: r_net_virt_roots.reply()?.atom,
            net_wm_name: r_net_wm_name.reply()?.atom,
            net_wm_visible_name: r_net_wm_visname.reply()?.atom,
            net_wm_desktop: r_net_wm_desktop.reply()?.atom,
            net_wm_window_type: r_net_wm_wtype.reply()?.atom,
            net_wm_window_type_desktop: r_net_wt_desktop.reply()?.atom,
            net_wm_window_type_dock: r_net_wt_dock.reply()?.atom,
            net_wm_window_type_toolbar: r_net_wt_toolbar.reply()?.atom,
            net_wm_window_type_menu: r_net_wt_menu.reply()?.atom,
            net_wm_window_type_utility: r_net_wt_utility.reply()?.atom,
            net_wm_window_type_splash: r_net_wt_splash.reply()?.atom,
            net_wm_window_type_dialog: r_net_wt_dialog.reply()?.atom,
            net_wm_window_type_normal: r_net_wt_normal.reply()?.atom,
            net_wm_state: r_net_wm_state.reply()?.atom,
            net_wm_state_modal: r_net_wm_modal.reply()?.atom,
            net_wm_state_sticky: r_net_wm_sticky.reply()?.atom,
            net_wm_state_maximized_vert: r_net_wm_max_v.reply()?.atom,
            net_wm_state_maximized_horiz: r_net_wm_max_h.reply()?.atom,
            net_wm_state_shaded: r_net_wm_shaded.reply()?.atom,
            net_wm_state_skip_taskbar: r_net_wm_notask.reply()?.atom,
            net_wm_state_skip_pager: r_net_wm_nopager.reply()?.atom,
            net_wm_state_hidden: r_net_wm_hidden.reply()?.atom,
            net_wm_state_fullscreen: r_net_wm_fullscr.reply()?.atom,
            net_wm_state_above: r_net_wm_above.reply()?.atom,
            net_wm_state_below: r_net_wm_below.reply()?.atom,
            net_wm_state_demands_attention: r_net_wm_demands.reply()?.atom,
            net_wm_allowed_actions: r_net_wm_allowed.reply()?.atom,
            net_wm_strut: r_net_wm_strut.reply()?.atom,
            net_wm_strut_partial: r_net_wm_strut_p.reply()?.atom,
            net_wm_pid: r_net_wm_pid.reply()?.atom,
            net_close_window: r_net_close.reply()?.atom,
            net_moveresize_window: r_net_moveresize.reply()?.atom,
            net_wm_fullscreen_monitors: r_net_fullscr_mon.reply()?.atom,
            net_frame_extents: r_net_frame_ext.reply()?.atom,
            gtk_show_window_menu: r_gtk_show_menu.reply()?.atom,
            gtk_frame_extents: r_gtk_frame_ext.reply()?.atom,
            net_wm_bypass_compositor: r_net_bypass_comp.reply()?.atom,
            motif_wm_hints: r_motif_hints.reply()?.atom,
            xdnd_aware: r_xdnd_aware.reply()?.atom,
            xdnd_enter: r_xdnd_enter.reply()?.atom,
            xdnd_position: r_xdnd_position.reply()?.atom,
            xdnd_status: r_xdnd_status.reply()?.atom,
            xdnd_type_list: r_xdnd_type_list.reply()?.atom,
            xdnd_action_copy: r_xdnd_act_copy.reply()?.atom,
            xdnd_action_move: r_xdnd_act_move.reply()?.atom,
            xdnd_action_link: r_xdnd_act_link.reply()?.atom,
            xdnd_drop: r_xdnd_drop.reply()?.atom,
            xdnd_finished: r_xdnd_finished.reply()?.atom,
            xdnd_selection: r_xdnd_selection.reply()?.atom,
            xdnd_proxy: r_xdnd_proxy.reply()?.atom,
            utf8_string: r_utf8_string.reply()?.atom,
            clipboard: r_clipboard.reply()?.atom,
            targets: r_targets.reply()?.atom,
            multiple: r_multiple.reply()?.atom,
            timestamp: r_timestamp.reply()?.atom,
            incr: r_incr.reply()?.atom,
        })
    }

    /// All EWMH atoms we support (for _NET_SUPPORTED property)
    pub fn supported_list(&self) -> Vec<u32> {
        vec![
            self.net_supported,
            self.net_client_list,
            self.net_client_list_stacking,
            self.net_number_of_desktops,
            self.net_desktop_geometry,
            self.net_desktop_viewport,
            self.net_current_desktop,
            self.net_desktop_names,
            self.net_active_window,
            self.net_workarea,
            self.net_supporting_wm_check,
            self.net_wm_name,
            self.net_wm_visible_name,
            self.net_wm_desktop,
            self.net_wm_window_type,
            self.net_wm_window_type_desktop,
            self.net_wm_window_type_dock,
            self.net_wm_window_type_toolbar,
            self.net_wm_window_type_menu,
            self.net_wm_window_type_utility,
            self.net_wm_window_type_splash,
            self.net_wm_window_type_dialog,
            self.net_wm_window_type_normal,
            self.net_wm_state,
            self.net_wm_state_modal,
            self.net_wm_state_sticky,
            self.net_wm_state_maximized_vert,
            self.net_wm_state_maximized_horiz,
            self.net_wm_state_hidden,
            self.net_wm_state_fullscreen,
            self.net_wm_state_above,
            self.net_wm_state_below,
            self.net_wm_state_demands_attention,
            self.net_close_window,
            self.net_wm_allowed_actions,
            self.net_wm_strut,
            self.net_wm_strut_partial,
            self.net_frame_extents,
            self.net_wm_pid,
        ]
    }
}
