//! The tray icon and its menu (StatusNotifierItem + dbusmenu via ksni).
//! Left click opens the menu, like nm-applet.

use ksni::menu::{CheckmarkItem, RadioGroup, RadioItem, StandardItem, SubMenu};
use ksni::{MenuItem, ToolTip};
use tokio::sync::mpsc::UnboundedSender;

use crate::aap::{Level, Noise};
use crate::app::{Cmd, Ev, Pods};
use crate::bluez::{Device, Snapshot};
use crate::text::t;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct View {
    pub snap: Snapshot,
    pub pods: Option<Pods>,
    pub ear_detection: bool,
}

pub struct PodTray {
    pub view: View,
    pub tx: UnboundedSender<Ev>,
}

fn level(l: &Level) -> String {
    format!("{}%{}", l.percent, if l.charging { " ⚡" } else { "" })
}

/// "S 85% · D 90% · Custodia 60% ⚡"
pub fn battery_line(p: &Pods) -> String {
    let mut parts = Vec::new();
    if let Some(l) = &p.headset {
        parts.push(level(l));
    }
    if let Some(l) = &p.left {
        parts.push(format!("{} {}", t().left, level(l)));
    }
    if let Some(l) = &p.right {
        parts.push(format!("{} {}", t().right, level(l)));
    }
    if let Some(l) = &p.case {
        parts.push(format!("{} {}", t().case, level(l)));
    }
    parts.join("  ·  ")
}

fn noise_label(n: Noise) -> &'static str {
    match n {
        Noise::Off => t().noise_off,
        Noise::Transparency => t().noise_transparency,
        Noise::Adaptive => t().noise_adaptive,
        Noise::Anc => t().noise_anc,
    }
}

fn display_name(d: &Device) -> String {
    if d.name.is_empty() {
        d.address.clone()
    } else {
        d.name.clone()
    }
}

fn device_icon(d: &Device) -> String {
    if d.icon.is_empty() {
        "bluetooth-active".into()
    } else {
        d.icon.clone()
    }
}

/// Rows reserved for devices found by a search. The menu keeps a fixed
/// layout and only toggles visibility: a layout change makes the host rebuild
/// the menu (badly, while it is open) and renumbers every item.
const FOUND_SLOTS: usize = 15;

impl PodTray {
    fn send(&self, c: Cmd) {
        let _ = self.tx.send(Ev::Cmd(c));
    }

    fn row_label(&self, d: &Device) -> String {
        let mut label = display_name(d);
        let pods_battery = self.pods_of(d).and_then(|p| p.min_level());
        if let Some(b) = pods_battery.or(d.battery) {
            label.push_str(&format!("  ·  {b}%"));
        }
        label
    }

    fn pods_of(&self, d: &Device) -> Option<&Pods> {
        self.view.pods.as_ref().filter(|p| p.address == d.address)
    }

    fn connect_toggle(d: &Device) -> Box<dyn Fn(&mut Self) + Send> {
        let path = d.path.clone();
        let connected = d.connected;
        Box::new(move |this: &mut Self| {
            this.send(if connected { Cmd::Disconnect(path.clone()) } else { Cmd::Connect(path.clone()) })
        })
    }

    /// A paired device: a checkmark row that connects or disconnects it.
    /// AirPods get a submenu instead, with their controls inside: the main
    /// menu then keeps its height when they connect. GTK cannot grow a popup
    /// that is already shown on Wayland, it would turn it into a scrolled one.
    fn device_item(&self, d: &Device, powered: bool) -> MenuItem<Self> {
        if !d.aap {
            return CheckmarkItem {
                label: self.row_label(d),
                checked: d.connected,
                icon_name: device_icon(d),
                visible: powered,
                activate: Self::connect_toggle(d),
                ..Default::default()
            }
            .into();
        }
        // Always the same items; `p` decides which of them are visible.
        let p = self.pods_of(d);
        let shown = p.is_some();
        let battery = p.map(battery_line).unwrap_or_default();
        let noise = p.and_then(|p| p.noise);
        let ca = p.and_then(|p| p.ca);
        let ear = self.view.ear_detection;
        let submenu = vec![
            CheckmarkItem {
                label: t().connected.into(),
                checked: d.connected,
                activate: Self::connect_toggle(d),
                ..Default::default()
            }
            .into(),
            StandardItem { label: battery.clone(), enabled: false, visible: !battery.is_empty(), ..Default::default() }
                .into(),
            MenuItem::SeparatorIf(noise.is_some()),
            RadioGroup {
                selected: noise.and_then(|c| Noise::ALL.iter().position(|&n| n == c)).unwrap_or(0),
                select: Box::new(|this: &mut Self, i| {
                    if let Some(&n) = Noise::ALL.get(i) {
                        this.send(Cmd::Noise(n));
                    }
                }),
                options: Noise::ALL
                    .iter()
                    .map(|&n| RadioItem { label: noise_label(n).into(), visible: noise.is_some(), ..Default::default() })
                    .collect(),
            }
            .into(),
            MenuItem::SeparatorIf(shown),
            CheckmarkItem {
                label: t().conversational.into(),
                checked: ca == Some(true),
                visible: ca.is_some(),
                activate: Box::new(move |this: &mut Self| this.send(Cmd::Ca(ca != Some(true)))),
                ..Default::default()
            }
            .into(),
            CheckmarkItem {
                label: t().ear_detection.into(),
                checked: ear,
                visible: shown,
                activate: Box::new(move |this: &mut Self| this.send(Cmd::EarDetection(!ear))),
                ..Default::default()
            }
            .into(),
        ];
        SubMenu { label: self.row_label(d), icon_name: device_icon(d), submenu, visible: powered, ..Default::default() }
            .into()
    }

    fn add_menu(&self, powered: bool) -> MenuItem<Self> {
        let mut found: Vec<&Device> =
            self.view.snap.devices.iter().filter(|d| !d.paired && !d.name.is_empty() && d.rssi.is_some()).collect();
        // strongest signals first, then shown by name: rows that move while
        // the pointer is on them get clicked by mistake
        found.sort_by_key(|d| std::cmp::Reverse(d.rssi));
        found.truncate(FOUND_SLOTS);
        found.sort_by_key(|d| display_name(d).to_lowercase());
        // Searching starts when this submenu opens and stops when it closes,
        // so there is no button: the header never changes while it is open.
        let mut sub: Vec<MenuItem<Self>> =
            vec![StandardItem { label: t().searching.into(), enabled: false, ..Default::default() }.into()];
        for i in 0..FOUND_SLOTS {
            sub.push(match found.get(i) {
                Some(d) => {
                    let path = d.path.clone();
                    StandardItem {
                        label: display_name(d),
                        icon_name: device_icon(d),
                        activate: Box::new(move |this: &mut Self| this.send(Cmd::Pair(path.clone()))),
                        ..Default::default()
                    }
                    .into()
                }
                None => StandardItem { visible: false, ..Default::default() }.into(),
            });
        }
        SubMenu {
            label: t().add_device.into(),
            icon_name: "list-add".into(),
            submenu: sub,
            visible: powered,
            ..Default::default()
        }
        .into()
    }

    fn remove_menu(&self, paired: &[&Device], powered: bool) -> MenuItem<Self> {
        let sub = paired
            .iter()
            .map(|d| {
                let path = d.path.clone();
                StandardItem {
                    label: display_name(d),
                    icon_name: device_icon(d),
                    activate: Box::new(move |this: &mut Self| this.send(Cmd::Remove(path.clone()))),
                    ..Default::default()
                }
                .into()
            })
            .collect();
        SubMenu {
            label: t().remove_device.into(),
            icon_name: "list-remove".into(),
            submenu: sub,
            enabled: !paired.is_empty(),
            visible: powered,
            ..Default::default()
        }
        .into()
    }
}

impl ksni::Tray for PodTray {
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        "podbar".into()
    }

    fn title(&self) -> String {
        t().bluetooth.into()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::Hardware
    }

    fn icon_name(&self) -> String {
        match &self.view.snap.adapter {
            Some(a) if a.powered => {
                if self.view.snap.devices.iter().any(|d| d.connected) {
                    "bluetooth-paired".into()
                } else {
                    "bluetooth-active".into()
                }
            }
            _ => "bluetooth-disabled".into(),
        }
    }

    fn tool_tip(&self) -> ToolTip {
        let mut lines: Vec<String> = Vec::new();
        for d in self.view.snap.devices.iter().filter(|d| d.connected) {
            match self.view.pods.as_ref().filter(|p| p.address == d.address) {
                Some(p) => {
                    let b = battery_line(p);
                    lines.push(if b.is_empty() { p.name.clone() } else { format!("{}: {b}", p.name) });
                }
                None => lines.push(match d.battery {
                    Some(b) => format!("{}: {b}%", display_name(d)),
                    None => display_name(d),
                }),
            }
        }
        ToolTip { title: t().bluetooth.into(), description: lines.join("\n"), ..Default::default() }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let Some(adapter) = self.view.snap.adapter.clone() else {
            return vec![StandardItem { label: t().no_adapter.into(), enabled: false, ..Default::default() }.into()];
        };
        let powered = adapter.powered;
        let mut items: Vec<MenuItem<Self>> = vec![
            CheckmarkItem {
                label: t().bluetooth.into(),
                checked: powered,
                activate: Box::new(move |this: &mut Self| this.send(Cmd::Power(!powered))),
                ..Default::default()
            }
            .into(),
            MenuItem::SeparatorIf(powered),
        ];
        let paired: Vec<&Device> = self.view.snap.devices.iter().filter(|d| d.paired).collect();
        items.push(
            StandardItem {
                label: t().no_devices.into(),
                enabled: false,
                visible: powered && paired.is_empty(),
                ..Default::default()
            }
            .into(),
        );
        for d in &paired {
            items.push(self.device_item(d, powered));
        }
        items.push(MenuItem::SeparatorIf(powered));
        items.push(self.add_menu(powered));
        items.push(self.remove_menu(&paired, powered));
        items
    }

    /// Opening "Add device" starts a search, closing it stops the search:
    /// clicking an item would close the menu, so the search has no button to
    /// wait for.
    fn submenu_toggled(&mut self, label: &str, opened: bool) {
        if label == t().add_device {
            self.send(Cmd::AutoScan(opened));
        }
    }
}
