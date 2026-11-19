//! Desktop notifications (org.freedesktop.Notifications).

use std::collections::HashMap;

use zbus::zvariant::Value;
use zbus::Connection;

pub async fn send(conn: &Connection, icon: &str, summary: &str, body: &str) {
    let hints: HashMap<&str, Value> = HashMap::new();
    let r = conn
        .call_method(
            Some("org.freedesktop.Notifications"),
            "/org/freedesktop/Notifications",
            Some("org.freedesktop.Notifications"),
            "Notify",
            &("podbar", 0u32, icon, summary, body, Vec::<&str>::new(), hints, 6000i32),
        )
        .await;
    if let Err(e) = r {
        eprintln!("podbar: notification failed: {e}");
    }
}
