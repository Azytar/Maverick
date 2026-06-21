use crate::types::Action;

#[derive(Debug, Clone)]
pub enum AppEvent {
    WindowCreated(u32),
    WindowDestroyed(u32),
    WindowUnmapped(u32),
    ActionTriggered(Action),
    FocusIn(u32),
    ButtonPress {
        win: u32,
        root_x: i32,
        root_y: i32,
        button: u8,
        state: u16,
    },
    PointerMotion {
        win: u32,
        root_x: i32,
        root_y: i32,
    },
    ButtonRelease,
}
