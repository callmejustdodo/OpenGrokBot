# App icons

OpenGrokBot's mark, taken from the Figma file "Formation — Mockup M3" (v2 "Sky Glass"): the main agent's glossy blue orb (node 60:146) carrying the menu bar mark, three connected nodes (node 29:22), on the Sky Glass backdrop. The menu bar glyph is that mark alone.

- `source/app-icon.svg`: the app icon, on the macOS icon grid (an 824 px rounded square in a 1024 px canvas). Every other image here except `tray-icon.png` is generated from it.
- `source/tray-icon.svg`: the menu bar glyph, black on clear, drawn as a template image. `src/shell.rs` embeds `tray-icon.png` and adds the attention dot to it.

Regenerate after editing a source file, from `apps/desktop`:

```sh
./node_modules/.bin/tauri icon src-tauri/icons/source/app-icon.svg -o <empty-folder>
# copy the files that already exist in src-tauri/icons from <empty-folder>; the android and ios folders are not used
./node_modules/.bin/tauri icon src-tauri/icons/source/tray-icon.svg -o <empty-folder> -p 44
# copy <empty-folder>/44x44.png to src-tauri/icons/tray-icon.png
```
