//! Audio side of the AirPods features: MPRIS players (pause/resume on ear
//! detection) and the PipeWire/Pulse card profile and volume via pactl.

use std::time::Duration;

use tokio::process::Command;
use zbus::Connection;

const MPRIS: &str = "org.mpris.MediaPlayer2.";
const PLAYER_PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";

async fn pactl(args: &[&str]) -> Option<String> {
    let out = tokio::time::timeout(
        Duration::from_secs(3),
        Command::new("pactl").args(args).env("LC_ALL", "C").kill_on_drop(true).output(),
    )
    .await
    .ok()?
    .ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn card(address: &str) -> String {
    format!("bluez_card.{}", address.replace(':', "_"))
}

/// True when the default sink is this Bluetooth device.
pub async fn is_default_output(address: &str) -> bool {
    let id = address.replace(':', "_");
    pactl(&["get-default-sink"]).await.is_some_and(|s| s.contains(&id))
}

pub async fn active_profile(address: &str) -> Option<String> {
    let cards = pactl(&["list", "cards"]).await?;
    let name = format!("Name: {}", card(address));
    let mut in_card = false;
    for line in cards.lines().map(str::trim) {
        if line.starts_with("Name: ") {
            in_card = line == name;
        } else if in_card {
            if let Some(p) = line.strip_prefix("Active Profile: ") {
                return Some(p.to_owned());
            }
        }
    }
    None
}

pub async fn set_profile(address: &str, profile: &str) -> bool {
    pactl(&["set-card-profile", &card(address), profile]).await.is_some()
}

/// Volume of the default sink in percent (first channel).
pub async fn volume() -> Option<u32> {
    let out = pactl(&["get-sink-volume", "@DEFAULT_SINK@"]).await?;
    out.split_whitespace().find_map(|w| w.strip_suffix('%')?.parse().ok())
}

pub async fn set_volume(percent: u32) {
    pactl(&["set-sink-volume", "@DEFAULT_SINK@", &format!("{percent}%")]).await;
}

async fn player_call(conn: &Connection, name: &str, method: &str) -> zbus::Result<()> {
    conn.call_method(Some(name), PLAYER_PATH, Some(PLAYER_IFACE), method, &()).await?;
    Ok(())
}

async fn status(conn: &Connection, name: &str) -> Option<String> {
    let reply = conn
        .call_method(
            Some(name),
            PLAYER_PATH,
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &(PLAYER_IFACE, "PlaybackStatus"),
        )
        .await
        .ok()?;
    let v: zbus::zvariant::OwnedValue = reply.body().deserialize().ok()?;
    <&str>::try_from(&v).ok().map(str::to_owned)
}

/// Pauses every playing player; returns the ones it paused.
pub async fn pause_all(conn: &Connection) -> Vec<String> {
    let Ok(dbus) = zbus::fdo::DBusProxy::new(conn).await else { return Vec::new() };
    let Ok(names) = dbus.list_names().await else { return Vec::new() };
    let mut paused = Vec::new();
    for name in names.iter().map(|n| n.as_str()).filter(|n| n.starts_with(MPRIS)) {
        if status(conn, name).await.as_deref() == Some("Playing") && player_call(conn, name, "Pause").await.is_ok() {
            paused.push(name.to_owned());
        }
    }
    paused
}

pub async fn play(conn: &Connection, players: &[String]) {
    for name in players {
        let _ = player_call(conn, name, "Play").await;
    }
}
