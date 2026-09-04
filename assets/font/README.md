# Custom Font — NanoChrono

Coloca aquí **un único archivo** `.ttf` o `.otf` y el motor lo cargará automáticamente al iniciar.

## ¿Cómo funciona?
1. Al arrancar, `try_load_custom_font()` busca el primer `*.ttf` / `*.otf` en esta carpeta.
2. Lee el **family name** directamente desde la tabla `name` del binario (nameID=1, plataforma Windows Unicode).
3. Registra la fuente en memoria con `AddFontMemResourceEx` (privado, no persiste en el sistema).
4. Si ese método falla, reintenta con `AddFontResourceExW` (FR_PRIVATE).
5. Llama a `CreateFontW` con ese family name: GDI ya conoce la fuente porque acabamos de registrarla.

## ¿Cuál family name usa GDI?
Si no sabes el nombre exacto que GDI usará, ejecuta el helper:

```
python check_font_name.py assets\font\TuFuente.ttf
```

Verás algo como:
```
Family name (nameID=1, Win Unicode): "JetBrains Mono"
```
Ese es el string que se pasará a CreateFontW.

## Recomendaciones de fuentes para un timer
- **JetBrains Mono** — excelente legibilidad, monoespaciada, descarga gratuita
- **Fira Code** — ligaduras opcionales, muy clara
- **Cascadia Code** — fuente de Windows Terminal, muy legible
- **Orbitron** (Google Fonts) — estética digital/retro, buena para cronómetros

## Notas
- Solo se carga el **primer** archivo encontrado (orden alfabético del sistema de archivos).
- El tamaño de punto se calcula automáticamente para que el texto ocupe el 94% del ancho disponible.
- Si la carpeta está vacía o el archivo no se puede parsear, se usa Consolas → Courier New como fallback.
