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

    // Inter goes *first*, ahead of phosphor. An earlier version of this
    // file put phosphor first on the (false) assumption that it only
    // defines glyphs in the Private Use Area; in fact egui-phosphor's font
    // is built on a template that also maps ordinary `a`-`z`, digits `0`/`1`,
    // space and hyphen to blank glyphs (the pieces Phosphor's own ligatures
    // are built from — this is documented in the crate's own
    // `add_font_bytes_as_family`). With phosphor first, every one of those
    // codepoints resolved to an invisible blank glyph instead of falling
    // through to Inter, so almost every word lost most of its letters
    // (`"Sprechen"` rendered as just `"S"`). Putting Inter first fixes that,
    // at the cost of the *opposite*, much narrower problem this file used
    // to work around: Inter's own PUA-range glyphs (stylistic-alternate
    // slots) collide with some Phosphor icon codepoints and win the lookup
    // instead of falling through, rendering as the wrong Latin letter
    // instead of the icon (observed: the gear icon rendered as "ÿ"). Of the
    // handful of icons this app actually uses, three collided
    // (`icon::GEAR`, `icon::INFO`, `icon::KEYBOARD`); every call site was
    // moved to a non-colliding equivalent (`GEAR_FINE`, `QUESTION`,
    // `COMMAND`) rather than solving it here, since egui has no per-codepoint
    // exclusion for a registered font — verified against Inter's cmap via
    // `fontTools` that no other icon used in this crate collides.
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

    /// The handful of Phosphor icons this crate actually uses must not
    /// collide with one of Inter's own PUA-range stylistic-alternate glyphs
    /// (the *other* direction of the same family-ordering tradeoff — see
    /// `install`'s doc comment): with Inter now first, a collision would
    /// make that icon render as the wrong Latin letter instead of falling
    /// through to Phosphor. Checked against Inter alone (no phosphor, no
    /// egui default fallbacks) so a `true` here can only come from Inter's
    /// own cmap.
    #[test]
    fn used_icons_do_not_collide_with_inter() {
        let mut fonts = egui::FontDefinitions::empty();
        fonts.font_data.insert(
            "Inter".into(),
            egui::FontData::from_static(INTER_REGULAR).into(),
        );
        fonts.font_data.insert(
            "Inter-SemiBold".into(),
            egui::FontData::from_static(INTER_SEMIBOLD).into(),
        );
        fonts
            .families
            .insert(egui::FontFamily::Proportional, vec!["Inter".into()]);
        fonts.families.insert(
            egui::FontFamily::Name("Inter-SemiBold".into()),
            vec!["Inter-SemiBold".into()],
        );
        let ctx = egui::Context::default();
        ctx.set_fonts(fonts);
        ctx.begin_pass(egui::RawInput::default()); // materializes the fonts

        for icon in [
            egui_phosphor::regular::ARROWS_CLOCKWISE,
            egui_phosphor::regular::BUG,
            egui_phosphor::regular::CERTIFICATE,
            egui_phosphor::regular::CHECK,
            egui_phosphor::regular::COPY,
            egui_phosphor::regular::EXPORT,
            egui_phosphor::regular::GAUGE,
            egui_phosphor::regular::GEAR_FINE,
            egui_phosphor::regular::HAND_TAP,
            egui_phosphor::regular::QUESTION,
            egui_phosphor::regular::COMMAND,
            egui_phosphor::regular::MICROPHONE,
            egui_phosphor::regular::MICROPHONE_SLASH,
            egui_phosphor::regular::LOCK_KEY,
            egui_phosphor::regular::PENCIL_SIMPLE,
            egui_phosphor::regular::PLUGS,
            egui_phosphor::regular::PLUGS_CONNECTED,
            egui_phosphor::regular::PROHIBIT,
            egui_phosphor::regular::SHIELD_STAR,
            egui_phosphor::regular::SIGN_IN,
            egui_phosphor::regular::SLIDERS,
            egui_phosphor::regular::SPEAKER_HIGH,
            egui_phosphor::regular::SPEAKER_SLASH,
            egui_phosphor::regular::SQUARES_FOUR,
            egui_phosphor::regular::TICKET,
            egui_phosphor::regular::TRASH,
            egui_phosphor::regular::USERS,
            egui_phosphor::regular::USERS_THREE,
            egui_phosphor::regular::VIDEO_CAMERA,
            egui_phosphor::regular::WARNING,
            egui_phosphor::regular::WARNING_CIRCLE,
            egui_phosphor::regular::WAVEFORM,
            egui_phosphor::regular::X,
        ] {
            let c = icon.chars().next().expect("icon constants are one char");
            for family in [
                egui::FontFamily::Proportional,
                egui::FontFamily::Name("Inter-SemiBold".into()),
            ] {
                let font_id = egui::FontId::new(16.0, family.clone());
                let collides = ctx.fonts_mut(|f| f.has_glyph(&font_id, c));
                assert!(
                    !collides,
                    "icon {c:?} ({:#06x}) collides with Inter in family {family:?} — \
                     pick a different egui_phosphor::regular constant for this spot",
                    c as u32
                );
            }
        }
    }
}
