use crate::types::Rect;

#[derive(Debug, Clone)]
pub enum Command {
    MoveResize { win: u32, geom: Rect, border_w: u32 },
    Kill(u32),
    SetInputFocus(u32),
    SetBorderColor { win: u32, color: u32 },
    MapWindow(u32),
    UnmapWindow(u32),
    GrabButton { win: u32, focused: bool },
    WarpPointer { win: u32, x: i16, y: i16 },
    UpdateBar(usize),
    EmitIpcState,
    CloseConnection,
}
