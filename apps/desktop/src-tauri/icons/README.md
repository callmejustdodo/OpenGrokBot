# App icons

OpenGrokBot's mark is its agent team, as in the demo and the README header: four of the agent avatars (rocket, star, magnifier, cat, in the header's colors) on the v2 "Sky Glass" backdrop. The shapes are the open-bot-avatar ones the app already draws (`src/components/sidebar/avatarShapes.ts`, MIT; see `THIRD_PARTY_NOTICES.md`).

- `source/generate.mjs` writes both SVG sources from those shapes. Edit it, not the SVGs.
- `source/app-icon.svg`: the app icon, on the macOS icon grid (an 824 px rounded square in a 1024 px canvas). Every other image here except `tray-icon.png` is rendered from it.
- `source/tray-icon.svg`: the menu bar glyph, the rocket's body in black with its eyes cut out, for a template image. `src/shell.rs` embeds `tray-icon.png` and adds the attention dot to it.

Regenerate after a change, from `apps/desktop`:

```sh
node src-tauri/icons/source/generate.mjs
./node_modules/.bin/tauri icon src-tauri/icons/source/app-icon.svg -o <empty-folder>
# copy the files that already exist in src-tauri/icons from <empty-folder>; the android and ios folders are not used
./node_modules/.bin/tauri icon src-tauri/icons/source/tray-icon.svg -o <empty-folder> -p 44
# copy <empty-folder>/44x44.png to src-tauri/icons/tray-icon.png
```
