# Custom Font — NanoChrono

Place **a single file** with the `.ttf` or `.otf` extension here, and the engine will load it automatically at startup.

## How does it work?
1. At startup, `try_load_custom_font()` searches for the first `*.ttf` / `*.otf` file in this folder.
2. It reads the **family name** directly from the binary’s `name` table (nameID=1, Windows Unicode platform).
3. It registers the font in memory using `AddFontMemResourceEx` (private; does not persist on the system).
4. If that method fails, it retries with `AddFontResourceExW` (FR_PRIVATE).
5. Call `CreateFontW` with that family name: GDI already knows the font because we just registered it.

## Which family name does GDI use?
If you don’t know the exact name that GDI will use, run the helper:

```
python check_font_name.py assets\font\YourFont.ttf
```

You’ll see something like:
```
Family name (nameID=1, Win Unicode): “JetBrains Mono”
```
That’s the string to pass to `CreateFontW`.

## Font recommendations for a timer
- **JetBrains Mono** — excellent readability, monospaced, free to download
- **Fira Code** — optional ligatures, very clear
- **Cascadia Code** — Windows Terminal font, very legible
- **Orbitron** (Google Fonts) — digital/retro aesthetic, good for stopwatches

## Notes
- Only the **first** file found is loaded (alphabetical order in the file system).
- The point size is calculated automatically so that the text occupies 94% of the available width.
- If the folder is empty or the file cannot be parsed, Consolas → Courier New is used as a fallback.

