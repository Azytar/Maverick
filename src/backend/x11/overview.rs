//! Temporary Overview pictures. Client windows remain mapped at their logical size.
//!
//! Automatic per-window redirection supplies complete images, including windows
//! outside the viewport. This module never redirects the root or owns the desktop
//! compositor selection. Every picture, pixmap and redirect belongs to one view.

use super::*;
use x11rb::protocol::{composite, damage, render};

type XResult<T> = Result<T, Box<dyn std::error::Error>>;

struct Image {
    conn: Rc<XConn>,
    window: Window,
    pixmap: Pixmap,
    picture: render::Picture,
    damage: damage::Damage,
    redirected: bool,
    extent: (u16, u16),
}

impl Drop for Image {
    fn drop(&mut self) {
        if self.damage != 0 {
            let _ = damage::destroy(&self.conn, self.damage);
        }
        if self.picture != 0 {
            let _ = render::free_picture(&self.conn, self.picture);
        }
        if self.pixmap != 0 {
            let _ = self.conn.free_pixmap(self.pixmap);
        }
        if self.redirected {
            let _ = composite::unredirect_window(
                &self.conn,
                self.window,
                composite::Redirect::AUTOMATIC,
            );
        }
    }
}

impl Image {
    fn new(
        conn: Rc<XConn>,
        window: Window,
        formats: &BTreeMap<Visualid, render::Pictformat>,
    ) -> XResult<Self> {
        let mut image = Self {
            conn,
            window,
            pixmap: 0,
            picture: 0,
            damage: 0,
            redirected: false,
            extent: (0, 0),
        };
        let visual = image.conn.get_window_attributes(window)?.reply()?.visual;
        let format = *formats
            .get(&visual)
            .ok_or("Overview: no Render format for the client visual")?;
        composite::redirect_window(&image.conn, window, composite::Redirect::AUTOMATIC)?.check()?;
        image.redirected = true;
        let pixmap = image.conn.generate_id()?;
        composite::name_window_pixmap(&image.conn, window, pixmap)?.check()?;
        image.pixmap = pixmap;
        let geometry = image.conn.get_geometry(pixmap)?.reply()?;
        image.extent = (geometry.width, geometry.height);
        let picture = image.conn.generate_id()?;
        render::create_picture(
            &image.conn,
            picture,
            pixmap,
            format,
            &render::CreatePictureAux::new(),
        )?
        .check()?;
        image.picture = picture;
        render::set_picture_filter(&image.conn, picture, b"bilinear", &[])?.check()?;
        let id = image.conn.generate_id()?;
        damage::create(&image.conn, id, window, damage::ReportLevel::NON_EMPTY)?.check()?;
        image.damage = id;
        Ok(image)
    }
}

pub(super) struct Overview {
    conn: Rc<XConn>,
    pub window: Window,
    pub view: ViewId,
    pub workarea: Rect,
    output: render::Picture,
    buffer: Pixmap,
    picture: render::Picture,
    formats: BTreeMap<Visualid, render::Pictformat>,
    images: BTreeMap<Window, Image>,
    tiles: Vec<(Window, Rect)>,
}

impl Drop for Overview {
    fn drop(&mut self) {
        self.images.clear();
        for picture in [self.picture, self.output] {
            if picture != 0 {
                let _ = render::free_picture(&self.conn, picture);
            }
        }
        if self.buffer != 0 {
            let _ = self.conn.free_pixmap(self.buffer);
        }
        if self.window != 0 {
            let _ = self.conn.destroy_window(self.window);
        }
    }
}

impl Overview {
    pub fn check_extensions(conn: &XConn) -> XResult<()> {
        let version = composite::query_version(conn, 0, 4)?.reply()?;
        if version.major_version == 0 && version.minor_version < 2 {
            return Err("Overview requires Composite 0.2 or newer".into());
        }
        let render = render::query_version(conn, 0, 11)?.reply()?;
        if render.major_version == 0 && render.minor_version < 6 {
            return Err("Overview requires Render 0.6 or newer".into());
        }
        damage::query_version(conn, 1, 1)?.reply()?;
        Ok(())
    }

    pub fn new(conn: Rc<XConn>, screen: usize, view: ViewId, workarea: Rect) -> XResult<Self> {
        Self::check_extensions(&conn)?;
        let formats = render::query_pict_formats(&conn)?
            .reply()?
            .screens
            .into_iter()
            .flat_map(|s| s.depths)
            .flat_map(|d| d.visuals)
            .map(|v| (v.visual, v.format))
            .collect::<BTreeMap<_, _>>();
        let setup = &conn.setup().roots[screen];
        let (root, depth, visual, black) = (
            setup.root,
            setup.root_depth,
            setup.root_visual,
            setup.black_pixel,
        );
        let format = *formats
            .get(&visual)
            .ok_or("Overview: no Render format for the root visual")?;
        let mut overview = Self {
            conn,
            window: 0,
            view,
            workarea,
            output: 0,
            buffer: 0,
            picture: 0,
            formats,
            images: BTreeMap::new(),
            tiles: Vec::new(),
        };
        let width = workarea.w.clamp(1, u16::MAX as u32) as u16;
        let height = workarea.h.clamp(1, u16::MAX as u32) as u16;
        let window = overview.conn.generate_id()?;
        overview
            .conn
            .create_window(
                depth,
                window,
                root,
                workarea.x.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
                workarea.y.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
                width,
                height,
                0,
                WindowClass::INPUT_OUTPUT,
                visual,
                &CreateWindowAux::new()
                    .override_redirect(1)
                    .background_pixel(black)
                    .event_mask(
                        EventMask::EXPOSURE | EventMask::BUTTON_PRESS | EventMask::POINTER_MOTION,
                    ),
            )?
            .check()?;
        overview.window = window;
        overview.conn.change_property8(
            PropMode::REPLACE,
            window,
            AtomEnum::WM_NAME,
            AtomEnum::STRING,
            b"Maverick Overview",
        )?;
        let output = overview.conn.generate_id()?;
        render::create_picture(
            &overview.conn,
            output,
            window,
            format,
            &render::CreatePictureAux::new(),
        )?
        .check()?;
        overview.output = output;
        let buffer = overview.conn.generate_id()?;
        overview
            .conn
            .create_pixmap(depth, buffer, root, width, height)?
            .check()?;
        overview.buffer = buffer;
        let picture = overview.conn.generate_id()?;
        render::create_picture(
            &overview.conn,
            picture,
            buffer,
            format,
            &render::CreatePictureAux::new(),
        )?
        .check()?;
        overview.picture = picture;
        Ok(overview)
    }

    pub fn resized(&mut self, event: &ConfigureNotifyEvent) -> bool {
        let extent = (
            event
                .width
                .saturating_add(event.border_width.saturating_mul(2)),
            event
                .height
                .saturating_add(event.border_width.saturating_mul(2)),
        );
        if self
            .images
            .get(&event.window)
            .is_some_and(|i| i.extent != extent)
        {
            self.images.remove(&event.window);
            return true;
        }
        false
    }

    pub fn damaged(&self, id: damage::Damage) -> bool {
        self.images.values().any(|image| image.damage == id)
    }

    pub fn hit(&self, x: i32, y: i32) -> Option<Window> {
        self.tiles
            .iter()
            .rev()
            .find(|(_, r)| {
                x >= r.x
                    && y >= r.y
                    && x < r.x.saturating_add(r.w as i32)
                    && y < r.y.saturating_add(r.h as i32)
            })
            .map(|(win, _)| *win)
    }

    pub fn draw(
        &mut self,
        placements: &Placements,
        scale: f32,
        focused: Option<Window>,
    ) -> XResult<()> {
        self.images
            .retain(|win, _| placements.iter().any(|(w, _, _)| w == win));
        self.tiles.clear();
        let rect = Rectangle {
            x: 0,
            y: 0,
            width: self.workarea.w.clamp(1, u16::MAX as u32) as u16,
            height: self.workarea.h.clamp(1, u16::MAX as u32) as u16,
        };
        render::fill_rectangles(
            &self.conn,
            render::PictOp::SRC,
            self.picture,
            render::Color {
                red: 0,
                green: 0,
                blue: 0,
                alpha: u16::MAX,
            },
            &[rect],
        )?;
        // Render transforms map destination coordinates back into the source.
        // The source is the full client pixmap, including its X11 border.
        let inverse = (65536.0 / scale) as i32;
        let transform = render::Transform {
            matrix11: inverse,
            matrix12: 0,
            matrix13: 0,
            matrix21: 0,
            matrix22: inverse,
            matrix23: 0,
            matrix31: 0,
            matrix32: 0,
            matrix33: 65536,
        };
        for &(win, tile, _) in placements {
            if !self.images.contains_key(&win) {
                match Image::new(self.conn.clone(), win, &self.formats) {
                    Ok(image) => {
                        self.images.insert(win, image);
                    }
                    Err(error) => {
                        log::debug!("Overview image {win:#x}: {error}");
                        continue;
                    }
                }
            }
            let image = &self.images[&win];
            render::set_picture_transform(&self.conn, image.picture, transform)?;
            // Clip before converting coordinates to the signed 16-bit wire space.
            // Offscreen columns can have arbitrary world positions.
            let x = tile.x.max(self.workarea.x);
            let y = tile.y.max(self.workarea.y);
            let right = tile
                .x
                .saturating_add(tile.w as i32)
                .min(self.workarea.x.saturating_add(self.workarea.w as i32));
            let bottom = tile
                .y
                .saturating_add(tile.h as i32)
                .min(self.workarea.y.saturating_add(self.workarea.h as i32));
            if right > x && bottom > y {
                render::composite(
                    &self.conn,
                    render::PictOp::OVER,
                    image.picture,
                    x11rb::NONE,
                    self.picture,
                    (x - tile.x).min(i16::MAX as i32) as i16,
                    (y - tile.y).min(i16::MAX as i32) as i16,
                    0,
                    0,
                    (x - self.workarea.x) as i16,
                    (y - self.workarea.y) as i16,
                    (right - x) as u16,
                    (bottom - y) as u16,
                )?;
                self.tiles.push((
                    win,
                    Rect::new(x, y, (right - x) as u32, (bottom - y) as u32),
                ));
                if focused == Some(win) {
                    let color = render::Color {
                        red: 0x7777,
                        green: 0xbbbb,
                        blue: u16::MAX,
                        alpha: u16::MAX,
                    };
                    let edge = Rectangle {
                        x: (x - self.workarea.x) as i16,
                        y: (y - self.workarea.y) as i16,
                        width: (right - x) as u16,
                        height: 2.min((bottom - y) as u16),
                    };
                    render::fill_rectangles(
                        &self.conn,
                        render::PictOp::SRC,
                        self.picture,
                        color,
                        &[edge],
                    )?;
                }
            }
            damage::subtract(&self.conn, image.damage, x11rb::NONE, x11rb::NONE)?;
        }
        render::composite(
            &self.conn,
            render::PictOp::SRC,
            self.picture,
            x11rb::NONE,
            self.output,
            0,
            0,
            0,
            0,
            0,
            0,
            rect.width,
            rect.height,
        )?;
        self.conn.map_window(self.window)?;
        self.conn.configure_window(
            self.window,
            &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
        )?;
        Ok(())
    }
}

impl WindowManager {
    pub(super) fn arrange_overview(&mut self, mi: usize) -> XResult<()> {
        let mon = &self.engine.state.monitors[mi];
        let (view, workarea, scale, focused) = (
            mon.ws().id,
            mon.workarea,
            mon.ws().overview_scale,
            mon.focused,
        );
        if self
            .overviews
            .get(&mi)
            .is_some_and(|o| o.view != view || o.workarea != workarea)
        {
            self.overviews.remove(&mi);
        }
        if !self.overviews.contains_key(&mi) {
            let overview = Overview::new(self.conn.clone(), self.screen_num, view, workarea)?;
            self.overviews.insert(mi, overview);
        }
        if self.engine.state.monitors[mi]
            .ws()
            .columns
            .iter()
            .flat_map(|c| &c.windows)
            .chain(self.engine.state.monitors[mi].ws().floats.iter())
            .any(|win| {
                !self.engine.state.monitors[mi]
                    .ws()
                    .overview_rects
                    .contains_key(win)
            })
        {
            let mut logical = Vec::new();
            crate::core::layout::arrange_logical(
                &self.engine.state,
                mi,
                &self.engine.cfg,
                &mut logical,
                &mut self.ribbon_scratch,
            );
            // Only new clients receive their initial layout configure. Existing
            // applications keep the exact geometry captured on entry.
            for (win, rect, border) in logical {
                if !self.engine.state.monitors[mi]
                    .ws()
                    .overview_rects
                    .contains_key(&win)
                {
                    self.apply_geom(win, rect, border, true)?;
                    self.engine.state.monitors[mi]
                        .ws_mut()
                        .overview_rects
                        .insert(win, (rect, border));
                }
            }
        }
        arrange(
            &self.engine.state,
            mi,
            &self.engine.cfg,
            &mut self.desired,
            &mut self.ribbon_scratch,
        );
        let mon = &self.engine.state.monitors[mi];
        self.desired.sort_by_key(|(win, _, _)| {
            let floating = self
                .engine
                .state
                .clients
                .get(win)
                .is_some_and(Client::is_float);
            let rank = mon
                .focus_stack
                .iter()
                .position(|w| w == win)
                .map_or(0, |i| i + 1);
            (floating, rank, *win)
        });
        self.overviews
            .get_mut(&mi)
            .unwrap()
            .draw(&self.desired, scale, focused)?;
        self.desired.clear();
        Ok(())
    }

    pub(super) fn overview_pointer(
        &mut self,
        window: Window,
        x: i32,
        y: i32,
        time: u32,
        click: bool,
    ) -> XResult<bool> {
        let Some((&mi, overview)) = self.overviews.iter().find(|(_, o)| o.window == window) else {
            return Ok(false);
        };
        if !click && !self.ptr_truth.enter_carries_intent(x as i16, y as i16) {
            return Ok(true);
        }
        let hit = overview.hit(x, y);
        self.last_event_time = time;
        self.ptr_truth.note(x, y);
        self.engine.state.sel_mon = mi;
        if click || self.engine.cfg.focus_mouse {
            if let Some(win) = hit {
                if self.engine.state.monitors[mi].focused != Some(win) {
                    self.do_action(Action::FocusWindow(win))?;
                }
            }
        }
        Ok(true)
    }
}
