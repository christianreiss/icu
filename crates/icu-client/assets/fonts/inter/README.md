# Inter (embedded)

Inter 4 by the Inter Project Authors, SIL Open Font License 1.1 (`LICENSE.txt`).

**Modified (2026-09-26):** the character-map entries for the Private Use Area
(U+E000–U+F8FF and planes 15/16) are removed from both files; every glyph is
still in the font, only unreachable by codepoint. Reason: the client draws
Phosphor icons, which live in the Private Use Area, and Inter comes first in
the font chain (`src/ui/fonts.rs`), so Inter's stylistic-alternate glyphs
there would replace the icons. The test `inter_maps_nothing_in_the_private_use_area`
in `src/ui/fonts.rs` checks it. Redo after updating the files:

```python
from fontTools.ttLib import TTFont
for name in ["Inter-Regular.ttf", "Inter-SemiBold.ttf"]:
    font = TTFont(name)
    for table in font["cmap"].tables:
        table.cmap = {c: g for c, g in table.cmap.items()
                      if not (0xE000 <= c <= 0xF8FF or c >= 0xF0000)}
    font.save(name)
```
