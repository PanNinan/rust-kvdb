//! HTML Dashboard — rendered server-side and served as a static page.

use crate::engine::MetricsSnapshot;

/// Render the dashboard HTML with current metrics.
pub fn render(uptime_secs: u64, m: &MetricsSnapshot) -> String {
    let days = uptime_secs / 86400;
    let hours = (uptime_secs % 86400) / 3600;
    let mins = (uptime_secs % 3600) / 60;
    let secs = uptime_secs % 60;
    let uptime_str = if days > 0 {
        format!("{}d {}h {}m {}s", days, hours, mins, secs)
    } else if hours > 0 {
        format!("{}h {}m {}s", hours, mins, secs)
    } else {
        format!("{}m {}s", mins, secs)
    };

    format!(r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>rust-kvdb · Dashboard</title>
<style>
  :root {{
    --bg: #0f172a; --surface: #1e293b; --border: #334155;
    --text: #e2e8f0; --muted: #94a3b8; --accent: #38bdf8;
    --green: #22c55e; --red: #f87171; --yellow: #fbbf24;
    --card: #1e293b;
  }}
  * {{ margin:0; padding:0; box-sizing:border-box; }}
  body {{ font-family: 'Segoe UI', system-ui, -apple-system, sans-serif;
         background: var(--bg); color: var(--text); min-height:100vh; }}
  .container {{ max-width:960px; margin:0 auto; padding:24px 16px; }}

  /* Header */
  header {{ display:flex; align-items:center; justify-content:space-between;
            padding:20px 0; border-bottom:1px solid var(--border); margin-bottom:24px; }}
  .logo {{ font-size:20px; font-weight:700; color:var(--accent); }}
  .logo span {{ color:var(--muted); font-weight:400; font-size:14px; margin-left:8px; }}
  .badge {{ display:inline-block; padding:3px 10px; border-radius:12px;
            font-size:12px; font-weight:600; }}
  .badge-ok {{ background:rgba(34,197,94,0.15); color:var(--green); }}

  /* Info bar */
  .info-bar {{ display:flex; gap:24px; flex-wrap:wrap; margin-bottom:24px;
               font-size:13px; color:var(--muted); }}
  .info-bar b {{ color:var(--text); font-weight:600; }}

  /* Metric cards */
  .metrics {{ display:grid; grid-template-columns:repeat(auto-fit,minmax(140px,1fr));
              gap:12px; margin-bottom:24px; }}
  .card {{ background:var(--card); border:1px solid var(--border);
           border-radius:10px; padding:16px; text-align:center; }}
  .card .label {{ font-size:12px; color:var(--muted); text-transform:uppercase;
                  letter-spacing:0.5px; margin-bottom:6px; }}
  .card .value {{ font-size:28px; font-weight:700; font-variant-numeric:tabular-nums; }}
  .card.writes .value {{ color:var(--green); }}
  .card.reads .value  {{ color:var(--accent); }}
  .card.deletes .value {{ color:var(--red); }}
  .card.compactions .value {{ color:var(--yellow); }}
  .card.flushes .value {{ color:#a78bfa; }}

  /* KV Operations */
  .ops {{ background:var(--card); border:1px solid var(--border);
          border-radius:10px; padding:20px; margin-bottom:24px; }}
  .ops h3 {{ font-size:15px; margin-bottom:16px; color:var(--accent); }}
  .op-row {{ display:flex; gap:8px; margin-bottom:10px; align-items:center; }}
  .op-row input {{ flex:1; padding:8px 12px; border-radius:6px;
                   border:1px solid var(--border); background:var(--bg);
                   color:var(--text); font-size:13px; outline:none; }}
  .op-row input:focus {{ border-color:var(--accent); }}
  .op-row button {{ padding:8px 16px; border:none; border-radius:6px;
                    font-size:13px; font-weight:600; cursor:pointer; }}
  .btn-get    {{ background:#1d4ed8; color:#fff; }}
  .btn-put    {{ background:#15803d; color:#fff; }}
  .btn-delete {{ background:#b91c1c; color:#fff; }}
  .btn-compact{{ background:#7c3aed; color:#fff; }}
  .result {{ margin-top:12px; padding:10px; border-radius:6px;
             font-size:13px; font-family:monospace; word-break:break-all;
             display:none; }}
  .result.show {{ display:block; }}
  .result.ok  {{ background:rgba(34,197,94,0.1); border:1px solid rgba(34,197,94,0.3); }}
  .result.err {{ background:rgba(248,113,113,0.1); border:1px solid rgba(248,113,113,0.3); }}

  /* Footer */
  footer {{ text-align:center; font-size:12px; color:var(--muted);
            padding:20px 0; border-top:1px solid var(--border); margin-top:24px; }}
  footer a {{ color:var(--accent); text-decoration:none; }}
</style>
</head>
<body>
<div class="container">
  <header>
    <div class="logo">⚡ rust-kvdb <span>v0.1.0</span></div>
    <div><span class="badge badge-ok">● Running</span></div>
  </header>

  <div class="info-bar">
    <div>⏱ Uptime: <b id="uptime">{uptime}</b></div>
    <div>🕐 Last refresh: <b id="last-refresh">-</b></div>
  </div>

  <div class="metrics">
    <div class="card writes">
      <div class="label">Writes</div>
      <div class="value" id="m-writes">{writes}</div>
    </div>
    <div class="card reads">
      <div class="label">Reads</div>
      <div class="value" id="m-reads">{reads}</div>
    </div>
    <div class="card deletes">
      <div class="label">Deletes</div>
      <div class="value" id="m-deletes">{deletes}</div>
    </div>
    <div class="card compactions">
      <div class="label">Compactions</div>
      <div class="value" id="m-compactions">{compactions}</div>
    </div>
    <div class="card flushes">
      <div class="label">Flushes</div>
      <div class="value" id="m-flushes">{flushes}</div>
    </div>
  </div>

  <div class="ops">
    <h3>🔑 Key-Value Operations</h3>

    <div class="op-row">
      <input id="get-key" placeholder="Key" />
      <button class="btn-get" onclick="doGet()">GET</button>
    </div>

    <div class="op-row">
      <input id="put-key" placeholder="Key" />
      <input id="put-val" placeholder="Value" />
      <button class="btn-put" onclick="doPut()">PUT</button>
    </div>

    <div class="op-row">
      <input id="del-key" placeholder="Key" />
      <button class="btn-delete" onclick="doDelete()">DELETE</button>
      <button class="btn-compact" onclick="doCompact()">COMPACT</button>
    </div>

    <div id="result" class="result"></div>
  </div>

  <footer>
    Built with 🦀 Rust &nbsp;·&nbsp;
    <a href="https://github.com/PanNinan/rust-kvdb" target="_blank">GitHub</a>
  </footer>
</div>

<script>
const show = (cls, msg) => {{
  const r = document.getElementById('result');
  r.className = 'result show ' + cls;
  r.textContent = msg;
}};

async function doGet() {{
  const key = document.getElementById('get-key').value;
  if (!key) return show('err', 'Please enter a key');
  try {{
    const r = await fetch('/api/get/' + encodeURIComponent(key));
    const d = await r.json();
    if (d.found) {{
      show('ok', `[${{d.key}}] = ${{d.value}}\nhex: ${{d.value_hex}}`);
    }} else {{
      show('err', `[${{d.key}}] not found`);
    }}
  }} catch(e) {{ show('err', 'Error: ' + e); }}
}}

async function doPut() {{
  const key = document.getElementById('put-key').value;
  const val = document.getElementById('put-val').value;
  if (!key) return show('err', 'Please enter a key');
  try {{
    const r = await fetch('/api/put', {{
      method:'POST',
      headers:{{'Content-Type':'application/json'}},
      body: JSON.stringify({{key, value: val || ''}})
    }});
    const d = await r.json();
    show(d.success ? 'ok' : 'err', d.message);
  }} catch(e) {{ show('err', 'Error: ' + e); }}
}}

async function doDelete() {{
  const key = document.getElementById('del-key').value;
  if (!key) return show('err', 'Please enter a key');
  try {{
    const r = await fetch('/api/delete/' + encodeURIComponent(key), {{method:'POST'}});
    const d = await r.json();
    show(d.success ? 'ok' : 'err', d.message);
  }} catch(e) {{ show('err', 'Error: ' + e); }}
}}

async function doCompact() {{
  try {{
    const r = await fetch('/api/compact', {{method:'POST'}});
    const d = await r.json();
    show('ok', d.message);
  }} catch(e) {{ show('err', 'Error: ' + e); }}
}}

async function refresh() {{
  try {{
    const r = await fetch('/api/metrics');
    const m = await r.json();
    document.getElementById('m-writes').textContent = m.writes.toLocaleString();
    document.getElementById('m-reads').textContent = m.reads.toLocaleString();
    document.getElementById('m-deletes').textContent = m.deletes.toLocaleString();
    document.getElementById('m-compactions').textContent = m.compactions.toLocaleString();
    document.getElementById('m-flushes').textContent = m.flushes.toLocaleString();
    document.getElementById('last-refresh').textContent = new Date().toLocaleTimeString();
  }} catch(e) {{}}
}}

setInterval(refresh, 2000);
refresh();
</script>
</body>
</html>"##,
        uptime = uptime_str,
        writes = m.writes,
        reads = m.reads,
        deletes = m.deletes,
        compactions = m.compactions,
        flushes = m.flushes,
    )
}
