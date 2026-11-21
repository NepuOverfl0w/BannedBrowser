# Test fonts

The snapshot tests render with **only** these fonts (`bb_text::FontSystem::use_only_fonts_from`),
so screenshots come out the same on Linux, Windows and macOS. They are not used by the browser
itself, which uses the system's fonts.

All are licensed under the SIL Open Font License 1.1 ([`OFL.txt`](OFL.txt)).

| Files | Font, version | Copyright and Reserved Font Names | Changes |
|---|---|---|---|
| `Liberation{Serif,Sans,Mono}-{Regular,Bold,Italic,BoldItalic}.ttf` | Liberation 2.1.5 | Digitized data copyright (c) 2010 Google Corporation with Reserved Font Arimo, Tinos and Cousine. Copyright (c) 2012 Red Hat, Inc. with Reserved Font Name Liberation. | none (unmodified) |
| `Inter-{Regular,Bold,Italic,BoldItalic}.otf` | Inter 4.000 | Copyright 2016 The Inter Project Authors (https://github.com/rsms/inter) | none (unmodified) |
| `NotoNaskhArabic-{Regular,Bold}.ttf` | Noto Naskh Arabic 2.004 | Copyright 2019-2020 Google LLC. | none (unmodified) |
| `NotoSansSC-Regular-subset.otf` | Noto Sans SC 2.004 | © 2014-2021 Adobe (http://www.adobe.com/), with Reserved Font Name 'Source'. | subset to the CJK characters of the test pages (`scripts/make-test-font-subsets.sh`) |
| `NotoSans-Regular-subset.ttf` | Noto Sans 2.015 | Copyright 2022 The Noto Project Authors (https://github.com/notofonts/latin-greek-cyrillic) | subset to U+FFFD (`scripts/make-test-font-subsets.sh`) |

The generic families resolve to Liberation Serif / Sans / Mono (serif, sans-serif, monospace) and
Inter (system-ui), the same fonts the approved snapshots were made with.
