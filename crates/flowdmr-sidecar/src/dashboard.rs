//! Tiny self-contained control dashboard (no web framework).
//!
//! Routes:
//!   GET  /              -> HTML control panel
//!   GET  /api/status    -> JSON status + current live settings
//!   GET  /api/log       -> recent decoder console lines (plain text)
//!   GET  /api/recordings-> JSON list of saved .wav files
//!   GET  /rec/<file>    -> download a recording
//!   POST /api/control   -> urlencoded: freq_mhz, gain_db, ppm, injection_tg

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
        ("GET", "/api/recordings") => {
            respond(&mut stream, "200 OK", "application/json", &recordings_json(cfg))
        }
        ("GET", p) if p.starts_with("/rec/") => serve_recording(&mut stream, cfg, &p[5..]),
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

fn respond_bytes(stream: &mut TcpStream, content_type: &str, extra: &str, body: &[u8]) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)
}

fn recordings_json(cfg: &SharedConfig) -> String {
    let dir = std::path::Path::new(&cfg.static_cfg.record_dir);
    let items: Vec<String> = crate::recorder::list_recordings(dir, 200)
        .into_iter()
        .map(|(name, size)| format!("{{\"name\":\"{}\",\"size\":{}}}", json_escape(&name), size))
        .collect();
    format!("[{}]", items.join(","))
}

/// Serve a recording for download. The name is sanitised to its basename so it
/// can't escape the recordings directory.
fn serve_recording(stream: &mut TcpStream, cfg: &SharedConfig, name: &str) -> std::io::Result<()> {
    if name.is_empty() || name.contains('/') || name.contains("..") || !name.ends_with(".wav") {
        return respond(stream, "400 Bad Request", "text/plain", "bad name");
    }
    let path = std::path::Path::new(&cfg.static_cfg.record_dir).join(name);
    match std::fs::read(&path) {
        Ok(bytes) => {
            let cd = format!("Content-Disposition: attachment; filename=\"{name}\"\r\n");
            respond_bytes(stream, "audio/wav", &cd, &bytes)
        }
        Err(_) => respond(stream, "404 Not Found", "text/plain", "not found"),
    }
}

fn apply_control(cfg: &SharedConfig, body: &str) {
    let mut live = cfg.live();
    for pair in body.split('&') {
        let Some((k, v)) = pair.split_once('=') else { continue };
        let v = v.trim();
        match k {
            "freq_mhz" => {
                if let Ok(mhz) = v.parse::<f64>() {
                    if mhz > 0.0 {
                        live.rx_freq_hz = (mhz * 1_000_000.0).round() as u64;
                    }
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
        "flowdmr-sidecar: dashboard set freq={} Hz gain={} ppm={} tg={} -> decoder {}",
        live.rx_freq_hz,
        live.gain_db,
        live.ppm,
        live.injection_tg,
        if restarted { "RETUNING" } else { "unchanged (no RF change)" }
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
<title>FlowDMR</title>
<style>
 :root{
  --bg:#0a0d13;--panel:#131a26;--panel2:#0e141e;--line:#23304a;--fg:#eaf1fb;--mut:#8595ad;
  --acc:#3b82f6;--acc2:#22d3ee;--ok:#22c55e;--warn:#f59e0b;--bad:#ef4444;
  --shadow:0 8px 30px rgba(0,0,0,.35);
 }
 *{box-sizing:border-box}
 body{margin:0;background:radial-gradient(1200px 600px at 70% -10%,#15233b 0,var(--bg) 55%) fixed;
   color:var(--fg);font:14px/1.5 system-ui,-apple-system,Segoe UI,Roboto,sans-serif;-webkit-font-smoothing:antialiased}
 a{color:var(--acc2)}
 .wrap{max-width:1080px;margin:0 auto;padding:22px 18px 60px}
 header{display:flex;align-items:center;gap:14px;margin-bottom:22px;flex-wrap:wrap}
 .logo{width:38px;height:38px;border-radius:10px;background:linear-gradient(135deg,var(--acc),var(--acc2));
   display:grid;place-items:center;box-shadow:var(--shadow);flex:none}
 .logo svg{width:22px;height:22px;color:#06121f}
 h1{font-size:19px;margin:0;letter-spacing:.2px}
 .tag{color:var(--mut);font-size:12.5px;margin-top:1px}
 .chips{margin-left:auto;display:flex;gap:8px;flex-wrap:wrap}
 .chip{display:inline-flex;align-items:center;gap:7px;background:var(--panel);border:1px solid var(--line);
   border-radius:999px;padding:6px 12px;font-size:12.5px;font-weight:600;box-shadow:var(--shadow)}
 .dot{width:9px;height:9px;border-radius:50%;background:var(--bad);flex:none}
 .dot.on{background:var(--ok);box-shadow:0 0 0 0 rgba(34,197,94,.6);animation:none}
 .chip.live .dot{background:var(--ok);animation:pulse 1.1s infinite}
 @keyframes pulse{0%{box-shadow:0 0 0 0 rgba(34,197,94,.55)}70%{box-shadow:0 0 0 8px rgba(34,197,94,0)}100%{box-shadow:0 0 0 0 rgba(34,197,94,0)}}
 .grid{display:grid;grid-template-columns:1.05fr .95fr;gap:16px}
 .card{background:linear-gradient(180deg,var(--panel),var(--panel2));border:1px solid var(--line);
   border-radius:14px;padding:18px;box-shadow:var(--shadow)}
 .card h2{font-size:12px;text-transform:uppercase;letter-spacing:.08em;color:var(--mut);margin:0 0 14px;display:flex;align-items:center;gap:9px}
 .full{grid-column:1/-1}
 .frow{display:grid;grid-template-columns:1fr 1fr;gap:12px}
 label{display:block;font-size:11.5px;color:var(--mut);margin:0 0 5px;font-weight:600}
 input{width:100%;background:#0a1018;border:1px solid var(--line);color:var(--fg);border-radius:9px;
   padding:10px 11px;font:inherit;outline:none;transition:border .15s,box-shadow .15s}
 input:focus{border-color:var(--acc);box-shadow:0 0 0 3px rgba(59,130,246,.18)}
 button{cursor:pointer;border:0;font:inherit}
 .apply{margin-top:14px;width:100%;background:linear-gradient(135deg,var(--acc),#2563eb);color:#fff;
   border-radius:10px;padding:12px;font-weight:700;font-size:14px;box-shadow:0 6px 18px rgba(37,99,235,.35);transition:filter .15s,transform .05s}
 .apply:hover{filter:brightness(1.07)}.apply:active{transform:translateY(1px)}
 .tuned{display:flex;justify-content:space-between;align-items:center;margin-top:14px;padding-top:12px;border-top:1px solid var(--line);
   font-size:13px}.tuned b{font-variant-numeric:tabular-nums}
 .lab{font-size:11.5px;color:var(--mut);font-weight:600}
 /* meter */
 .meterwrap{margin-top:16px}
 .meterhead{display:flex;justify-content:space-between;align-items:baseline;margin-bottom:6px}
 .meterhead .v{font-variant-numeric:tabular-nums;font-weight:700;font-size:15px}
 .meter{position:relative;height:16px;border-radius:8px;background:#0a1018;border:1px solid var(--line);overflow:hidden}
 .meter>span{position:absolute;left:0;top:0;bottom:0;width:0;border-radius:8px 0 0 8px;transition:width .12s linear,background .2s}
 .ticks{display:flex;justify-content:space-between;color:var(--mut);font-size:10px;margin-top:3px;font-variant-numeric:tabular-nums}
 .clipbadge{font-size:11.5px;font-weight:700;color:var(--mut)}.clipbadge.hot{color:var(--bad)}
 /* stats */
 .stats{display:grid;grid-template-columns:1fr 1fr;gap:1px;background:var(--line);border:1px solid var(--line);border-radius:10px;overflow:hidden}
 .stat{background:var(--panel);padding:11px 13px}
 .stat .k{font-size:11px;color:var(--mut);text-transform:uppercase;letter-spacing:.04em}
 .stat .v{font-size:17px;font-weight:700;font-variant-numeric:tabular-nums;margin-top:2px}
 .pill{display:inline-block;padding:2px 10px;border-radius:999px;font-size:12px;font-weight:700}
 .pill.live{background:rgba(34,197,94,.16);color:var(--ok)}.pill.idle{background:rgba(133,149,173,.16);color:var(--mut)}
 .meta{margin-top:13px}.meta code{display:block;margin-top:5px;background:#0a1018;border:1px solid var(--line);border-radius:8px;
   padding:8px 10px;font:12px/1.4 ui-monospace,Menlo,Consolas,monospace;color:#aebdd4;word-break:break-all}
 .warn{background:rgba(239,68,68,.1);border:1px solid rgba(239,68,68,.4);color:#fca5a5;border-radius:10px;
   padding:9px 11px;margin-top:12px;font-size:12.5px;display:none}
 .hint{color:var(--mut);font-size:12px;margin-top:11px;line-height:1.5}
 /* log */
 .loghead{display:flex;align-items:center;gap:10px;margin-bottom:12px}
 .loghead h2{margin:0}
 .qual{margin-left:auto;font-size:12px;font-weight:700;padding:3px 10px;border-radius:999px;background:rgba(133,149,173,.14)}
 .btn-sm{background:#1c2840;color:var(--fg);border:1px solid var(--line);border-radius:8px;padding:6px 13px;font-size:12px;font-weight:600;transition:background .15s}
 .btn-sm:hover{background:#243352}
 pre.log{margin:0;background:#070b11;border:1px solid var(--line);border-radius:10px;padding:12px;height:340px;overflow:auto;
   font:12px/1.5 ui-monospace,Menlo,Consolas,monospace;color:#c9d6e8;white-space:pre-wrap;word-break:break-word}
 pre.log::-webkit-scrollbar{width:10px}pre.log::-webkit-scrollbar-thumb{background:#26344f;border-radius:6px}
 /* recordings */
 #player{width:100%;margin-bottom:12px;border-radius:8px}
 .recs{display:flex;flex-direction:column;max-height:300px;overflow:auto}
 .rec{display:flex;justify-content:space-between;align-items:center;gap:10px;padding:9px 6px;border-bottom:1px solid var(--line)}
 .rec:last-child{border-bottom:0}
 .rec .nm{font:12px/1.3 ui-monospace,Menlo,Consolas,monospace;color:#cdd9ec;word-break:break-all}
 .rec .right{display:flex;gap:10px;align-items:center;white-space:nowrap}
 .rec .sz{color:var(--mut);font-size:11px}
 .rec a,.rec button.play{color:var(--acc2);background:none;border:0;cursor:pointer;font-size:16px;text-decoration:none;padding:0 2px}
 /* toast */
 #toast{position:fixed;left:50%;bottom:26px;transform:translateX(-50%) translateY(20px);opacity:0;pointer-events:none;
   background:#0f1826;border:1px solid var(--line);color:var(--fg);padding:11px 18px;border-radius:12px;
   box-shadow:var(--shadow);font-weight:600;font-size:13px;transition:opacity .2s,transform .2s;z-index:9}
 #toast.show{opacity:1;transform:translateX(-50%) translateY(0)}
 @media(max-width:760px){.grid{grid-template-columns:1fr}.chips{width:100%;margin-left:0}}
</style></head><body><div class="wrap">

 <header>
  <div class="logo"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M4 12a8 8 0 0 1 8-8"/><path d="M7 12a5 5 0 0 1 5-5"/><circle cx="12" cy="12" r="2" fill="currentColor" stroke="none"/><path d="M12 14v6"/></svg></div>
  <div><h1>FlowDMR</h1><div class="tag">DMR → TETRA local injector</div></div>
  <div class="chips">
   <span class="chip" id="decchip"><span class="dot" id="dec_dot"></span><span id="dec">decoder</span></span>
   <span class="chip" id="callchip"><span class="dot"></span><span id="callstate">idle</span></span>
  </div>
 </header>

 <div class="grid">
  <div class="card">
   <h2>Receiver</h2>
   <form id="f">
    <div class="frow">
     <div><label>RX frequency (MHz)</label><input id="freq_mhz" type="number" step="0.000001" placeholder="439.4000"></div>
     <div><label>Tuner gain (dB · 0 = auto)</label><input id="gain_db" type="number" step="0.1" placeholder="20"></div>
     <div><label>PPM correction</label><input id="ppm" type="number" step="1" placeholder="0"></div>
     <div><label>Injection TalkGroup (GSSI)</label><input id="injection_tg" type="number" step="1" placeholder="9"></div>
    </div>
    <button class="apply" type="submit">Apply &amp; retune</button>
   </form>
   <div class="tuned"><span class="lab">Currently tuned</span><b id="tunedfreq">—</b></div>

   <div class="meterwrap">
    <div class="meterhead"><span class="lab">Audio into ACELP</span><span class="v" id="dbfs">—</span></div>
    <div class="meter"><span id="lvlbar"></span></div>
    <div class="ticks"><span>-60</span><span>-30</span><span>-18</span><span>-8</span><span class="clipbadge" id="clip">0 dB</span></div>
    <div class="warn" id="rfwarn">⚠ RF overload (CLIP) in the log — lower the Tuner gain until it clears.</div>
    <div class="hint">Aim the bar at the <b>−8</b> mark (green), never CLIP. Then watch the log: fewer red errors = cleaner voice.</div>
   </div>
  </div>

  <div class="card">
   <h2>Status</h2>
   <div class="stats">
    <div class="stat"><div class="k">Call</div><div class="v"><span class="pill idle" id="call">idle</span></div></div>
    <div class="stat"><div class="k">Source ID</div><div class="v" id="src">—</div></div>
    <div class="stat"><div class="k">Calls total</div><div class="v" id="calls">0</div></div>
    <div class="stat"><div class="k">PCM frames</div><div class="v" id="pcm">0</div></div>
    <div class="stat"><div class="k">Inject GSSI</div><div class="v" id="tg">—</div></div>
    <div class="stat"><div class="k">Decoder restarts</div><div class="v" id="restarts">0</div></div>
   </div>
   <div class="meta"><span class="lab">Last call line</span><code id="metaline">—</code></div>
   <div class="warn" id="errwrap" style="color:#fca5a5">⚠ <span id="err"></span></div>
  </div>

  <div class="card full">
   <div class="loghead"><h2>Live decoder log</h2><span class="qual" id="qual">—</span><button class="btn-sm" id="copylog" type="button">Copy</button></div>
   <pre class="log" id="log">waiting for decoder…</pre>
  </div>

  <div class="card full">
   <h2>Recordings</h2>
   <audio id="player" controls preload="none"></audio>
   <div class="recs" id="recs">—</div>
  </div>
 </div>
</div>
<div id="toast"></div>
<script>
let dirty=false;
const $=id=>document.getElementById(id);
['freq_mhz','gain_db','ppm','injection_tg'].forEach(id=>$(id).addEventListener('input',()=>dirty=true));

function toast(msg){const t=$('toast');t.textContent=msg;t.classList.add('show');clearTimeout(t._t);t._t=setTimeout(()=>t.classList.remove('show'),2600);}
function copyText(t){
 if(navigator.clipboard&&window.isSecureContext){return navigator.clipboard.writeText(t);}
 return new Promise(res=>{const ta=document.createElement('textarea');ta.value=t;ta.style.cssText='position:fixed;top:0;opacity:0';
  document.body.appendChild(ta);ta.focus();ta.select();try{document.execCommand('copy');}catch(e){}document.body.removeChild(ta);res();});
}

function meter(dbfs,clip){
 const pct=Math.max(0,Math.min(100,(dbfs+60)/60*100));
 const col=dbfs>=-1?'#ef4444':(dbfs>=-6?'#f59e0b':'#22c55e');
 const bar=$('lvlbar');bar.style.width=pct+'%';bar.style.background=col;
 $('dbfs').textContent=(dbfs<=-99?'silence':dbfs.toFixed(1)+' dBFS');
 const c=$('clip');if(clip>0){c.textContent='CLIP×'+clip;c.className='clipbadge hot';}else{c.textContent='0 dB';c.className='clipbadge';}
}
function render(s){
 $('dec_dot').className='dot'+(s.decoder_running?' on':'');
 $('dec').textContent=s.decoder_running?('running · '+s.decoder_pid):'stopped';
 const cc=$('callchip');cc.className='chip'+(s.active_call?' live':'');
 $('callstate').textContent=s.active_call?'CALL LIVE':'idle';
 const c=$('call');c.textContent=s.active_call?'LIVE':'idle';c.className='pill '+(s.active_call?'live':'idle');
 $('src').textContent=s.current_source??'—';
 $('calls').textContent=s.calls_total;$('pcm').textContent=s.pcm_frames;$('restarts').textContent=s.decoder_restarts;
 $('tg').textContent=s.injection_tg;
 $('tunedfreq').textContent=(s.freq_hz/1e6).toFixed(4)+' MHz · '+(s.gain_db==0?'auto':s.gain_db+' dB');
 $('metaline').textContent=s.last_meta_line||'—';
 const ew=$('errwrap');if(s.last_error){ew.style.display='block';$('err').textContent=s.last_error;}else ew.style.display='none';
 meter(s.peak_dbfs,s.pcm_clip);
 if(!dirty){$('freq_mhz').value=(s.freq_hz/1e6).toFixed(4);$('gain_db').value=s.gain_db;$('ppm').value=s.ppm;$('injection_tg').value=s.injection_tg;}
}
async function poll(){try{render(await (await fetch('/api/status')).json());}catch(e){}}

let lastLog='';
function esc(s){return s.replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;');}
const ANSI={31:'#ef4444',32:'#22c55e',33:'#f59e0b',34:'#3b82f6',35:'#c678dd',36:'#22d3ee'};
function ansi(t){t=esc(t);let open=false;
 t=t.replace(/\x1b\[(\d+)m/g,(_,c)=>{c=+c;
  if(c===0){const r=open?'</span>':'';open=false;return r;}
  if(ANSI[c]){const r=(open?'</span>':'')+'<span style="color:'+ANSI[c]+'">';open=true;return r;}return '';});
 return open?t+'</span>':t;}
function quality(raw){const lines=raw.split('\n').slice(-60).filter(l=>l.trim());
 if(!lines.length)return['—','#8595ad'];
 const err=lines.filter(l=>/ERR|Sync Err|no sync/i.test(l)).length;
 const p=Math.round(100*(1-err/lines.length));
 return['decode '+p+'%',p>=80?'#22c55e':(p>=50?'#f59e0b':'#ef4444')];}
async function pollLog(){try{
 const t=await (await fetch('/api/log')).text();lastLog=t;
 const el=$('log');const atBottom=el.scrollTop+el.clientHeight>=el.scrollHeight-30;
 el.innerHTML=ansi(t)||'(no decoder output yet)';if(atBottom)el.scrollTop=el.scrollHeight;
 const[ql,qc]=quality(t);const q=$('qual');q.textContent=ql;q.style.color=qc;
 $('rfwarn').style.display=/RF Level CLIP/i.test(t)?'block':'none';
}catch(e){}}
$('copylog').onclick=()=>{copyText(lastLog.replace(/\x1b\[\d+m/g,'')).then(()=>{const b=$('copylog');b.textContent='Copied!';setTimeout(()=>b.textContent='Copy',1200);});};

async function pollRecs(){try{
 const r=await (await fetch('/api/recordings')).json();const el=$('recs');
 if(!r.length){el.textContent='no recordings yet';return;}
 el.innerHTML=r.map(x=>{const u='/rec/'+encodeURIComponent(x.name);
  return '<div class="rec"><span class="nm">'+x.name+'</span><span class="right"><span class="sz">'
   +(x.size/1024).toFixed(0)+' KB</span><button class="play" title="play" data-u="'+u+'">▶</button><a href="'+u+'" download title="download">⬇</a></span></div>';}).join('');
 el.querySelectorAll('button.play').forEach(b=>b.onclick=()=>{const p=$('player');p.src=b.dataset.u;p.play();});
}catch(e){}}

$('f').addEventListener('submit',async e=>{
 e.preventDefault();
 const b=new URLSearchParams();
 ['freq_mhz','gain_db','ppm','injection_tg'].forEach(id=>b.append(id,$(id).value));
 const s=await (await fetch('/api/control',{method:'POST',headers:{'Content-Type':'application/x-www-form-urlencoded'},body:b.toString()})).json();
 dirty=false;render(s);
 toast('Applied — retuning to '+(s.freq_hz/1e6).toFixed(4)+' MHz');
});
poll();setInterval(poll,500);
pollLog();setInterval(pollLog,800);
pollRecs();setInterval(pollRecs,3000);
</script>
</body></html>"##;
