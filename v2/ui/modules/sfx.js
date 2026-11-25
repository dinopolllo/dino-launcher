/* Dino.sfx — sonidos de la interfaz.

   Por omisión **no suena nada**: el launcher base es silencioso. Los sonidos los trae el
   tema de un modpack (`theme.sounds`), y cada momento puede ser:
     · el nombre de un sonido de fábrica: soft, blip, chime, void (sintetizados, sin archivos)
     · un https://… a un archivo de audio (se baja una vez, con tope de tamaño y de tiempo)
     · nada: ese momento queda en silencio

   Momentos: click, hover, done (acabó una instalación o actualización), play (se pulsó
   Jugar).

   Control del jugador: interruptor general y volumen (en su perfil). El volumen del pack
   se multiplica por el del jugador, así que un pack nunca puede sonar más alto de lo que
   el jugador eligió. `Dino.perf` suspende el audio con la ventana sin foco o durante la
   partida. */
(function(){
  "use strict";
  var D = (window.Dino = window.Dino || {});

  var MAX_BYTES = 600 * 1024;
  var FETCH_MS = 4000;
  var GAIN = { click: 0.9, hover: 0.35, done: 1, play: 1 };   /* el hover, siempre más bajo */
  var HOVER_GAP_MS = 90;
  var INTERACTIVE = "button, .btn, [role=button], [role=tab], .packtile, .nav button, a[href], select, .switch";

  var enabled = true, volume = 0.6;
  var theme = null;               /* theme.sounds ya saneado, o null */
  var ctx = null, suspended = false;
  var cache = {};                 /* url → AudioBuffer | null (null: no se pudo, no se reintenta) */
  var lastHover = 0, lastEl = null;

  function context(){
    if (ctx) return ctx;
    var AC = window.AudioContext || window.webkitAudioContext;
    if (!AC) return null;
    try { ctx = new AC(); } catch (e) { ctx = null; }
    return ctx;
  }

  /* Un tono con envolvente: nunca arranca ni acaba en seco, que es lo que chasquea. */
  function tone(c, out, o){
    var t0 = c.currentTime + (o.at || 0);
    var osc = c.createOscillator(), g = c.createGain();
    osc.type = o.type || "sine";
    osc.frequency.setValueAtTime(o.f0, t0);
    if (o.f1) osc.frequency.exponentialRampToValueAtTime(o.f1, t0 + o.dur);
    g.gain.setValueAtTime(0.0001, t0);
    g.gain.exponentialRampToValueAtTime(o.peak || 0.5, t0 + 0.008);
    g.gain.exponentialRampToValueAtTime(0.0001, t0 + o.dur);
    osc.connect(g); g.connect(out);
    osc.start(t0); osc.stop(t0 + o.dur + 0.02);
  }
  var PRESETS = {
    soft:  function(c, o){ tone(c, o, { f0: 520, f1: 430, dur: 0.07, peak: 0.35 }); },
    blip:  function(c, o){ tone(c, o, { type: "square", f0: 880, dur: 0.045, peak: 0.16 }); },
    chime: function(c, o){
      tone(c, o, { f0: 784,  dur: 0.38, peak: 0.28 });
      tone(c, o, { f0: 1175, dur: 0.30, peak: 0.18, at: 0.05 });
    },
    void:  function(c, o){
      var lp = c.createBiquadFilter(); lp.type = "lowpass"; lp.frequency.value = 520;
      lp.connect(o);
      tone(c, lp, { type: "sawtooth", f0: 118, f1: 52, dur: 0.30, peak: 0.45 });
    }
  };

  function isUrl(s){ return /^https:\/\/[^\s"'<>\\]+$/.test(s); }

  function load(url){
    if (url in cache) return Promise.resolve(cache[url]);
    var c = context();
    if (!c || !window.fetch) return Promise.resolve(null);
    var ctl = window.AbortController ? new AbortController() : null;
    var timer = ctl ? setTimeout(function(){ ctl.abort(); }, FETCH_MS) : null;
    cache[url] = null;            /* mientras tanto, y si falla, no se reintenta */
    return fetch(url, ctl ? { signal: ctl.signal, referrerPolicy: "no-referrer" } : {}).then(function(r){
      if (!r.ok) throw new Error("http " + r.status);
      var len = Number(r.headers.get("content-length") || 0);
      if (len > MAX_BYTES) throw new Error("demasiado grande");
      return r.arrayBuffer();
    }).then(function(buf){
      if (buf.byteLength > MAX_BYTES) throw new Error("demasiado grande");
      return new Promise(function(ok, no){ c.decodeAudioData(buf, ok, no); });
    }).then(function(ab){ cache[url] = ab; return ab; })
      .catch(function(){ return null; })
      .then(function(r){ if (timer) clearTimeout(timer); return r; });
  }

  function play(event){
    if (!enabled || suspended || !theme) return;
    var what = theme[event];
    if (!what) return;
    var c = context();
    if (!c) return;
    if (c.state === "suspended") { try { c.resume(); } catch (e) {} }
    var master = c.createGain();
    var pack = (typeof theme.volume === "number") ? Math.max(0, Math.min(1, theme.volume)) : 1;
    master.gain.value = volume * pack * (GAIN[event] || 1);
    master.connect(c.destination);
    if (PRESETS[what]) { PRESETS[what](c, master); return; }
    if (!isUrl(what)) return;
    load(what).then(function(buf){
      if (!buf || !enabled || suspended) return;
      var src = c.createBufferSource();
      src.buffer = buf; src.connect(master); src.start();
    });
  }

  /* Un clic y un paso por encima de lo pulsable, sin cablear cada botón. El hover no
     repite mientras el puntero sigue sobre el mismo elemento, ni más de una vez cada 90 ms. */
  function onClick(e){
    var el = e.target && e.target.closest && e.target.closest(INTERACTIVE);
    if (el && !el.disabled) play("click");
  }
  function onOver(e){
    var el = e.target && e.target.closest && e.target.closest(INTERACTIVE);
    if (!el || el === lastEl || el.disabled) return;
    lastEl = el;
    var now = Date.now();
    if (now - lastHover < HOVER_GAP_MS) return;
    lastHover = now;
    play("hover");
  }
  function onOut(e){ if (!e.relatedTarget) lastEl = null; }

  var bound = false;
  function bind(){
    if (bound) return;
    bound = true;
    document.addEventListener("click", onClick, true);
    document.addEventListener("pointerover", onOver, { passive: true });
    document.addEventListener("pointerout", onOut, { passive: true });
  }
  function unbind(){
    if (!bound) return;
    bound = false;
    document.removeEventListener("click", onClick, true);
    document.removeEventListener("pointerover", onOver);
    document.removeEventListener("pointerout", onOut);
    lastEl = null;
  }

  D.perf.register("sfx", {
    pause: function(){
      suspended = true; unbind();
      if (ctx && ctx.state === "running") { try { ctx.suspend(); } catch (e) {} }
    },
    resume: function(){
      suspended = false;
      if (enabled && theme) bind();
      if (ctx && ctx.state === "suspended") { try { ctx.resume(); } catch (e) {} }
    }
  });

  D.sfx = {
    /* Las preferencias del jugador: interruptor y volumen de 0 a 1. */
    configure: function(o){
      enabled = o.enabled !== false;
      if (typeof o.volume === "number" && isFinite(o.volume)) volume = Math.max(0, Math.min(1, o.volume));
      if (enabled && theme && !suspended) bind(); else unbind();
    },
    /* `sounds` es `theme.sounds` (o null para el tema base, que es silencioso). */
    setTheme: function(sounds){
      theme = sounds && (sounds.click || sounds.hover || sounds.done || sounds.play) ? sounds : null;
      if (enabled && theme && !suspended) bind(); else unbind();
      /* Los de URL se bajan ahora, para que el primer clic ya suene. */
      if (theme && enabled) ["click", "hover", "done", "play"].forEach(function(k){
        if (theme[k] && isUrl(theme[k]) && context()) load(theme[k]);
      });
    },
    play: play,
    /* Solo para la vista previa del editor: suena aunque el tema aún no esté aplicado. */
    preview: function(what, vol){
      var old = theme, was = enabled;
      theme = { x: what, volume: vol };
      enabled = true;
      var c = context();
      if (c) {
        if (c.state === "suspended") { try { c.resume(); } catch (e) {} }
        play("x");
      }
      theme = old; enabled = was;
    }
  };
})();
