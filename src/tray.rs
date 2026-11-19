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

impl PodTray {
    fn send(&self, c: Cmd) {
        let _ = self.tx.send(Ev::Cmd(c));
    }

    fn pods_section(&self, p: &Pods) -> Vec<MenuItem<Self>> {
        let mut items: Vec<MenuItem<Self>> = vec![StandardItem {
            label: p.name.clone(),
            icon_name: "audio-headphones".into(),
            ..Default::default()
        }
        .into()];
        let battery = battery_line(p);
        if !battery.is_empty() {
            items.push(StandardItem { label: battery, ..Default::default() }.into());
        }
        if let Some(current) = p.noise {
            items.push(MenuItem::Separator);
            items.push(
                RadioGroup {
                    selected: Noise::ALL.iter().position(|&n| n == current).unwrap_or(0),
                    select: Box::new(|this: &mut Self, i| {
                        if let Some(&n) = Noise::ALL.get(i) {
                            this.send(Cmd::Noise(n));
                        }
                    }),
                    options: Noise::ALL
                        .iter()
                        .map(|&n| RadioItem { label: noise_label(n).into(), ..Default::default() })
                        .collect(),
                }
                .into(),
            );
        }
        items.push(MenuItem::Separator);
        if let Some(ca) = p.ca {
            items.push(
                CheckmarkItem {
                    label: t().conversational.into(),
                    checked: ca,
                    activate: Box::new(move |this: &mut Self| this.send(Cmd::Ca(!ca))),
                    ..Default::default()
                }
                .into(),
            );
        }
        let ear = self.view.ear_detection;
        items.push(
            CheckmarkItem {
                label: t().ear_detection.into(),
                checked: ear,
                activate: Box::new(move |this: &mut Self| this.send(Cmd::EarDetection(!ear))),
                ..Default::default()
            }
            .into(),
        );
        items.push(MenuItem::Separator);
        items
    }

    fn device_item(&self, d: &Device) -> MenuItem<Self> {
        let mut label = display_name(d);
        let pods_battery = self.view.pods.as_ref().filter(|p| p.address == d.address).and_then(|p| p.min_level());
        if let Some(b) = pods_battery.or(d.battery) {
            label.push_str(&format!("  ·  {b}%"));
        }
        let path = d.path.clone();
        let connected = d.connected;
        CheckmarkItem {
            label,
            checked: connected,
            icon_name: device_icon(d),
            activate: Box::new(move |this: &mut Self| {
                this.send(if connected { Cmd::Disconnect(path.clone()) } else { Cmd::Connect(path.clone()) })
            }),
            ..Default::default()
        }
        .into()
    }

    fn add_menu(&self, discovering: bool) -> MenuItem<Self> {
        let mut sub: Vec<MenuItem<Self>> = vec![StandardItem {
            label: if discovering { t().stop_search } else { t().search }.into(),
            icon_name: "edit-find".into(),
            activate: Box::new(move |this: &mut Self| this.send(Cmd::Scan(!discovering))),
            ..Default::default()
        }
        .into()];
        let mut found: Vec<&Device> =
            self.view.snap.devices.iter().filter(|d| !d.paired && !d.name.is_empty() && d.rssi.is_some()).collect();
        found.sort_by_key(|d| std::cmp::Reverse(d.rssi));
        if discovering || !found.is_empty() {
            sub.push(MenuItem::Separator);
        }
        if found.is_empty() && discovering {
            sub.push(StandardItem { label: t().searching.into(), enabled: false, ..Default::default() }.into());
        }
        for d in found.into_iter().take(15) {
            let path = d.path.clone();
            sub.push(
                StandardItem {
                    label: display_name(d),
                    icon_name: device_icon(d),
                    activate: Box::new(move |this: &mut Self| this.send(Cmd::Pair(path.clone()))),
                    ..Default::default()
                }
                .into(),
            );
        }
        SubMenu { label: t().add_device.into(), icon_name: "list-add".into(), submenu: sub, ..Default::default() }
            .into()
    }

    fn remove_menu(&self, paired: &[&Device]) -> MenuItem<Self> {
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
            MenuItem::Separator,
        ];
        if !powered {
            return items;
        }
        if let Some(p) = &self.view.pods {
            items.extend(self.pods_section(p));
        }
        let paired: Vec<&Device> = self.view.snap.devices.iter().filter(|d| d.paired).collect();
        if paired.is_empty() {
            items.push(StandardItem { label: t().no_devices.into(), enabled: false, ..Default::default() }.into());
        }
        for d in &paired {
            items.push(self.device_item(d));
        }
        items.push(MenuItem::Separator);
        items.push(self.add_menu(adapter.discovering));
        items.push(self.remove_menu(&paired));
        items
    }
}
