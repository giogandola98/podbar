//! UI strings: Italian when the locale says so, English otherwise.

pub struct Text {
    pub bluetooth: &'static str,
    pub no_adapter: &'static str,
    pub no_devices: &'static str,
    pub add_device: &'static str,
    pub search: &'static str,
    pub stop_search: &'static str,
    pub searching: &'static str,
    pub remove_device: &'static str,
    pub left: &'static str,
    pub right: &'static str,
    pub case: &'static str,
    pub noise_off: &'static str,
    pub noise_transparency: &'static str,
    pub noise_adaptive: &'static str,
    pub noise_anc: &'static str,
    pub conversational: &'static str,
    pub ear_detection: &'static str,
    pub connect_failed: &'static str,
    pub pair_failed: &'static str,
    pub paired: &'static str,
    pub confirm_code: &'static str,
    pub type_code: &'static str,
    pub low_battery: &'static str,
}

const EN: Text = Text {
    bluetooth: "Bluetooth",
    no_adapter: "No Bluetooth adapter",
    no_devices: "No paired devices",
    add_device: "Add device",
    search: "Search for devices",
    stop_search: "Stop searching",
    searching: "Searching…",
    remove_device: "Remove device",
    left: "L",
    right: "R",
    case: "Case",
    noise_off: "Off",
    noise_transparency: "Transparency",
    noise_adaptive: "Adaptive",
    noise_anc: "Noise cancellation",
    conversational: "Conversational awareness",
    ear_detection: "Ear detection",
    connect_failed: "Could not connect",
    pair_failed: "Pairing failed",
    paired: "Paired",
    confirm_code: "Check that the device shows this code",
    type_code: "Type this code on the device, then Enter",
    low_battery: "Low battery",
};

const IT: Text = Text {
    bluetooth: "Bluetooth",
    no_adapter: "Nessun adattatore Bluetooth",
    no_devices: "Nessun dispositivo associato",
    add_device: "Aggiungi dispositivo",
    search: "Cerca dispositivi",
    stop_search: "Interrompi la ricerca",
    searching: "Ricerca in corso…",
    remove_device: "Rimuovi dispositivo",
    left: "S",
    right: "D",
    case: "Custodia",
    noise_off: "No",
    noise_transparency: "Trasparenza",
    noise_adaptive: "Adattiva",
    noise_anc: "Cancellazione del rumore",
    conversational: "Rilevamento conversazione",
    ear_detection: "Rilevamento orecchio",
    connect_failed: "Connessione non riuscita",
    pair_failed: "Associazione non riuscita",
    paired: "Associato",
    confirm_code: "Controlla che il dispositivo mostri questo codice",
    type_code: "Digita questo codice sul dispositivo, poi Invio",
    low_battery: "Batteria scarica",
};

pub fn t() -> &'static Text {
    static PICK: std::sync::OnceLock<&'static Text> = std::sync::OnceLock::new();
    PICK.get_or_init(|| {
        let lang = ["LC_ALL", "LC_MESSAGES", "LANG"]
            .iter()
            .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
            .unwrap_or_default();
        if lang.starts_with("it") {
            &IT
        } else {
            &EN
        }
    })
}
