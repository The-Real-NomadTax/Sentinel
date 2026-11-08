//! The device dashboard: model, Android version, chipset, battery, storage, memory, display.

use gpui::{
    AnyElement, IntoElement, ParentElement, SharedString, Styled, div, prelude::*, px, relative, rgb,
};
use sentinel_core::info::{BatteryHealth, BatteryStatus, DeviceInfo, PowerSource};

use crate::theme::*;

pub fn render(info: Option<&DeviceInfo>, error: Option<&str>, has_device: bool) -> AnyElement {
    let center = || div().size_full().flex().items_center().justify_center().text_color(rgb(MUTED));
    let Some(info) = info else {
        return match (error, has_device) {
            (Some(err), _) => center()
                .flex_col()
                .gap_2()
                .p_8()
                .child(div().text_color(rgb(ERR)).child("Couldn't read device info"))
                .child(div().text_xs().child(err.to_string()))
                .into_any_element(),
            (None, true) => center().child("Loading device info…").into_any_element(),
            (None, false) => center().child("Connect a device to see its details").into_any_element(),
        };
    };

    let title = format!("{} {}", capitalize(&info.manufacturer), info.model);
    let mut cards: Vec<AnyElement> = Vec::new();

    // Android
    let mut android_detail = Vec::new();
    if let Some(sdk) = info.sdk {
        android_detail.push(format!("API level {sdk}"));
    }
    if let Some(patch) = &info.security_patch {
        android_detail.push(format!("Security patch {patch}"));
    }
    cards.push(card("Android", format!("Android {}", info.android_version), android_detail, None));

    // Chipset
    let chip = &info.chipset;
    let chip_title = chip.name.clone().or_else(|| chip.model.clone()).unwrap_or_else(|| "Unknown".into());
    let mut chip_detail = Vec::new();
    // Always show the raw code next to a friendly name, so nothing is hidden by our lookup table.
    let code = if chip.name.is_some() { chip.model.as_deref() } else { chip.platform.as_deref() };
    let maker_line: Vec<&str> = [chip.manufacturer.as_deref(), code].into_iter().flatten().collect();
    if !maker_line.is_empty() {
        chip_detail.push(maker_line.join(" · "));
    }
    let core_line: Vec<String> = [chip.cpu_cores.map(|c| format!("{c} cores")), chip.cpu_abi.clone()]
        .into_iter()
        .flatten()
        .collect();
    if !core_line.is_empty() {
        chip_detail.push(core_line.join(" · "));
    }
    if !chip.cpu_clusters.is_empty() {
        let clusters: Vec<String> = chip
            .cpu_clusters
            .iter()
            .map(|(count, mhz)| format!("{count}×{:.2}", *mhz as f32 / 1000.0))
            .collect();
        chip_detail.push(format!("{} GHz", clusters.join(" · ")));
    }
    cards.push(card("Chipset", chip_title, chip_detail, None));

    // Battery
    if let Some(b) = &info.battery {
        let status = match (b.status, b.power_source) {
            (BatteryStatus::Charging, Some(src)) => format!("Charging via {}", power_source(src)),
            (BatteryStatus::Charging, None) => "Charging".into(),
            (BatteryStatus::Full, _) => "Full".into(),
            (BatteryStatus::NotCharging, _) => "Plugged in, not charging".into(),
            (BatteryStatus::Discharging, _) => "On battery".into(),
            (BatteryStatus::Unknown, _) => "Status unknown".into(),
        };
        let mut detail = vec![status];
        let mut extras = Vec::new();
        if let Some(t) = b.temperature_c {
            extras.push(format!("{t:.1} °C"));
        }
        if let Some(mv) = b.voltage_mv {
            extras.push(format!("{:.2} V", mv as f32 / 1000.0));
        }
        extras.push(format!("Health: {}", battery_health(b.health)));
        detail.push(extras.join(" · "));
        let color = match b.level {
            0..=15 => ERR,
            16..=30 => WARN,
            _ => OK,
        };
        cards.push(card("Battery", format!("{}%", b.level), detail, Some((b.level as f32 / 100.0, color))));
    }

    // Storage
    if let Some(s) = &info.storage {
        let fraction = ratio(s.used_bytes, s.total_bytes);
        cards.push(card(
            "Storage",
            format!("{} of {}", gb(s.used_bytes), gb(s.total_bytes)),
            vec![format!("{} free", gb(s.available_bytes))],
            Some((fraction, usage_color(fraction))),
        ));
    }

    // RAM
    if let Some(m) = &info.memory {
        let fraction = ratio(m.used_bytes(), m.total_bytes);
        cards.push(card(
            "RAM",
            format!("{} of {}", gb(m.used_bytes()), gb(m.total_bytes)),
            vec![format!("{} available", gb(m.available_bytes))],
            Some((fraction, usage_color(fraction))),
        ));
    }

    // Display
    if let Some(d) = &info.display {
        let detail = d.density_dpi.map(|dpi| vec![format!("{dpi} dpi")]).unwrap_or_default();
        cards.push(card("Display", format!("{} × {}", d.width, d.height), detail, None));
    }

    cards.push(card("Serial", info.serial.clone(), vec![format!("Codename: {}", info.codename)], None));

    div()
        .id("dashboard")
        .size_full()
        .overflow_y_scroll()
        .p_6()
        .flex()
        .flex_col()
        .gap_5()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_2xl().font_weight(gpui::FontWeight::BOLD).child(title))
                .child(div().text_sm().text_color(rgb(MUTED)).child(format!("{} · {}", info.serial, info.codename))),
        )
        .child(div().flex().flex_wrap().gap_4().children(cards))
        .into_any_element()
}

/// A stat card: label, headline value, detail lines, and an optional usage bar (fraction, color).
fn card(label: &'static str, value: String, detail: Vec<String>, bar: Option<(f32, u32)>) -> AnyElement {
    div()
        .w(px(300.))
        .flex()
        .flex_col()
        .gap_2()
        .p_4()
        .rounded_lg()
        .bg(rgb(PANEL))
        .border_1()
        .border_color(rgb(BORDER))
        .child(section_label(label))
        .child(div().text_xl().font_weight(gpui::FontWeight::SEMIBOLD).child(SharedString::from(value)))
        .children(bar.map(|(fraction, color)| {
            div()
                .h(px(6.))
                .w_full()
                .rounded_full()
                .bg(rgb(PANEL_HOVER))
                .child(div().h_full().rounded_full().bg(rgb(color)).w(relative(fraction.clamp(0.0, 1.0))))
        }))
        .children(detail.into_iter().map(|line| div().text_sm().text_color(rgb(MUTED)).child(line)))
        .into_any_element()
}

fn ratio(part: u64, total: u64) -> f32 {
    if total == 0 { 0.0 } else { part as f32 / total as f32 }
}

fn usage_color(fraction: f32) -> u32 {
    if fraction >= 0.9 {
        ERR
    } else if fraction >= 0.75 {
        WARN
    } else {
        ACCENT
    }
}

/// Decimal gigabytes, matching how phones label storage.
fn gb(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1e9)
}

fn power_source(src: PowerSource) -> &'static str {
    match src {
        PowerSource::Ac => "charger",
        PowerSource::Usb => "USB",
        PowerSource::Wireless => "wireless",
        PowerSource::Dock => "dock",
    }
}

fn battery_health(h: BatteryHealth) -> &'static str {
    match h {
        BatteryHealth::Good => "Good",
        BatteryHealth::Overheat => "Overheating",
        BatteryHealth::Dead => "Dead",
        BatteryHealth::OverVoltage => "Over voltage",
        BatteryHealth::Cold => "Cold",
        BatteryHealth::Failure => "Failure",
        BatteryHealth::Unknown => "Unknown",
    }
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}
