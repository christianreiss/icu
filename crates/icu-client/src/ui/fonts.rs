//! Embeds Inter (OFL-1.1, `assets/fonts/inter/LICENSE.txt`) and the Phosphor
//! icon font, and wires them into egui's font atlas. Called once per window
//! at creation; never from [`super::theme::apply`], which runs on every
//! settings change and must not rebuild the atlas.

const INTER_REGULAR: &[u8] = include_bytes!("../../assets/fonts/inter/Inter-Regular.ttf");
/// Also rasterizes the names in the overlay (`crate::overlay`).
pub(crate) const INTER_SEMIBOLD: &[u8] =
    include_bytes!("../../assets/fonts/inter/Inter-SemiBold.ttf");

/// Font family used for headings and button labels (semibold weight).
pub fn semibold() -> egui::FontFamily {
    egui::FontFamily::Name("Inter-SemiBold".into())
}

/// Installs Inter + Phosphor into `ctx`'s font atlas.
pub fn install(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();

    fonts.font_data.insert(
        "Inter".into(),
        egui::FontData::from_static(INTER_REGULAR).into(),
    );
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

    // Inter goes *first*, ahead of phosphor: egui-phosphor's font maps
    // ordinary `a`-`z`, digits `0`/`1`, space and hyphen to blank glyphs (the
    // pieces of its ligatures, documented in the crate's own
    // `add_font_bytes_as_family`), so with phosphor first almost every word
    // lost most of its letters. The other direction — Inter's own glyphs in
    // the Private Use Area, where Phosphor's icons live, winning over the
    // icon — is solved in the embedded font files: their PUA mappings are
    // removed (`assets/fonts/inter/README.md`), and a test keeps it so.
    let mut proportional = vec!["Inter".to_string(), "phosphor".to_string()];
    proportional.extend(defaults.clone());
    fonts
        .families
        .insert(egui::FontFamily::Proportional, proportional);

    let mut semibold = vec!["Inter-SemiBold".to_string(), "phosphor".to_string()];
    semibold.extend(defaults);
    fonts
        .families
        .insert(egui::FontFamily::Name("Inter-SemiBold".into()), semibold);

    // Monospace (Hack) is untouched: settings.rs's ERL/ERLE stats and
    // main_window.rs's debug stats text depend on it staying put. It isn't
    // affected by the Inter/phosphor collision above since it never mixes
    // phosphor into its family chain.

    ctx.set_fonts(fonts);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression test for the bug this file's `install` doc comment
    /// describes: egui-phosphor's font maps ordinary `a`-`z` (among other
    /// codepoints) to zero-advance-width glyphs — the blank pieces its own
    /// ligatures are built from — so if "phosphor" ever ends up ahead of
    /// "Inter" in the `Proportional`/semibold family chains again, every
    /// word of body text collapses to just its first letter (verified via
    /// `fontTools`: Phosphor's `d`/`e`/`i`/`n` glyphs all have `hmtx`
    /// advance `0`, Inter's are all `>1200` font units at 2048 upm).
    #[test]
    fn ordinary_lowercase_letters_are_not_blanked_by_phosphor() {
        let ctx = egui::Context::default();
        install(&ctx);
        ctx.begin_pass(egui::RawInput::default()); // materializes the fonts
        let font_id = egui::FontId::new(13.5, egui::FontFamily::Proportional);
        for c in "dein name beitreten sprechen".chars() {
            if c == ' ' {
                continue;
            }
            let width = ctx.fonts_mut(|f| f.glyph_width(&font_id, c));
            assert!(
                width > 1.0,
                "'{c}' has near-zero advance width ({width}) — phosphor is \
                 shadowing ordinary text again"
            );
        }
    }

    /// The Verbindung window sets throughput and per-mille readouts in
    /// typographic characters rather than words (`↑ 74 · ↓ 148 kbit/s`).
    /// Those are ordinary text, not icons, so they have to come out of the
    /// composed font chain — a tofu box is as broken as a blanked letter
    /// above, and far easier to miss in review.
    ///
    /// Only the proportional family is asserted here: `Fonts::has_glyph`
    /// answers `false` for every character in `FontFamily::Monospace`,
    /// including plain `A`, so it cannot test that side. Hack — egui's
    /// bundled monospace — was checked against its `cmap` directly and does
    /// carry all six, which is what lets `widgets::chip` (monospace) hold
    /// them too.
    #[test]
    fn typographic_characters_render() {
        let ctx = egui::Context::default();
        install(&ctx);
        ctx.begin_pass(egui::RawInput::default()); // materializes the fonts
        let proportional = egui::FontId::new(13.0, egui::FontFamily::Proportional);
        for c in ['↑', '↓', '·', '‰', '×', '—'] {
            assert!(
                ctx.fonts_mut(|f| f.has_glyph(&proportional, c)),
                "{c:?} has no proportional glyph — readouts using it show tofu"
            );
        }
    }

    /// Phosphor's icons live in the Private Use Area, where Inter also
    /// maps hundreds of stylistic-alternate glyphs. With Inter first in the
    /// chain (see `install`), any such mapping would win and draw a wrong
    /// letter instead of the icon (seen: the gear as „ÿ“, the back arrow as
    /// „ʅ“). The embedded copies have those mappings removed; this keeps it
    /// so for every icon, used today or later.
    #[test]
    fn inter_maps_nothing_in_the_private_use_area() {
        for (name, bytes) in [
            ("Inter-Regular", INTER_REGULAR),
            ("Inter-SemiBold", INTER_SEMIBOLD),
        ] {
            let face = ttf_parser::Face::parse(bytes, 0).expect("embedded font parses");
            let mapped: Vec<u32> = (0xE000u32..=0xF8FF)
                .filter(|&c| char::from_u32(c).is_some_and(|ch| face.glyph_index(ch).is_some()))
                .collect();
            assert!(
                mapped.is_empty(),
                "{name} maps {} PUA codepoints (first {:#06x}); strip them as \
                 assets/fonts/inter/README.md describes",
                mapped.len(),
                mapped[0]
            );
        }
    }
}
