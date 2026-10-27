from pathlib import Path
import base64

ROOT = Path(__file__).resolve().parent.parent
JS_PATH = ROOT / "pkg" / "internetmap.js"
WASM_PATH = ROOT / "pkg" / "internetmap_bg.wasm"
OUT_PATH = ROOT / "standalone.html"

if not JS_PATH.exists() or not WASM_PATH.exists():
    raise SystemExit("Run `wasm-pack build --target web --release` first.")

js = JS_PATH.read_text()
js = js.replace("</script", "<\\/script")

wasm_b64 = base64.b64encode(WASM_PATH.read_bytes()).decode("ascii")

html = f"""<!doctype html>
<html lang="en">
<head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>internetmap</title>
    <style>
        html, body {{
            margin: 0;
            padding: 0;
            width: 100%;
            height: 100%;
            overflow: hidden;
            background: #111;
        }}
        canvas {{
            display: block;
            width: 100vw;
            height: 100vh;
        }}
    </style>
</head>
<body>
    <canvas id="map"></canvas>

    <!-- wasm bytes are base64 so this file works from file:// with no server -->
    <script id="wasm-b64" type="application/octet-stream">{wasm_b64}</script>

    <script type="module">
{js}

        const encoded = document.getElementById("wasm-b64").textContent.trim();
        const binary = atob(encoded);
        const bytes = new Uint8Array(binary.length);
        for (let i = 0; i < binary.length; i++) {{
            bytes[i] = binary.charCodeAt(i);
        }}

        await __wbg_init({{ module_or_path: bytes }});
        const app = new WebHandle();
        await app.start(document.getElementById("map"));
    </script>
</body>
</html>
"""

OUT_PATH.write_text(html)
print(f"Wrote {OUT_PATH} ({OUT_PATH.stat().st_size / 1024 / 1024:.1f} MiB)")
