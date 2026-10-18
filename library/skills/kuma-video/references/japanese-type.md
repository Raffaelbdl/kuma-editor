# Japanese (and other CJK) type

For any video that shows Chinese, Japanese or Korean text. Japanese is the worked case; the
same holds for the others with their own regional faces and `lang`. Two separate failures:
characters drawn in the **wrong glyph forms**, and, when the video teaches words, kanji
whose **meaning** the viewer cannot get.

## Glyph forms: every character from a Japanese font

A kanji is drawn by the first font in the stack that has it. When none does, Chrome asks
the machine for a fallback and picks it by the page's language. Under `lang="en"` on
Linux that resolves to Noto CJK **SC**: Simplified Chinese forms, subtly wrong to a Japanese
reader. It happened to kanji and kana in labels set in JetBrains Mono (`熊 · kuma`,
`つづく…?`) and in a mini timeline's mono track names (`承¹ 承²`).

So:

1. **Fonts that cover Japanese.** `list_fonts` must report `han` and `kana` for every
   family that sets CJK text (it probes 永 and あ). A subset cut to "the characters I use
   today" fails the probe and breaks on the next edit.
2. **The JP face, and the simplest way to get it.** A Noto CJK `.ttc` holds five regional
   faces over shared outlines, in the order JP, KR, SC, TC, HK. The JP face maps each code
   point to its Japanese form; any other face draws the same characters as Chinese or
   Korean.
   - **Simplest:** `add_font` the whole collection
     (`/usr/share/fonts/opentype/noto/NotoSerifCJK-SemiBold.ttc`). kuma splits it into
     faces and names each by its typographic family, so name the family **"Noto Serif CJK
     JP"** (weight from the face) in `font` vars. It is complete, and has Japanese forms.
   - **To shrink the project**, cut a subset with `scripts/subset_jp_fonts.py`: all of JIS
     X 0208 (6,356 kanji, every jōyō kanji), kana, Latin and the project's text, about
     5–8 MB per face.
3. **Keep the family names a subset had.** A subset that keeps only name IDs 0–6 is named
   by its legacy family ("Noto Serif CJK JP SemiBold"). One that keeps ID 16 is named "Noto
   Serif CJK JP", and would merge SemiBold and Black into one family. Whichever you choose,
   choose it before entries name it.
4. **The page's language is Japanese** wherever Japanese is shown, so any fallback that
   still happens picks Japanese forms: set the entry's `lang` var to `ja` (`list_blueprints`
   shows which blueprints declare one; give your own components one too),
   so one folder serves Latin and Japanese entries alike. A component without one needs
   `<html lang="ja">` on its page. `transcribe` with `language: "ja"` sets it on the
   subtitles it makes. Japanese **inside** a line in another language (a place name in a
   French sentence) is the element's business, not the page's: give it its own element,
   `<span id="name" lang="ja">`, written with `text("name", vars.name)`, and leave the page
   French.
5. **A `jp` fallback font var** in every overlay whose font can be a Latin face (label,
   list, narration, bubble, code, timeline…):
   ```json
   "jp": { "type": "font", "default": "Noto Serif CJK JP SemiBold", "label": "Japanese fallback (kanji, kana)" }
   ```
   ```css
   font-family: var(--var-font), var(--var-jp, serif), serif;
   ```
   The `var(--var-jp, serif)` form keeps the stack valid where an overlay lacks the var.
6. **Check it:** `validate_project` warns about every entry with characters none of its
   `font` vars' families can set (`glyphs_missing`, with the characters), and CJK text that
   lands in an element whose language (its nearest `lang`, else the page's) is not a CJK one
   (`cjk_lang`). `check_overlay` says the same of a folder's defaults.
7. Vertical text: `writing-mode: vertical-rl; text-orientation: upright`. Keep all layout
   features when subsetting (`vert`, `vrt2`).

## Meaning: when the video teaches words, gloss with Japanese, not only with the reading

A kanji shown with only its **on'yomi** (the Sino-Japanese reading) carries no meaning
for a viewer. The viewer who saw "承 · SHŌ · development" did not know what 承 means. The
**kun'yomi**, the native Japanese verb, carries the sense:

| Kanji | On'yomi (its name) | Kun'yomi verb | Meaning |
|---|---|---|---|
| 起 | KI | 起こす okosu | start it |
| 承 | SHŌ | 承ける ukeru | take it up |
| 転 | TEN | 転がす korogasu | turn it over |
| 結 | KETSU | 結ぶ musubu | tie it up |

- Name a concept by its usual reading, and teach it by the verb: "起承転結: start it, take it
  up, turn it over, tie it up."
- On a title card, set the kun'yomi in kana beside the big kanji, like furigana (vertical,
  small, brush), with the romaji and the meaning under it.
- Every kakimoji (onomatopoeia lettering: ビリッ, ドン, ゴゴゴゴ) gets a small gloss:
  "(riiip)", "(DON!)", "(a menacing rumble)".
- Words the viewer needs (掟 okite "the law", 巡 meguru "to go around", 秘密 himitsu "the
  secret") get reading and meaning in one small label beside them.
- Use Japanese forms and words (転, not 转 or 轉), and let someone who reads Japanese check
  the glosses.
