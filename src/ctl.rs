//! Session-bus interface of the running tray, and the `podbar <command>`
//! client that talks to it. Owning the bus name also keeps podbar single
//! instance.

use std::sync::{Arc, Mutex};

use tokio::sync::mpsc::UnboundedSender;
use zbus::Connection;

use crate::aap::Noise;
use crate::app::{Cmd, Ev};

pub const NAME: &str = "io.github.giogandola98.Podbar";
pub const PATH: &str = "/io/github/giogandola98/Podbar";
const IFACE: &str = "io.github.giogandola98.Podbar1";

pub struct Ctl {
    pub tx: UnboundedSender<Ev>,
    /// human-readable AirPods state, kept current by the app
    pub status: Arc<Mutex<String>>,
}

impl Ctl {
    fn send(&self, c: Cmd) -> zbus::fdo::Result<()> {
        self.tx.send(Ev::Cmd(c)).map_err(|_| zbus::fdo::Error::Failed("shutting down".into()))
    }
}

#[zbus::interface(name = "io.github.giogandola98.Podbar1")]
impl Ctl {
    fn set_noise(&self, mode: &str) -> zbus::fdo::Result<()> {
        let n = Noise::parse(mode).ok_or_else(|| zbus::fdo::Error::InvalidArgs(format!("unknown mode {mode}")))?;
        self.send(Cmd::Noise(n))
    }

    fn set_conversational_awareness(&self, on: bool) -> zbus::fdo::Result<()> {
        self.send(Cmd::Ca(on))
    }

    fn set_ear_detection(&self, on: bool) -> zbus::fdo::Result<()> {
        self.send(Cmd::EarDetection(on))
    }

    fn status(&self) -> String {
        self.status.lock().unwrap().clone()
    }
}

fn on_off(v: Option<&String>) -> Result<bool, Box<dyn std::error::Error>> {
    match v.map(String::as_str) {
        Some("on" | "1" | "true") => Ok(true),
        Some("off" | "0" | "false") => Ok(false),
        _ => Err("expected on or off".into()),
    }
}

pub async fn client(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let conn = Connection::session().await?;
    let iface = Some(IFACE);
    match args[0].as_str() {
        "status" => {
            let reply = conn.call_method(Some(NAME), PATH, iface, "Status", &()).await?;
            println!("{}", reply.body().deserialize::<String>()?);
        }
        "noise" => {
            let mode = args.get(1).ok_or("usage: podbar noise off|transparency|adaptive|anc")?;
            conn.call_method(Some(NAME), PATH, iface, "SetNoise", &(mode.as_str(),)).await?;
        }
        "ca" => {
            let on = on_off(args.get(1))?;
            conn.call_method(Some(NAME), PATH, iface, "SetConversationalAwareness", &(on,)).await?;
        }
        "ear" => {
            let on = on_off(args.get(1))?;
            conn.call_method(Some(NAME), PATH, iface, "SetEarDetection", &(on,)).await?;
        }
        other => return Err(format!("unknown command {other} (see podbar --help)").into()),
    }
    Ok(())
}
