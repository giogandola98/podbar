//! Main loop: BlueZ signals, menu commands and AirPods packets all arrive
//! as events on one channel; the state is rebuilt and pushed to the tray
//! only when the visible part changed.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use ksni::TrayMethods;
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};
use zbus::message::Type as MsgType;
use zbus::{Connection, MatchRule, MessageStream};

use crate::aap::{self, Level, Link, Msg, Noise, Part, Place};
use crate::bluez::{self, Snapshot};
use crate::ctl::{self, Ctl};
use crate::text::t;
use crate::tray::{battery_line, PodTray, View};
use crate::{agent, media, notify};

const SCAN_SECS: u64 = 60;
const PAIR_WINDOW_SECS: u64 = 60;
const AAP_RETRY: Duration = Duration::from_secs(20);
const LOW_BATTERY: u8 = 10;

pub enum Cmd {
    Power(bool),
    Connect(String),
    Disconnect(String),
    Pair(String),
    Remove(String),
    Scan(bool),
    Noise(Noise),
    Ca(bool),
    EarDetection(bool),
}

pub enum Ev {
    Cmd(Cmd),
    Aap(aap::Event),
    Refresh,
    BluezRestarted,
    ScanTimeout(u64),
    /// ask for notifications if the AirPods stayed silent after the handshake
    AapNudge(String),
}

impl From<aap::Event> for Ev {
    fn from(e: aap::Event) -> Ev {
        Ev::Aap(e)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pods {
    pub address: String,
    pub name: String,
    pub headset: Option<Level>,
    pub left: Option<Level>,
    pub right: Option<Level>,
    pub case: Option<Level>,
    pub noise: Option<Noise>,
    pub ca: Option<bool>,
    pub ear: (Place, Place),
}

impl Pods {
    pub fn min_level(&self) -> Option<u8> {
        [self.headset, self.left, self.right].iter().flatten().map(|l| l.percent).min()
    }

    fn charging(&self) -> bool {
        [self.headset, self.left, self.right].iter().flatten().any(|l| l.charging)
    }
}

struct App {
    sys: Connection,
    session: Connection,
    tx: UnboundedSender<Ev>,
    window: agent::Window,
    snap: Snapshot,
    pods: Option<Pods>,
    link: Option<Link>,
    connecting: Option<String>,
    last_attempt: Option<(String, Instant)>,
    scan_gen: u64,
    ear_detection: bool,
    /// players paused because a bud came out
    paused: Vec<String>,
    /// AirPods whose card profile we switched off with both buds out
    profile_off: Option<String>,
    /// volume before conversational awareness lowered it
    ca_volume: Option<u32>,
    low_warned: bool,
    /// other devices (mice, keyboards) already warned about a low battery
    low_warned_devs: Vec<String>,
}

fn config_path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(Into::into)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))?;
    Some(base.join("podbar").join("ear-detection"))
}

fn load_ear_detection() -> bool {
    config_path().and_then(|p| std::fs::read_to_string(p).ok()).is_none_or(|s| s.trim() != "off")
}

fn save_ear_detection(on: bool) {
    if let Some(p) = config_path() {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(p, if on { "on\n" } else { "off\n" });
    }
}

impl App {
    fn view(&self) -> View {
        let mut snap = self.snap.clone();
        // RSSI only orders scan results: coarse steps keep an open menu still
        for d in &mut snap.devices {
            d.rssi = d.rssi.map(|r| r / 10 * 10);
        }
        View { snap, pods: self.pods.clone(), ear_detection: self.ear_detection }
    }

    fn status_text(&self) -> String {
        let Some(p) = &self.pods else { return "no AirPods connected".into() };
        let place = |p: Place| match p {
            Place::InEar => "in ear",
            Place::Out => "out",
            Place::InCase => "in case",
            Place::Unknown => "?",
        };
        format!(
            "{}\nbattery: {}\nnoise: {}\nconversational awareness: {}\near detection: {} ({}, {})",
            p.name,
            battery_line(p),
            p.noise.map_or("?", Noise::key),
            p.ca.map_or("?", |c| if c { "on" } else { "off" }),
            if self.ear_detection { "on" } else { "off" },
            place(p.ear.0),
            place(p.ear.1),
        )
    }

    fn device_name(&self, path: &str) -> String {
        self.snap
            .device(path)
            .map(|d| if d.name.is_empty() { d.address.clone() } else { d.name.clone() })
            .unwrap_or_default()
    }

    /// Runs a BlueZ call in the background; failures become a notification.
    fn background<F>(&self, failure: &'static str, name: String, fut: F)
    where
        F: std::future::Future<Output = zbus::Result<()>> + Send + 'static,
    {
        let session = self.session.clone();
        tokio::spawn(async move {
            if let Err(e) = fut.await {
                notify::send(&session, "bluetooth-disabled", &format!("{failure}: {name}"), &e.to_string()).await;
            }
        });
    }

    async fn refresh(&mut self) {
        match bluez::snapshot(&self.sys).await {
            Ok(s) => self.snap = s,
            Err(e) => {
                eprintln!("podbar: BlueZ: {e}");
                self.snap = Snapshot::default();
            }
        }
        self.check_batteries();
        let target = self.snap.devices.iter().find(|d| d.connected && d.aap).map(|d| d.address.clone());
        if let Some(link) = &self.link {
            if target.as_deref() != Some(link.address.as_str()) {
                link.close();
                self.link = None;
                self.pods = None;
            }
        }
        let Some(addr) = target else { return };
        if self.link.is_some() || self.connecting.is_some() {
            return;
        }
        let recent = self.last_attempt.as_ref().is_some_and(|(a, at)| *a == addr && at.elapsed() < AAP_RETRY);
        if !recent {
            self.connecting = Some(addr.clone());
            self.last_attempt = Some((addr.clone(), Instant::now()));
            aap::spawn(addr, self.tx.clone());
        }
    }

    /// Low battery warning for devices that report it through BlueZ (Battery1);
    /// the AirPods have their own, from AAP.
    fn check_batteries(&mut self) {
        for d in self.snap.devices.iter().filter(|d| d.connected && !d.aap) {
            let Some(b) = d.battery else { continue };
            let warned = self.low_warned_devs.iter().position(|a| *a == d.address);
            if b <= LOW_BATTERY && warned.is_none() {
                self.low_warned_devs.push(d.address.clone());
                let name = if d.name.is_empty() { d.address.clone() } else { d.name.clone() };
                let (body, session) = (format!("{name}: {b}%"), self.session.clone());
                tokio::spawn(async move { notify::send(&session, "battery-low", t().low_battery, &body).await });
            } else if let Some(i) = warned.filter(|_| b > LOW_BATTERY + 10) {
                self.low_warned_devs.remove(i);
            }
        }
    }

    async fn handle(&mut self, ev: Ev) {
        match ev {
            Ev::Refresh => self.refresh().await,
            Ev::BluezRestarted => {
                if let Err(e) = agent::register(&self.sys, &self.session, &self.window).await {
                    eprintln!("podbar: pairing agent: {e}");
                }
                self.refresh().await;
            }
            Ev::ScanTimeout(gen) if gen == self.scan_gen => self.command(Cmd::Scan(false)).await,
            Ev::ScanTimeout(_) => {}
            Ev::Cmd(c) => self.command(c).await,
            Ev::Aap(e) => self.aap_event(e).await,
            Ev::AapNudge(addr) => {
                if let (Some(link), Some(p)) = (&self.link, &self.pods) {
                    if link.address == addr && p.min_level().is_none() {
                        aap::request_notifications(link);
                    }
                }
            }
        }
    }

    async fn command(&mut self, c: Cmd) {
        let adapter = self.snap.adapter.as_ref().map(|a| a.path.clone()).unwrap_or_default();
        let sys = self.sys.clone();
        match c {
            Cmd::Power(on) => {
                self.background(t().connect_failed, t().bluetooth.into(), async move {
                    bluez::set_powered(&sys, &adapter, on).await
                });
            }
            Cmd::Connect(dev) => {
                let name = self.device_name(&dev);
                self.background(t().connect_failed, name, async move { bluez::connect(&sys, &dev).await });
            }
            Cmd::Disconnect(dev) => {
                let name = self.device_name(&dev);
                self.background(t().connect_failed, name, async move { bluez::disconnect(&sys, &dev).await });
            }
            Cmd::Pair(dev) => {
                agent::open_window(&self.window, PAIR_WINDOW_SECS);
                let name = self.device_name(&dev);
                let session = self.session.clone();
                let done = name.clone();
                self.background(t().pair_failed, name, async move {
                    // discovery slows pairing down and is no longer needed
                    let _ = bluez::discovery(&sys, &adapter, false).await;
                    bluez::pair(&sys, &dev).await?;
                    notify::send(&session, "bluetooth-paired", t().paired, &done).await;
                    Ok(())
                });
            }
            Cmd::Remove(dev) => {
                let name = self.device_name(&dev);
                self.background(t().connect_failed, name, async move { bluez::remove(&sys, &adapter, &dev).await });
            }
            Cmd::Scan(on) => {
                self.scan_gen += 1;
                if on {
                    let (tx, gen) = (self.tx.clone(), self.scan_gen);
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_secs(SCAN_SECS)).await;
                        let _ = tx.send(Ev::ScanTimeout(gen));
                    });
                }
                self.background(t().connect_failed, t().bluetooth.into(), async move {
                    bluez::discovery(&sys, &adapter, on).await
                });
            }
            Cmd::Noise(n) => {
                if let (Some(link), Some(p)) = (&self.link, &mut self.pods) {
                    if link.send(&aap::noise_packet(n)) {
                        p.noise = Some(n);
                    }
                }
            }
            Cmd::Ca(on) => {
                if let (Some(link), Some(p)) = (&self.link, &mut self.pods) {
                    if link.send(&aap::ca_packet(on)) {
                        p.ca = Some(on);
                    }
                }
            }
            Cmd::EarDetection(on) => {
                self.ear_detection = on;
                save_ear_detection(on);
                if !on {
                    self.paused.clear();
                    if let Some(addr) = self.profile_off.take() {
                        media::set_profile(&addr, "a2dp-sink").await;
                    }
                }
            }
        }
    }

    async fn aap_event(&mut self, e: aap::Event) {
        match e {
            aap::Event::Up(link) => {
                self.connecting = None;
                let Some(d) = self.snap.devices.iter().find(|d| d.address == link.address && d.connected) else {
                    link.close();
                    return;
                };
                self.pods = Some(Pods { address: d.address.clone(), name: d.name.clone(), ..Default::default() });
                self.low_warned = false;
                let (tx, addr) = (self.tx.clone(), link.address.clone());
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    let _ = tx.send(Ev::AapNudge(addr));
                });
                self.link = Some(link);
            }
            aap::Event::Down(addr) => {
                if self.connecting.as_deref() == Some(addr.as_str()) {
                    self.connecting = None;
                    let tx = self.tx.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(AAP_RETRY).await;
                        let _ = tx.send(Ev::Refresh);
                    });
                }
                if self.link.as_ref().is_some_and(|l| l.address == addr) {
                    self.link = None;
                    self.pods = None;
                    self.paused.clear();
                    self.ca_volume = None;
                    if self.profile_off.as_deref() == Some(addr.as_str()) {
                        self.profile_off = None;
                    }
                }
            }
            aap::Event::Packet(addr, data) => {
                if self.pods.as_ref().is_some_and(|p| p.address == addr) {
                    if let Some(msg) = aap::parse(&data) {
                        self.apply(msg).await;
                    }
                }
            }
        }
    }

    async fn apply(&mut self, msg: Msg) {
        let Some(p) = &mut self.pods else { return };
        match msg {
            Msg::Battery(parts) => {
                for (part, level) in parts {
                    // a missing level keeps the last known one (bud in a closed case)
                    let Some(l) = level else { continue };
                    match part {
                        Part::Headset => p.headset = Some(l),
                        Part::Left => p.left = Some(l),
                        Part::Right => p.right = Some(l),
                        Part::Case => p.case = Some(l),
                    }
                }
                match p.min_level() {
                    Some(m) if m <= LOW_BATTERY && !p.charging() && !self.low_warned => {
                        self.low_warned = true;
                        let (body, session) = (format!("{}: {}", p.name, battery_line(p)), self.session.clone());
                        tokio::spawn(async move { notify::send(&session, "battery-low", t().low_battery, &body).await });
                    }
                    Some(m) if m > LOW_BATTERY + 10 => self.low_warned = false,
                    _ => {}
                }
            }
            Msg::Noise(n) => p.noise = Some(n),
            Msg::Ca(on) => p.ca = Some(on),
            Msg::Metadata { name, model } => {
                if p.name.is_empty() {
                    p.name = aap::model_name(&model).map(str::to_owned).unwrap_or(name);
                }
            }
            Msg::Ear(a, b) => {
                let prev = std::mem::replace(&mut p.ear, (a, b));
                let addr = p.address.clone();
                self.ear_changed(&addr, prev, (a, b)).await;
            }
            Msg::CaData(flag) => {
                let addr = p.address.clone();
                self.conversation(&addr, flag).await;
            }
            Msg::HandshakeAck | Msg::FeaturesAck => {}
        }
    }

    /// LibrePods' default behaviour: pause when one bud comes out, resume
    /// when both are back, audio off while neither is worn.
    async fn ear_changed(&mut self, addr: &str, prev: (Place, Place), now: (Place, Place)) {
        if !self.ear_detection {
            return;
        }
        let worn = |e: (Place, Place)| (e.0 == Place::InEar, e.1 == Place::InEar);
        let (p0, p1) = worn(prev);
        let (n0, n1) = worn(now);
        if p0 && p1 && !(n0 && n1) && media::is_default_output(addr).await {
            self.paused = media::pause_all(&self.session).await;
        }
        if (p0 || p1) && !(n0 || n1) {
            let a2dp = media::active_profile(addr).await.is_some_and(|p| p.starts_with("a2dp"));
            if a2dp && media::set_profile(addr, "off").await {
                self.profile_off = Some(addr.to_owned());
            }
        }
        if (n0 || n1) && self.profile_off.as_deref() == Some(addr) {
            self.profile_off = None;
            media::set_profile(addr, "a2dp-sink").await;
        }
        if n0 && n1 && !self.paused.is_empty() {
            let players = std::mem::take(&mut self.paused);
            media::play(&self.session, &players).await;
        }
    }

    /// Conversational awareness: lower the volume while the wearer speaks.
    async fn conversation(&mut self, addr: &str, flag: u8) {
        match flag {
            1 => {
                if self.ca_volume.is_none() && media::is_default_output(addr).await {
                    if let Some(v) = media::volume().await {
                        self.ca_volume = Some(v);
                        media::set_volume((v / 5).max(1)).await;
                    }
                }
            }
            8 => self.ca_volume = None,
            9 => {}
            _ => {
                if let Some(v) = self.ca_volume.take() {
                    if media::is_default_output(addr).await {
                        media::set_volume(v).await;
                    }
                }
            }
        }
    }
}

/// Forwards BlueZ activity as (coalesced) refresh events.
async fn watch_bluez(sys: Connection, tx: UnboundedSender<Ev>) -> zbus::Result<()> {
    let rule = MatchRule::builder().msg_type(MsgType::Signal).sender("org.bluez")?.build();
    let mut bluez = MessageStream::for_match_rule(rule, &sys, Some(256)).await?;
    let rule = MatchRule::builder()
        .msg_type(MsgType::Signal)
        .sender("org.freedesktop.DBus")?
        .interface("org.freedesktop.DBus")?
        .member("NameOwnerChanged")?
        .arg(0, "org.bluez")?
        .build();
    let mut owner = MessageStream::for_match_rule(rule, &sys, None).await?;
    loop {
        tokio::select! {
            Some(_) = bluez.next() => {
                // a connection or a scan is a burst of signals: one refresh
                let _ = tokio::time::timeout(Duration::from_millis(100), async {
                    while bluez.next().await.is_some() {}
                })
                .await;
                if tx.send(Ev::Refresh).is_err() {
                    return Ok(());
                }
            }
            Some(_) = owner.next() => {
                // give bluetoothd a moment to export its objects
                tokio::time::sleep(Duration::from_secs(1)).await;
                if tx.send(Ev::BluezRestarted).is_err() {
                    return Ok(());
                }
            }
            else => return Ok(()),
        }
    }
}

pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let (tx, mut rx) = unbounded_channel::<Ev>();
    let status = Arc::new(Mutex::new(String::new()));
    let session = zbus::connection::Builder::session()?
        .name(ctl::NAME)?
        .serve_at(ctl::PATH, Ctl { tx: tx.clone(), status: status.clone() })?
        .build()
        .await
        .map_err(|e| format!("cannot own {} (already running?): {e}", ctl::NAME))?;
    let sys = Connection::system().await?;
    let window: agent::Window = Arc::new(Mutex::new(None));
    if let Err(e) = agent::register(&sys, &session, &window).await {
        eprintln!("podbar: pairing agent: {e}");
    }
    {
        let (sys, tx) = (sys.clone(), tx.clone());
        tokio::spawn(async move {
            if let Err(e) = watch_bluez(sys, tx).await {
                eprintln!("podbar: BlueZ signals: {e}");
            }
        });
    }
    // At login podbar starts together with waybar: wait for the
    // StatusNotifierWatcher instead of exiting if it is not up yet.
    let tray = PodTray { view: View::default(), tx: tx.clone() }
        .assume_sni_available(true)
        .spawn()
        .await?;
    let mut app = App {
        sys,
        session,
        tx,
        window,
        snap: Snapshot::default(),
        pods: None,
        link: None,
        connecting: None,
        last_attempt: None,
        scan_gen: 0,
        ear_detection: load_ear_detection(),
        paused: Vec::new(),
        profile_off: None,
        ca_volume: None,
        low_warned: false,
        low_warned_devs: Vec::new(),
    };
    app.refresh().await;
    let mut shown: Option<View> = None;
    loop {
        let view = app.view();
        if shown.as_ref() != Some(&view) {
            shown = Some(view.clone());
            tray.update(move |t: &mut PodTray| t.view = view).await;
        }
        *status.lock().unwrap() = app.status_text();
        let Some(ev) = rx.recv().await else { break };
        app.handle(ev).await;
        while let Ok(ev) = rx.try_recv() {
            app.handle(ev).await;
        }
    }
    Ok(())
}
