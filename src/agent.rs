//! BlueZ pairing agent (org.bluez.Agent1), the part of blueman-applet that
//! answers pairing requests. Capability DisplayYesNo: codes are shown as
//! notifications. Requests are accepted only while a pairing started from
//! the menu is in progress, or for services of already-paired devices.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use zbus::zvariant::{ObjectPath, OwnedObjectPath};
use zbus::Connection;

use crate::{bluez, notify, text::t};

pub const PATH: &str = "/io/github/giogandola98/podbar/agent";

/// Until when pairing requests are accepted.
pub type Window = Arc<Mutex<Option<Instant>>>;

pub fn open_window(w: &Window, secs: u64) {
    *w.lock().unwrap() = Some(Instant::now() + Duration::from_secs(secs));
}

fn window_open(w: &Window) -> bool {
    w.lock().unwrap().is_some_and(|until| Instant::now() < until)
}

#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.bluez.Error")]
pub enum AgentError {
    #[zbus(error)]
    ZBus(zbus::Error),
    Rejected(String),
}

fn rejected() -> AgentError {
    AgentError::Rejected("not pairing from podbar".into())
}

pub struct Agent {
    system: Connection,
    session: Connection,
    window: Window,
}

impl Agent {
    async fn show(&self, device: &OwnedObjectPath, what: &str, code: String) {
        let name = bluez::alias(&self.system, device.as_str()).await;
        notify::send(&self.session, "bluetooth-active", &format!("{name}: {code}"), what).await;
    }
}

#[zbus::interface(name = "org.bluez.Agent1")]
impl Agent {
    async fn release(&self) {}

    async fn request_pin_code(&self, device: OwnedObjectPath) -> Result<String, AgentError> {
        // legacy (pre-2.1) devices with a fixed PIN; most use 0000
        if !window_open(&self.window) {
            return Err(rejected());
        }
        self.show(&device, t().type_code, "0000".into()).await;
        Ok("0000".into())
    }

    async fn display_pin_code(&self, device: OwnedObjectPath, pincode: String) -> Result<(), AgentError> {
        if !window_open(&self.window) {
            return Err(rejected());
        }
        self.show(&device, t().type_code, pincode).await;
        Ok(())
    }

    async fn request_passkey(&self, _device: OwnedObjectPath) -> Result<u32, AgentError> {
        Err(rejected())
    }

    async fn display_passkey(&self, device: OwnedObjectPath, passkey: u32, entered: u16) {
        // called again for every key typed on the keyboard: show it once
        if entered == 0 && window_open(&self.window) {
            self.show(&device, t().type_code, format!("{passkey:06}")).await;
        }
    }

    async fn request_confirmation(&self, device: OwnedObjectPath, passkey: u32) -> Result<(), AgentError> {
        if !window_open(&self.window) {
            return Err(rejected());
        }
        self.show(&device, t().confirm_code, format!("{passkey:06}")).await;
        Ok(())
    }

    async fn request_authorization(&self, _device: OwnedObjectPath) -> Result<(), AgentError> {
        if window_open(&self.window) {
            Ok(())
        } else {
            Err(rejected())
        }
    }

    async fn authorize_service(&self, device: OwnedObjectPath, _uuid: String) -> Result<(), AgentError> {
        if window_open(&self.window) || bluez::is_paired(&self.system, device.as_str()).await {
            Ok(())
        } else {
            Err(rejected())
        }
    }

    async fn cancel(&self) {}
}

/// Exports the agent and makes it BlueZ's default one. Call again after
/// bluetoothd restarts.
pub async fn register(system: &Connection, session: &Connection, window: &Window) -> zbus::Result<()> {
    let agent = Agent { system: system.clone(), session: session.clone(), window: window.clone() };
    // already exported after a bluetoothd restart: that is fine
    let _ = system.object_server().at(PATH, agent).await?;
    let path = ObjectPath::try_from(PATH)?;
    let manager = "org.bluez.AgentManager1";
    system.call_method(Some("org.bluez"), "/org/bluez", Some(manager), "RegisterAgent", &(&path, "DisplayYesNo")).await?;
    system.call_method(Some("org.bluez"), "/org/bluez", Some(manager), "RequestDefaultAgent", &(&path,)).await?;
    Ok(())
}
