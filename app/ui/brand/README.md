# Ember mods' Steam artwork

Each mod's icon and Steam library artwork, in the mod's own colour. All of it comes from Ember's own artwork, which
the SF4 Ember project drew: the mark (火 in an open brush circle) and the ink-landscape menu background. The layout
is SFxT Ember's. The wordmark is Inter SemiBold (OFL). No Capcom artwork or logos are used; the game names appear
only as part of each project's name.

| Game | Accent | Recolour (the one its own menus use) | Wordmark |
|---|---|---|---|
| `usf4` | orange `#FF8738` | none, Ember's own colours | USF**4** |
| `sfxt` | red `#FF5555` | hue turned, lightness kept (identical to SFxT Ember's own files) | SF**x**T |
| `umvc3` | cobalt `#3887FF` | red and blue channels swapped, as UMvC3 Ember's palette | UM**v**C3 |
| `sfv` | gold `#DDB961` | orange to SFV's gold, warm greys to charcoal; SFV Ember's own mark export; square tile | SF**V** |

| File | Size | Steam slot |
|---|---|---|
| `<id>-ember-icon.png` | 256 x 256 | shortcut icon (list view, taskbar, the Deck's left bar) |
| `<id>-ember-wide.png` | 920 x 430 | grid capsule |
| `<id>-ember-portrait.png` | 600 x 900 | library tile |
| `<id>-ember-hero.png` | 1920 x 620 | hero banner, no text |
| `<id>-ember-logo.png` | x 400, transparent | logo over the hero |
| `<id>-ember-mark.png` | the mark alone | guides and the web |
| `<id>-ember.ico` | 16 to 256 | Windows icon |

The launcher also shows the icon and hero on its Library page.

Rebuild with `tools/brand/build-brand.py --ember <SF4 Ember checkout> --sfv-mark <SFV Ember's art/brand/sfv-ember-mark.png>`
(Pillow). `--check` rebuilds in memory and fails when a file here differs. Text uses Pillow's basic layout on every
machine, so the output does not depend on whether Pillow has raqm.
