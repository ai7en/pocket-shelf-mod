# Fonts

Figtree is built into Pocket Shelf (`core/src/text.rs` includes both files)
and rasterized at run time with `fontdue`. `tools/src/art.rs` draws the LiveArea
pictures with Figtree Bold.

- Figtree (Regular, Bold): SIL Open Font License 1.1, `OFL-Figtree.txt`,
  https://github.com/erikdkennedy/figtree

- Noto Sans Cyrillic (Regular, Bold): the glyphs U+0400-045F and U+2116 of Noto Sans, cut with `pyftsubset` from `hinted/ttf/NotoSans` of https://github.com/notofonts/noto-fonts (`NotoSans-Regular.ttf`, `NotoSans-Bold.ttf`); SIL Open Font License 1.1, `OFL-NotoSans.txt`. Core text draws Cyrillic from these files because Figtree has none.

Keep each font's license file next to it when you add one.
