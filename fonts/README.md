# Font Setup for Spectral

Spectral uses **Berkeley Mono** as its primary font. This is a licensed font that
cannot be distributed with the source code.

## Setting Up the Font

If you have a license for Berkeley Mono:

1. Place your font files in this directory:
   ```
   fonts/BerkeleyMono-Regular.ttf  (or .otf)
   fonts/BerkeleyMono-Bold.ttf     (or .otf)
   ```

2. The build system will automatically detect and embed the fonts into the binary.

3. Build the application:
   ```bash
   nix build
   # or
   cargo build --release
   ```

## Supported Font Formats

- `.otf` (OpenType) - preferred
- `.ttf` (TrueType)
- `.ttc` (TrueType Collection)

## Without the Licensed Font

If you don't have Berkeley Mono, the application will still build and run,
but you'll see a build warning and the application will fall back to the
system's default monospace font.

To use a different font:

1. Edit `ui/app-window.slint` and change the import statement to point to your font
2. Update the `default-font-family` property to match your font's family name

## Why This Approach?

This setup ensures:

- **License compliance**: The font is never committed to version control
- **Binary embedding**: Once built, the font is embedded in the binary and
  travels with it - users don't need the font installed
- **No extraction**: The font data is compiled into the binary and cannot be
  easily extracted for unauthorized use
- **Graceful fallback**: The application works without the font, just with
  different typography

## Purchasing Berkeley Mono

Berkeley Mono is available from: https://berkeleygraphics.com/typefaces/berkeley-mono/
