//! Tiny self-contained control dashboard (no web framework).
//!
//! Routes:
//!   GET  /              -> HTML control panel
//!   GET  /api/status    -> JSON status + current live settings
//!   POST /api/control   -> urlencoded: freq_mhz, gain_db, ppm, injection_tg
//!
//! Bound to localhost by default. This keeps all DMR-specific UI out of the
//! public FlowStation dashboard (clean git boundary).

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

use crate::config::{LiveSettings, SharedConfig};
use crate::status::{SharedLog, SharedStatus};

pub fn run(bind: &str, cfg: SharedConfig, status: SharedStatus, log: SharedLog) -> std::io::Result<()> {
    let listener = TcpListener::bind(bind)?;
    tracing::info!("flowdmr-sidecar: mini-dashboard on http://{bind}");
    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let cfg = cfg.clone();
                let status = status.clone();
                let log = log.clone();
                std::thread::spawn(move || {
                    if let Err(e) = handle(s, &cfg, &status, &log) {
                        tracing::trace!("flowdmr-sidecar: dashboard conn error: {e}");
                    }
                });
            }
            Err(e) => tracing::warn!("flowdmr-sidecar: accept error: {e}"),
        }
    }
    Ok(())
}

fn handle(mut stream: TcpStream, cfg: &SharedConfig, status: &SharedStatus, log: &SharedLog) -> std::io::Result<()> {
    let mut buf = [0u8; 8192];
    let n = stream.read(&mut buf)?;
    let req = String::from_utf8_lossy(&buf[..n]);
    let mut lines = req.lines();
    let start_line = lines.next().unwrap_or("");
    let mut parts = start_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("/");

    match (method, path) {
        ("GET", "/") => respond(&mut stream, "200 OK", "text/html; charset=utf-8", PAGE),
        ("GET", "/api/status") => {
            let body = status_json(cfg, status);
            respond(&mut stream, "200 OK", "application/json", &body)
        }
        ("GET", "/api/log") => respond(&mut stream, "200 OK", "text/plain; charset=utf-8", &log.tail(120)),
        ("POST", "/api/control") => {
            let body = req.split("\r\n\r\n").nth(1).unwrap_or("");
            apply_control(cfg, body);
            let resp = status_json(cfg, status);
            respond(&mut stream, "200 OK", "application/json", &resp)
        }
        _ => respond(&mut stream, "404 Not Found", "text/plain", "not found"),
    }
}

fn respond(stream: &mut TcpStream, code: &str, content_type: &str, body: &str) -> std::io::Result<()> {
    let resp = format!(
        "HTTP/1.1 {code}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(resp.as_bytes())
}

fn apply_control(cfg: &SharedConfig, body: &str) {
    let mut live = cfg.live();
    for pair in body.split('&') {
        let Some((k, v)) = pair.split_once('=') else { continue };
        let v = v.trim();
        match k {
            "freq_mhz" => {
                if let Ok(mhz) = v.parse::<f64>() {
                    live.rx_freq_hz = (mhz * 1_000_000.0).round() as u64;
                }
            }
            "gain_db" => {
                if let Ok(g) = v.parse::<f32>() {
                    live.gain_db = g;
                }
            }
            "ppm" => {
                if let Ok(p) = v.parse::<i32>() {
                    live.ppm = p;
                }
            }
            "injection_tg" => {
                if let Ok(tg) = v.parse::<u32>() {
                    live.injection_tg = tg;
                }
            }
            _ => {}
        }
    }
    let restarted = cfg.apply_live(live.clone());
    tracing::info!(
        "flowdmr-sidecar: dashboard set freq={} Hz gain={} ppm={} tg={} (decoder_restart={})",
        live.rx_freq_hz,
        live.gain_db,
        live.ppm,
        live.injection_tg,
        restarted
    );
}

fn status_json(cfg: &SharedConfig, status: &SharedStatus) -> String {
    let s = status.snapshot();
    let LiveSettings { rx_freq_hz, gain_db, ppm, injection_tg } = cfg.live();
    let src = s.current_source.map(|v| v.to_string()).unwrap_or_else(|| "null".into());
    let pid = s.decoder_pid.map(|v| v.to_string()).unwrap_or_else(|| "null".into());
    let err = match &s.last_error {
        Some(e) => format!("\"{}\"", json_escape(e)),
        None => "null".into(),
    };
    let peak_dbfs = if s.pcm_peak <= 0 {
        -99.0
    } else {
        20.0 * (s.pcm_peak as f32 / 32768.0).log10()
    };
    format!(
        "{{\"decoder_running\":{},\"decoder_pid\":{},\"decoder_restarts\":{},\
         \"pcm_frames\":{},\"active_call\":{},\"current_source\":{},\"calls_total\":{},\
         \"last_meta_line\":\"{}\",\"last_error\":{},\
         \"peak_dbfs\":{:.1},\"pcm_clip\":{},\
         \"freq_hz\":{},\"gain_db\":{},\"ppm\":{},\"injection_tg\":{}}}",
        s.decoder_running,
        pid,
        s.decoder_restarts,
        s.pcm_frames,
        s.active_call,
        src,
        s.calls_total,
        json_escape(&s.last_meta_line),
        err,
        peak_dbfs,
        s.pcm_clip,
        rx_freq_hz,
        gain_db,
        ppm,
        injection_tg,
    )
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' | '\r' | '\t' => out.push(' '),
            c if (c as u32) < 0x20 => {}
            c => out.push(c),
        }
    }
    out
}

const PAGE: &str = r##"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>FlowDMR — mini control</title>
<style>
 :root{--bg:#0e1116;--panel:#171c24;--line:#262e3a;--fg:#e6edf3;--mut:#8b97a7;--ok:#3fb950;--bad:#f85149;--acc:#2f81f7}
 *{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--fg);font:14px/1.5 system-ui,Segoe UI,Roboto,sans-serif}
 .wrap{max-width:720px;margin:0 auto;padding:24px}
 h1{font-size:20px;margin:0 0 2px}.sub{color:var(--mut);margin:0 0 20px;font-size:13px}
 .grid{display:grid;grid-template-columns:1fr 1fr;gap:16px}
 .card{background:var(--panel);border:1px solid var(--line);border-radius:10px;padding:16px}
 .card h2{font-size:13px;text-transform:uppercase;letter-spacing:.04em;color:var(--mut);margin:0 0 12px}
 label{display:block;font-size:12px;color:var(--mut);margin:10px 0 4px}
 input{width:100%;background:#0b0f14;border:1px solid var(--line);color:var(--fg);border-radius:7px;padding:9px 10px;font:inherit}
 button{margin-top:16px;width:100%;background:var(--acc);color:#fff;border:0;border-radius:7px;padding:11px;font:600 14px/1 inherit;cursor:pointer}
 button:hover{filter:brightness(1.08)}
 .row{display:flex;justify-content:space-between;padding:6px 0;border-bottom:1px solid var(--line)}
 .row:last-child{border-bottom:0}.k{color:var(--mut)}.v{font-variant-numeric:tabular-nums}
 .dot{display:inline-block;width:9px;height:9px;border-radius:50%;margin-right:7px;vertical-align:middle}
 .on{background:var(--ok)}.off{background:var(--bad)}
 .pill{display:inline-block;padding:2px 9px;border-radius:999px;font-size:12px;font-weight:600}
 .live{background:rgba(63,185,80,.15);color:var(--ok)}.idle{background:rgba(139,151,167,.15);color:var(--mut)}
 code{color:var(--mut);font-size:12px;word-break:break-all}
 .meter{height:14px;border-radius:7px;background:#0b0f14;border:1px solid var(--line);overflow:hidden;margin-top:4px}
 .meter>span{display:block;height:100%;width:0;transition:width .1s linear}
 .lvl{display:flex;justify-content:space-between;align-items:center;margin-top:10px}
 .clip{font-size:12px;font-weight:700;color:var(--mut)}.clip.hot{color:var(--bad)}
 .logcard{margin-top:16px}
 pre.log{margin:0;background:#0b0f14;border:1px solid var(--line);border-radius:8px;padding:10px;height:320px;overflow:auto;
   font:12px/1.45 ui-monospace,Menlo,Consolas,monospace;color:#c9d4e0;white-space:pre-wrap;word-break:break-word}
 @media(max-width:640px){.grid{grid-template-columns:1fr}}
</style></head><body><div class="wrap">
 <h1>FlowDMR</h1><p class="sub">DMR → TETRA local injector — control panel</p>
 <div class="grid">
  <div class="card"><h2>Receiver</h2>
   <form id="f">
    <label>RX frequency (MHz)</label><input id="freq_mhz" type="number" step="0.0000125" placeholder="439.0000">
    <label>Tuner gain (dB, 0 = auto)</label><input id="gain_db" type="number" step="0.1" value="0">
    <label>Frequency correction (PPM)</label><input id="ppm" type="number" step="1" value="0">
    <label>Injection TalkGroup (TETRA GSSI)</label><input id="injection_tg" type="number" step="1" placeholder="5000">
    <button type="submit">Apply</button>
   </form>
   <div class="lvl"><span class="k">Audio level (into ACELP)</span><span class="clip" id="clip">—</span></div>
   <div class="meter"><span id="lvlbar"></span></div>
   <div class="lvl"><span class="k" id="dbfs">— dBFS</span><span class="k">aim for peaks below −3, never CLIP</span></div>
  </div>
  <div class="card"><h2>Status</h2>
   <div class="row"><span class="k">Decoder</span><span class="v"><span id="dec_dot" class="dot off"></span><span id="dec">—</span></span></div>
   <div class="row"><span class="k">Call</span><span class="v"><span id="call" class="pill idle">idle</span></span></div>
   <div class="row"><span class="k">Source ID</span><span class="v" id="src">—</span></div>
   <div class="row"><span class="k">Calls total</span><span class="v" id="calls">0</span></div>
   <div class="row"><span class="k">PCM frames</span><span class="v" id="pcm">0</span></div>
   <div class="row"><span class="k">Restarts</span><span class="v" id="restarts">0</span></div>
   <div class="row"><span class="k">Inject GSSI</span><span class="v" id="tg">—</span></div>
   <div style="margin-top:12px"><span class="k">Last call line</span><br><code id="meta">—</code></div>
   <div id="errwrap" style="margin-top:10px;display:none"><span class="k" style="color:var(--bad)">Error</span> <code id="err"></code></div>
  </div>
 </div>
 <div class="card logcard"><h2>Live decoder log (dsd-neo)</h2><pre class="log" id="log">waiting for decoder…</pre></div>
</div>
<script>
let dirty=false;
['freq_mhz','gain_db','ppm','injection_tg'].forEach(id=>document.getElementById(id).addEventListener('input',()=>dirty=true));
function meter(dbfs,clip){
 const pct=Math.max(0,Math.min(100,(dbfs+60)/60*100));            // -60..0 dBFS -> 0..100%
 const col = dbfs>=-1?'#f85149':(dbfs>=-6?'#d29922':'#3fb950');
 const bar=document.getElementById('lvlbar');bar.style.width=pct+'%';bar.style.background=col;
 document.getElementById('dbfs').textContent=(dbfs<=-99?'silence':dbfs.toFixed(1)+' dBFS');
 const c=document.getElementById('clip');
 if(clip>0){c.textContent='CLIP ×'+clip;c.className='clip hot';}else{c.textContent='no clip';c.className='clip';}
}
function render(s){
 document.getElementById('dec_dot').className='dot '+(s.decoder_running?'on':'off');
 document.getElementById('dec').textContent=s.decoder_running?('running (pid '+s.decoder_pid+')'):'stopped';
 const c=document.getElementById('call');c.textContent=s.active_call?'LIVE':'idle';c.className='pill '+(s.active_call?'live':'idle');
 document.getElementById('src').textContent=s.current_source??'—';
 document.getElementById('calls').textContent=s.calls_total;
 document.getElementById('pcm').textContent=s.pcm_frames;
 document.getElementById('restarts').textContent=s.decoder_restarts;
 document.getElementById('tg').textContent=s.injection_tg;
 document.getElementById('meta').textContent=s.last_meta_line||'—';
 const ew=document.getElementById('errwrap');if(s.last_error){ew.style.display='';document.getElementById('err').textContent=s.last_error;}else ew.style.display='none';
 meter(s.peak_dbfs,s.pcm_clip);
 if(!dirty){
  document.getElementById('freq_mhz').value=(s.freq_hz/1e6).toFixed(4);
  document.getElementById('gain_db').value=s.gain_db;
  document.getElementById('ppm').value=s.ppm;
  document.getElementById('injection_tg').value=s.injection_tg;
 }
}
async function poll(){try{const r=await fetch('/api/status');render(await r.json());}catch(e){}}
async function pollLog(){try{
 const t=await (await fetch('/api/log')).text();
 const el=document.getElementById('log');
 const atBottom=el.scrollTop+el.clientHeight>=el.scrollHeight-30;
 el.textContent=t||'(no decoder output yet)';
 if(atBottom)el.scrollTop=el.scrollHeight;
}catch(e){}}
document.getElementById('f').addEventListener('submit',async e=>{
 e.preventDefault();
 const b=new URLSearchParams();
 ['freq_mhz','gain_db','ppm','injection_tg'].forEach(id=>b.append(id,document.getElementById(id).value));
 const r=await fetch('/api/control',{method:'POST',headers:{'Content-Type':'application/x-www-form-urlencoded'},body:b.toString()});
 dirty=false;render(await r.json());
});
poll();setInterval(poll,500);
pollLog();setInterval(pollLog,700);
</script>
</body></html>"##;
