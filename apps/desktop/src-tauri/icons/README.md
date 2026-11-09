# App icons

OpenGrokBot's own mark, drawn for the v2 "Sky Glass" design (the blue `--brand-grad` gradient in `src/styles/tokens.css`, the agent avatars' two pill eyes): a white chat bubble with an agent's eyes, in front of a frosted bubble for the teammate on the other side.

- `source/app-icon.svg`: the app icon, on the macOS icon grid (an 824 px rounded square in a 1024 px canvas). Every other image here except `tray-icon.png` is generated from it.
- `source/tray-icon.svg`: the menu bar glyph, black on clear, drawn as a template image. `src/shell.rs` embeds `tray-icon.png` and adds the attention dot to it.

Regenerate after editing a source file, from `apps/desktop`:

```sh
./node_modules/.bin/tauri icon src-tauri/icons/source/app-icon.svg -o <empty-folder>
# copy the files that already exist in src-tauri/icons from <empty-folder>; the android and ios folders are not used
./node_modules/.bin/tauri icon src-tauri/icons/source/tray-icon.svg -o <empty-folder> -p 44
# copy <empty-folder>/44x44.png to src-tauri/icons/tray-icon.png
```
