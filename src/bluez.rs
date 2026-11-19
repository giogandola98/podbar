//! BlueZ model: one snapshot of the adapter and devices from ObjectManager,
//! plus the few method calls the menu needs.

use std::collections::HashMap;

use zbus::fdo::ObjectManagerProxy;
use zbus::zvariant::{Array, ObjectPath, OwnedValue, Value};
use zbus::Connection;

/// Service UUID of Apple's accessory protocol (AAP), advertised by AirPods.
pub const AAP_UUID: &str = "74ec2172-0bad-4d01-8f77-997b2be0722a";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Adapter {
    pub path: String,
    pub powered: bool,
    pub discovering: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Device {
    pub path: String,
    pub address: String,
    pub name: String,
    /// freedesktop icon name from BlueZ ("audio-headset", "phone", ...)
    pub icon: String,
    pub paired: bool,
    pub connected: bool,
    pub battery: Option<u8>,
    pub rssi: Option<i16>,
    /// speaks AAP (AirPods, Beats)
    pub aap: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub adapter: Option<Adapter>,
    pub devices: Vec<Device>,
}

impl Snapshot {
    pub fn device(&self, path: &str) -> Option<&Device> {
        self.devices.iter().find(|d| d.path == path)
    }
}

type Props = HashMap<String, OwnedValue>;

fn get_bool(p: &Props, k: &str) -> bool {
    p.get(k).and_then(|v| bool::try_from(v).ok()).unwrap_or(false)
}

fn get_str(p: &Props, k: &str) -> String {
    p.get(k).and_then(|v| <&str>::try_from(v).ok()).unwrap_or_default().to_owned()
}

fn has_uuid(p: &Props, uuid: &str) -> bool {
    p.get("UUIDs")
        .and_then(|v| <&Array>::try_from(v).ok())
        .is_some_and(|a| {
            a.inner().iter().any(|u| matches!(u, Value::Str(s) if s.as_str().eq_ignore_ascii_case(uuid)))
        })
}

pub async fn snapshot(conn: &Connection) -> zbus::Result<Snapshot> {
    let om = ObjectManagerProxy::builder(conn).destination("org.bluez")?.path("/")?.build().await?;
    let objects = om.get_managed_objects().await?;
    let mut snap = Snapshot::default();
    for (path, ifaces) in objects {
        let mut dev: Option<Device> = None;
        let mut battery = None;
        for (iface, p) in &ifaces {
            match iface.as_str() {
                // first adapter wins (the Chromebook has one)
                "org.bluez.Adapter1" if snap.adapter.is_none() => {
                    snap.adapter = Some(Adapter {
                        path: path.to_string(),
                        powered: get_bool(p, "Powered"),
                        discovering: get_bool(p, "Discovering"),
                    });
                }
                "org.bluez.Device1" => {
                    let alias = get_str(p, "Alias");
                    dev = Some(Device {
                        path: path.to_string(),
                        address: get_str(p, "Address"),
                        // Alias falls back to the address when there is no name
                        name: if p.contains_key("Name") { alias } else { String::new() },
                        icon: get_str(p, "Icon"),
                        paired: get_bool(p, "Paired"),
                        connected: get_bool(p, "Connected"),
                        battery: None,
                        rssi: p.get("RSSI").and_then(|v| i16::try_from(v).ok()),
                        aap: has_uuid(p, AAP_UUID),
                    });
                }
                "org.bluez.Battery1" => battery = p.get("Percentage").and_then(|v| u8::try_from(v).ok()),
                _ => {}
            }
        }
        if let Some(mut d) = dev {
            d.battery = battery;
            snap.devices.push(d);
        }
    }
    snap.devices.sort_by(|a, b| {
        b.connected.cmp(&a.connected).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(snap)
}

async fn call(conn: &Connection, path: &str, iface: &str, method: &str) -> zbus::Result<()> {
    let path = ObjectPath::try_from(path)?;
    conn.call_method(Some("org.bluez"), &path, Some(iface), method, &()).await?;
    Ok(())
}

async fn set_prop(conn: &Connection, path: &str, iface: &str, name: &str, value: bool) -> zbus::Result<()> {
    let path = ObjectPath::try_from(path)?;
    conn.call_method(
        Some("org.bluez"),
        &path,
        Some("org.freedesktop.DBus.Properties"),
        "Set",
        &(iface, name, Value::from(value)),
    )
    .await?;
    Ok(())
}

pub async fn set_powered(conn: &Connection, adapter: &str, on: bool) -> zbus::Result<()> {
    set_prop(conn, adapter, "org.bluez.Adapter1", "Powered", on).await
}

pub async fn connect(conn: &Connection, dev: &str) -> zbus::Result<()> {
    call(conn, dev, "org.bluez.Device1", "Connect").await
}

pub async fn disconnect(conn: &Connection, dev: &str) -> zbus::Result<()> {
    call(conn, dev, "org.bluez.Device1", "Disconnect").await
}

/// Pair, trust (so it can reconnect by itself) and connect.
pub async fn pair(conn: &Connection, dev: &str) -> zbus::Result<()> {
    call(conn, dev, "org.bluez.Device1", "Pair").await?;
    set_prop(conn, dev, "org.bluez.Device1", "Trusted", true).await?;
    // LE mice and keyboards are usually connected by the pairing itself
    if is_connected(conn, dev).await {
        return Ok(());
    }
    match connect(conn, dev).await {
        Err(zbus::Error::MethodError(name, _, _))
            if matches!(name.as_str(), "org.bluez.Error.AlreadyConnected" | "org.bluez.Error.InProgress") =>
        {
            Ok(())
        }
        r => r,
    }
}

pub async fn remove(conn: &Connection, adapter: &str, dev: &str) -> zbus::Result<()> {
    let adapter = ObjectPath::try_from(adapter)?;
    let dev = ObjectPath::try_from(dev)?;
    conn.call_method(Some("org.bluez"), &adapter, Some("org.bluez.Adapter1"), "RemoveDevice", &(dev,))
        .await?;
    Ok(())
}

pub async fn discovery(conn: &Connection, adapter: &str, on: bool) -> zbus::Result<()> {
    call(conn, adapter, "org.bluez.Adapter1", if on { "StartDiscovery" } else { "StopDiscovery" }).await
}

async fn device_prop(conn: &Connection, dev: &str, name: &str) -> Option<OwnedValue> {
    let path = ObjectPath::try_from(dev).ok()?;
    let reply = conn
        .call_method(
            Some("org.bluez"),
            &path,
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &("org.bluez.Device1", name),
        )
        .await
        .ok()?;
    reply.body().deserialize::<OwnedValue>().ok()
}

pub async fn is_paired(conn: &Connection, dev: &str) -> bool {
    device_prop(conn, dev, "Paired").await.and_then(|v| bool::try_from(&v).ok()).unwrap_or(false)
}

pub async fn is_connected(conn: &Connection, dev: &str) -> bool {
    device_prop(conn, dev, "Connected").await.and_then(|v| bool::try_from(&v).ok()).unwrap_or(false)
}

pub async fn alias(conn: &Connection, dev: &str) -> String {
    device_prop(conn, dev, "Alias")
        .await
        .and_then(|v| <&str>::try_from(&v).ok().map(str::to_owned))
        .unwrap_or_else(|| dev.rsplit('/').next().unwrap_or(dev).trim_start_matches("dev_").replace('_', ":"))
}
