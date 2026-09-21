//! Embeds Inter (OFL-1.1, `assets/fonts/inter/LICENSE.txt`) and the Phosphor
//! icon font, and wires them into egui's font atlas. Called once per window
//! at creation; never from [`super::theme::apply`], which runs on every
//! settings change and must not rebuild the atlas.

const INTER_REGULAR: &[u8] = include_bytes!("../../assets/fonts/inter/Inter-Regular.ttf");
const INTER_SEMIBOLD: &[u8] = include_bytes!("../../assets/fonts/inter/Inter-SemiBold.ttf");

/// Font family used for headings and button labels (semibold weight).
pub fn semibold() -> egui::FontFamily {
    egui::FontFamily::Name("Inter-SemiBold".into())
}

/// Installs Inter + Phosphor into `ctx`'s font atlas.
pub fn install(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();

    fonts
        .font_data
        .insert("Inter".into(), egui::FontData::from_static(INTER_REGULAR).into());
    fonts.font_data.insert(
        "Inter-SemiBold".into(),
        egui::FontData::from_static(INTER_SEMIBOLD).into(),
    );

    // Registers "phosphor" font data + a `Name("phosphor")` family; the
    // Proportional/semibold chains are rebuilt explicitly below instead of
    // relying on where this appends it.
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);

    // egui's bundled fallbacks (Ubuntu-Light, NotoEmoji-Regular,
    // emoji-icon-font), with "phosphor" filtered back out so it can be
    // placed explicitly.
    let defaults: Vec<String> = fonts.families[&egui::FontFamily::Proportional]
        .iter()
        .filter(|n| n.as_str() != "phosphor")
        .cloned()
        .collect();

    // Phosphor goes *first*, ahead of Inter. Phosphor only defines glyphs
    // in the Private Use Area, so this never shadows ordinary text — but
    // the reverse order does: Inter's own PUA-range glyphs (used
    // internally for stylistic alternates) collide with several Phosphor
    // icon codepoints and win the lookup instead of falling through,
    // rendering as the wrong Latin letter instead of the icon (observed:
    // the gear icon rendered as "ÿ" when Inter came first).
    let mut proportional = vec!["phosphor".to_string(), "Inter".to_string()];
    proportional.extend(defaults.clone());
    fonts
        .families
        .insert(egui::FontFamily::Proportional, proportional);

    let mut semibold = vec!["phosphor".to_string(), "Inter-SemiBold".to_string()];
    semibold.extend(defaults);
    fonts
        .families
        .insert(egui::FontFamily::Name("Inter-SemiBold".into()), semibold);

    // Monospace (Hack) is untouched: settings.rs's ERL/ERLE stats and
    // main_window.rs's debug stats text depend on it staying put.

    ctx.set_fonts(fonts);
}
