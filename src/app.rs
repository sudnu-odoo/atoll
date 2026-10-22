//! Wiring: starts services, builds the island, and connects their channels.

use std::sync::{Arc, Mutex};

use gtk::{glib, Application};

use crate::cards::peek::{Kind, Peek};
use crate::island::{self, Island};
use crate::services::{bluez, lyrics, mpris, upower};
use crate::window;

pub fn build(app: &Application) {
    let hit: island::HitRect = Arc::new(Mutex::new((0, 0, 0, 0)));
    let (music_tx, music_rx) = async_channel::unbounded();
    let commands = mpris::spawn(music_tx);
    let (lyrics_tx, lyrics_rx) = async_channel::unbounded();
    let lyrics_requests = lyrics::spawn(lyrics_tx);
    let island = Island::new(hit.clone(), commands, lyrics_requests);

    let (hover_tx, hover_rx) = async_channel::unbounded();
    if !window::show(app, &island.root, hit, hover_tx) {
        return;
    }

    let i = island.clone();
    glib::MainContext::default().spawn_local(async move {
        while let Ok(inside) = hover_rx.recv().await {
            i.on_hover(inside);
        }
    });
    let i = island.clone();
    glib::MainContext::default().spawn_local(async move {
        while let Ok(update) = lyrics_rx.recv().await {
            i.on_lyrics(update);
        }
    });
    let (power_tx, power_rx) = async_channel::unbounded();
    upower::spawn(power_tx);
    let i = island.clone();
    glib::MainContext::default().spawn_local(async move {
        while let Ok(update) = power_rx.recv().await {
            i.on_peek(power_peek(update));
        }
    });
    let (bt_tx, bt_rx) = async_channel::unbounded();
    bluez::spawn(bt_tx);
    let i = island.clone();
    glib::MainContext::default().spawn_local(async move {
        while let Ok(device) = bt_rx.recv().await {
            i.on_peek(bluetooth_peek(device));
        }
    });

    glib::MainContext::default().spawn_local(async move {
        while let Ok(update) = music_rx.recv().await {
            island.on_music(update);
        }
    });
}

fn power_peek(update: upower::Update) -> Peek {
    match update {
        upower::Update::Plugged { percent } => Peek {
            kind: Kind::Charging,
            title: "Charging".into(),
            subtitle: "Charger connected".into(),
            percent: Some(percent),
        },
        upower::Update::Unplugged { percent } => Peek {
            kind: Kind::Unplugged,
            title: "On battery".into(),
            subtitle: "Charger disconnected".into(),
            percent: Some(percent),
        },
    }
}

fn bluetooth_peek(device: bluez::Connected) -> Peek {
    let subtitle = match device.battery {
        Some(b) => format!("Connected · {b}%"),
        None => "Connected".into(),
    };
    Peek {
        kind: Kind::Bluetooth {
            audio: device.audio,
        },
        title: device.name,
        subtitle,
        percent: device.battery.map(f64::from),
    }
}
