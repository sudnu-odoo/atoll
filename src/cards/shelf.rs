//! Shelf card: files dropped on the notch, as tiles (thumbnail or type icon,
//! name, size). Drag a tile out to hand the file to another app; × removes
//! it, Clear empties the shelf. Dropping happens on the whole island (see
//! island.rs), which opens this card.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, gio, glib, Align, Label, Orientation};

use super::control_button;
use super::widgets::Cover;
use crate::shelf::Command;

const TILE_W: i32 = 92;
const THUMB: i32 = 56;

pub struct ShelfCard {
    pub root: gtk::Box,
    tiles: gtk::Box,
    empty: gtk::Box,
    scroller: gtk::ScrolledWindow,
    clear: gtk::Button,
    commands: async_channel::Sender<Command>,
    /// Tells the island a drag out is in progress, so it stays open.
    on_drag: Rc<dyn Fn(bool)>,
}

impl ShelfCard {
    pub fn new(
        commands: async_channel::Sender<Command>,
        on_drag: impl Fn(bool) + 'static,
    ) -> Rc<Self> {
        // Top row (over the GNOME bar; its Clear button is clicked through the
        // island's bar-click handling).
        // The island's shared top row (tabs, clock, settings) names the card
        // and shows the time; this row keeps only the card's own actions.
        let clear = control_button(&["shelf-clear"], None);
        clear.set_label("Clear");
        let top = gtk::CenterBox::builder()
            .css_classes(["card-top"])
            .start_widget(&clear)
            .build();

        let tiles = gtk::Box::builder().spacing(8).build();
        // Many files scroll sideways; scrolling here doesn't flip cards.
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .min_content_height(120)
            .hexpand(true)
            .child(&tiles)
            .build();

        let empty = gtk::Box::builder()
            .orientation(Orientation::Vertical)
            .spacing(6)
            .css_classes(["shelf-empty"])
            .valign(Align::Center)
            .halign(Align::Center)
            .hexpand(true)
            .vexpand(true)
            .build();
        let icon = gtk::Image::from_icon_name("atoll-tray-symbolic");
        icon.add_css_class("shelf-empty-icon");
        empty.append(&icon);
        empty.append(
            &Label::builder()
                .label("Drop files here")
                .css_classes(["claude-title"])
                .build(),
        );
        empty.append(
            &Label::builder()
                .label("Drag any file onto the island to keep it handy, then drag it out where you need it")
                .css_classes(["subtitle"])
                .wrap(true)
                .width_chars(44)
                .max_width_chars(46)
                .justify(gtk::Justification::Center)
                .build(),
        );

        let body = gtk::Box::builder().css_classes(["shelf-body"]).build();
        body.append(&empty);
        body.append(&scroller);

        let root = gtk::Box::builder()
            .orientation(Orientation::Vertical)
            .css_classes(["water-card"])
            .width_request(crate::island::EXPANDED_SHELF.0 as i32)
            .height_request(crate::island::EXPANDED_SHELF.1 as i32)
            .valign(Align::Start)
            .build();
        root.append(&top);
        root.append(&body);

        let card = Rc::new(Self {
            root,
            tiles,
            empty,
            scroller,
            clear,
            commands,
            on_drag: Rc::new(on_drag),
        });
        let tx = card.commands.clone();
        card.clear.connect_clicked(move |_| {
            let _ = tx.try_send(Command::Clear);
        });
        card.update(&[]);
        card
    }

    pub fn update(self: &Rc<Self>, items: &[PathBuf]) {
        while let Some(child) = self.tiles.first_child() {
            self.tiles.remove(&child);
        }
        let empty = items.is_empty();
        self.empty.set_visible(empty);
        self.scroller.set_visible(!empty);
        self.clear.set_visible(!empty);
        for path in items {
            self.tiles.append(&self.tile(path));
        }
    }

    fn tile(self: &Rc<Self>, path: &Path) -> gtk::Overlay {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let meta = std::fs::metadata(path).ok();
        let is_dir = meta.as_ref().is_some_and(|m| m.is_dir());

        // Images get a real thumbnail (loaded off the UI thread); the rest
        // the icon of their file type.
        let thumb: gtk::Widget = if !is_dir && is_image(&name) {
            let cover = Cover::new(THUMB, 10.0);
            let area = cover.area.clone();
            area.set_halign(Align::Center);
            load_thumbnail(path.to_path_buf(), cover);
            area.upcast()
        } else {
            let icon = if is_dir {
                gio::ThemedIcon::new("folder").upcast::<gio::Icon>()
            } else {
                let (kind, _) = gio::content_type_guess(Some(path), None);
                let icon = gio::content_type_get_icon(&kind);
                // Unusual types may have no icon in the theme: fall back to a
                // generic document rather than a broken-image glyph.
                let known = gdk::Display::default()
                    .is_some_and(|d| gtk::IconTheme::for_display(&d).has_gicon(&icon));
                if known {
                    icon
                } else {
                    gio::ThemedIcon::from_names(&["text-x-generic", "application-x-generic"])
                        .upcast()
                }
            };
            gtk::Image::builder()
                .gicon(&icon)
                .pixel_size(THUMB)
                .halign(Align::Center)
                .build()
                .upcast()
        };

        let size = if is_dir {
            "Folder".to_owned()
        } else {
            meta.map(|m| human_size(m.len())).unwrap_or_default()
        };
        let content = gtk::Box::builder()
            .orientation(Orientation::Vertical)
            .spacing(4)
            .width_request(TILE_W)
            .css_classes(["shelf-tile"])
            .build();
        content.append(&thumb);
        content.append(
            &Label::builder()
                .label(&name)
                .css_classes(["shelf-name"])
                .ellipsize(gtk::pango::EllipsizeMode::Middle)
                .max_width_chars(11)
                .build(),
        );
        content.append(
            &Label::builder()
                .label(size)
                .css_classes(["shelf-size"])
                .build(),
        );
        content.set_tooltip_text(Some(&path.to_string_lossy()));
        content.set_cursor_from_name(Some("grab"));

        let remove = control_button(&["shelf-remove"], Some("atoll-close-symbolic"));
        remove.set_halign(Align::End);
        remove.set_valign(Align::Start);
        remove.set_tooltip_text(Some("Remove from shelf"));
        let (tx, p) = (self.commands.clone(), path.to_path_buf());
        remove.connect_clicked(move |_| {
            let _ = tx.try_send(Command::Remove(p.clone()));
        });

        let tile = gtk::Overlay::builder().child(&content).build();
        tile.add_overlay(&remove);
        self.make_draggable(&content, path);
        tile
    }

    /// Dragging a tile offers the file (as a file list and URI) to whatever
    /// it's dropped on.
    fn make_draggable(self: &Rc<Self>, widget: &gtk::Box, path: &Path) {
        let source = gtk::DragSource::new();
        source.set_actions(gdk::DragAction::COPY);
        let file = gio::File::for_path(path);
        source.connect_prepare(move |_, _, _| {
            let list = gdk::FileList::from_array(std::slice::from_ref(&file));
            Some(gdk::ContentProvider::for_value(&list.to_value()))
        });
        let w = widget.downgrade();
        source.connect_drag_begin(move |src, _| {
            if let Some(w) = w.upgrade() {
                src.set_icon(Some(&gtk::WidgetPaintable::new(Some(&w))), TILE_W / 2, 30);
            }
        });
        let on_drag = self.on_drag.clone();
        source.connect_drag_begin(move |_, _| on_drag(true));
        let on_drag = self.on_drag.clone();
        source.connect_drag_end(move |_, _, _| on_drag(false));
        widget.add_controller(source);
    }
}

fn is_image(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [".png", ".jpg", ".jpeg", ".webp", ".gif", ".bmp", ".svg"]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

/// Decodes a small version of the image in a worker thread, then shows it.
/// Decoded pixels; plain data, so it can come back from the worker thread
/// (a Pixbuf can't cross threads).
struct Pixels {
    bytes: glib::Bytes,
    width: i32,
    height: i32,
    stride: usize,
    alpha: bool,
}

/// Decodes a small version of the image in a worker thread, then shows it.
fn load_thumbnail(path: PathBuf, cover: Cover) {
    glib::MainContext::default().spawn_local(async move {
        let decoded = gio::spawn_blocking(move || {
            let pb = gtk::gdk_pixbuf::Pixbuf::from_file_at_scale(&path, THUMB * 2, THUMB * 2, true)
                .ok()?;
            Some(Pixels {
                bytes: pb.read_pixel_bytes(),
                width: pb.width(),
                height: pb.height(),
                stride: usize::try_from(pb.rowstride()).ok()?,
                alpha: pb.has_alpha(),
            })
        })
        .await;
        if let Ok(Some(p)) = decoded {
            let format = if p.alpha {
                gdk::MemoryFormat::R8g8b8a8
            } else {
                gdk::MemoryFormat::R8g8b8
            };
            let texture = gdk::MemoryTexture::new(p.width, p.height, format, &p.bytes, p.stride);
            cover.set(Some(texture.upcast_ref()));
        }
    });
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2.0 KB");
        assert_eq!(human_size(2_500_000), "2.4 MB");
    }

    #[test]
    fn images_by_extension() {
        assert!(is_image("Beach.JPG"));
        assert!(is_image("x.webp"));
        assert!(!is_image("notes.txt"));
        assert!(!is_image("png"));
    }
}
