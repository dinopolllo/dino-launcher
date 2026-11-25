/* Dino.perf — qué se apaga y cuándo.

   Todo lo que se mueve o escucha sin que nadie lo pida (la cabeza que sigue al cursor, el
   resplandor del fondo, el brillo de la ficha, el audio) se registra aquí con un par
   `pause` / `resume`. Este módulo es el único que decide cuándo corren:

     · la ventana pierde el foco o se oculta        → quieto
     · se está abriendo Minecraft (`launching`)     → quieto
     · hay una partida (`playing` / `stopping`)     → quieto, y además se congelan las
                                                       animaciones CSS (`data-quiet="game"`)

   Al volver todo a la normalidad se reanuda cada pieza. Una pieza nueva no necesita tocar
   el resto: se registra y ya. Por omisión el núcleo cierra esta ventana al jugar, así que
   esto cubre sobre todo el arranque del juego y quien tiene `close_ui_while_playing`
   apagado. */
(function(){
  "use strict";
  var D = (window.Dino = window.Dino || {});
  var root = document.documentElement;

  var parts = {};            /* nombre → { pause, resume, on } */
  var focused = true;
  var launching = false;
  var game = false;

  function mustRun(){ return focused && !launching && !game; }

  function apply(){
    var run = mustRun();
    if (run) root.removeAttribute("data-quiet");
    else root.setAttribute("data-quiet", game ? "game" : launching ? "launch" : "idle");
    Object.keys(parts).forEach(function(n){
      var p = parts[n];
      try {
        if (run && !p.on)  { p.on = true;  p.resume(); }
        if (!run && p.on)  { p.on = false; p.pause(); }
      } catch (e) { try { console.debug("[dino] perf " + n + ": " + e); } catch (_) {} }
    });
  }

  function readFocus(){
    focused = document.visibilityState !== "hidden" && document.hasFocus();
  }
  window.addEventListener("focus", function(){ focused = document.visibilityState !== "hidden"; apply(); });
  window.addEventListener("blur",  function(){ focused = false; apply(); });
  document.addEventListener("visibilitychange", function(){ readFocus(); apply(); });
  readFocus();

  D.perf = {
    /* `hooks.pause()` y `hooks.resume()` tienen que ser idempotentes y baratos. Si ya
       toca correr, se reanuda en el acto. */
    register: function(name, hooks){
      if (!hooks || typeof hooks.pause !== "function" || typeof hooks.resume !== "function") return;
      parts[name] = { pause: hooks.pause, resume: hooks.resume, on: false };
      apply();
    },
    unregister: function(name){
      var p = parts[name];
      if (p && p.on) { try { p.pause(); } catch (e) {} }
      delete parts[name];
    },
    /* Se está abriendo el juego: lo no esencial se queda quieto, la barra de progreso no. */
    setLaunching: function(v){ v = !!v; if (v !== launching) { launching = v; apply(); } },
    /* Hay una partida: además se congelan las animaciones CSS. */
    setGame: function(v){ v = !!v; if (v !== game) { game = v; if (v) launching = false; apply(); } },
    running: mustRun
  };
})();
