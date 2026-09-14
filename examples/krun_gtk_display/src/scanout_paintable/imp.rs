#[cfg(target_os = "linux")]
use crate::display_worker::SharedDmabuf;
use gtk::{
    cairo::{RectangleInt as CairoRect, Region},
    gdk::{self, Paintable, PaintableFlags, RGBA, Snapshot, Texture},
    glib,
    graphene::Rect,
    prelude::*,
    subclass::prelude::*,
};
#[cfg(target_os = "linux")]
use gtk::gdk::DmabufTextureBuilder;
#[cfg(target_os = "linux")]
use krun_display::Rect as KrunRect;

use log::debug;
use std::cell::{Cell, RefCell};

#[cfg(target_os = "linux")]
pub struct DmabufUpdate {
    pub dmabuf: SharedDmabuf,
    pub damage_area: Option<KrunRect>,
}

#[derive(glib::Properties)]
#[properties(wrapper_type = super::ScanoutPaintable)]
pub struct ScanoutPaintable {
    // Store the texture that this paintable will draw.
    pub texture: RefCell<Option<Texture>>,
    #[property(get, set)]
    pub default_width: Cell<i32>,
    #[property(get, set)]
    pub default_height: Cell<i32>,
    pub update_region: RefCell<Option<Region>>,
    #[cfg(target_os = "linux")]
    pub dmabuf_update: RefCell<Option<DmabufUpdate>>,
}

#[cfg(not(target_os = "linux"))]
impl Default for ScanoutPaintable {
    fn default() -> Self {
        Self {
            texture: RefCell::new(None),
            default_width: Cell::new(0),
            default_height: Cell::new(0),
            update_region: RefCell::new(None),
        }
    }
}

#[cfg(target_os = "linux")]
impl Default for ScanoutPaintable {
    fn default() -> Self {
        Self {
            texture: RefCell::new(None),
            default_width: Cell::new(0),
            default_height: Cell::new(0),
            update_region: RefCell::new(None),
            dmabuf_update: RefCell::new(None),
        }
    }
}

#[glib::object_subclass]
impl ObjectSubclass for ScanoutPaintable {
    const NAME: &'static str = "ScanoutPaintable";
    type Type = super::ScanoutPaintable;
    type Interfaces = (Paintable,);
}

#[glib::derived_properties]
impl ObjectImpl for ScanoutPaintable {
    fn dispose(&self) {
        debug!("ScanoutPaintable::dispose");
    }
}

#[cfg(target_os = "linux")]
pub fn build_dmabuf_texture(update: DmabufUpdate, old_texture: Option<&Texture>) -> Option<Texture> {
    let dmabuf = update.dmabuf;
    let damage_area = update.damage_area;

    let n_planes = dmabuf.n_planes;
    assert_eq!(n_planes, 1);
    let mut builder = DmabufTextureBuilder::new()
        .set_display(gdk::Display::default().as_ref().unwrap())
        .set_width(dmabuf.width)
        .set_height(dmabuf.height)
        .set_fourcc(dmabuf.fourcc)
        .set_modifier(dmabuf.modifier)
        .set_n_planes(n_planes);

    if let Some(damage) = damage_area {
        log::trace!(
            "Building texture with damage area: x={}, y={}, width={}, height={}",
            damage.x,
            damage.y,
            damage.width,
            damage.height
        );

        if let Some(old_tex) = old_texture {
            let rect = CairoRect::new(
                damage.x as i32,
                damage.y as i32,
                damage.width as i32,
                damage.height as i32,
            );
            let region = Region::create_rectangle(&rect);

            builder = builder
                .set_update_texture(Some(old_tex))
                .set_update_region(Some(&region));
        }
    }

    for i in 0..n_planes as usize {
        let stride = dmabuf.strides[i];
        let offset = dmabuf.offsets[i];
        let fd = dmabuf.dmabuf_fds[i];

        builder = builder
            .set_stride(i as u32, stride)
            .set_offset(i as u32, offset);
        // SAFETY: Safe, the lifetime of the fd (on the display side) is managed by reference counting.
        unsafe {
            builder = builder.set_fd(i as u32, fd);
        }
    }

    log::debug!(
        "Building DMABUF texture: {}x{} fourcc=0x{:08x} ({}) modifier=0x{:016x} n_planes={} fds={:?} [from {}:{}]",
        dmabuf.width,
        dmabuf.height,
        dmabuf.fourcc,
        fourcc_to_string(dmabuf.fourcc),
        dmabuf.modifier,
        dmabuf.n_planes,
        &dmabuf.dmabuf_fds[..dmabuf.n_planes as usize],
        file!(),
        line!()
    );

    match unsafe {
        let dmabuf = dmabuf.clone();
        builder.build_with_release_func(move || {
            drop(dmabuf);
        })
    } {
        Ok(texture) => {
            log::debug!("Successfully built DMABUF texture");
            let texture_upcast: Texture = texture.upcast();
            Some(texture_upcast)
        }
        Err(e) => {
            log::error!(
                "Failed to build dmabuf texture: {e} (n_planes={}, fds={:?}, fourcc=0x{:08x}, modifier=0x{:016x})",
                dmabuf.n_planes,
                &dmabuf.dmabuf_fds[..dmabuf.n_planes as usize],
                dmabuf.fourcc,
                dmabuf.modifier
            );
            None
        }
    }
}

fn fourcc_to_string(fourcc: u32) -> String {
    let bytes = fourcc.to_le_bytes();
    String::from_utf8_lossy(&bytes).to_string()
}

impl PaintableImpl for ScanoutPaintable {
    fn snapshot(&self, snapshot: &Snapshot, width: f64, height: f64) {
        snapshot.append_color(
            &RGBA::BLACK,
            &Rect::new(0.0, 0.0, width as f32, height as f32),
        );

        // Texture is now built immediately in configure_dmabuf(), not lazily here
        if let Some(texture) = self.texture.borrow().as_ref() {
            snapshot.append_texture(texture, &Rect::new(0.0, 0.0, width as f32, height as f32));
            self.update_region.replace(None);
        }
    }

    fn flags(&self) -> PaintableFlags {
        PaintableFlags::empty()
    }

    fn intrinsic_aspect_ratio(&self) -> f64 {
        if let Some(texture) = self.texture.borrow().as_ref() {
            texture.width() as f64 / texture.height() as f64
        } else {
            self.default_width.get() as f64 / self.default_height.get() as f64
        }
    }

    fn intrinsic_width(&self) -> i32 {
        self.texture
            .borrow()
            .as_ref()
            .map(|t| t.width())
            .unwrap_or(self.default_width.get())
    }

    fn intrinsic_height(&self) -> i32 {
        self.texture
            .borrow()
            .as_ref()
            .map(|t| t.height())
            .unwrap_or(self.default_height.get())
    }
}
